enum SocketTerminal {
    SendClose { code: u16, reason: &'static str },
    FlushPeerClose,
}

async fn bridge_socket(
    socket: WebSocket,
    hub: WebSocketHub,
    processor: Arc<CommandProcessor>,
    commands: bool,
    client_namespace: u64,
    relays: Arc<WebSocketRelaySink>,
    (query, mutation, key_grant): (Option<&str>, super::auth::MutationAuthorization, Option<super::api_keys::KeyGrant>),
) {
    let (connection, mut outbound) = if commands {
        hub.connect_commands()
    } else {
        hub.connect()
    };
    let Some(client_id) = namespaced_client_id(client_namespace, connection.id()) else {
        connection.cleanup();
        return;
    };
    let overlay = query.and_then(|query| decoded_query_value(query, "l"));
    let client = match CommandClient::from_query(client_id, overlay.as_deref()) {
        Ok(client) => client,
        Err(_) => {
            connection.cleanup();
            return;
        }
    };
    let control = connection.control_handle();
    let connection = Arc::new(tokio::sync::Mutex::new(connection));
    let _relay_registration = relays.register(client_namespace, client.clone(), control);
    let (mut sink, mut stream) = socket.split();
    let (terminal_tx, mut terminal_rx) = tokio::sync::mpsc::channel::<SocketTerminal>(1);

    let revocation = key_grant.as_ref().map(|grant| grant.revoked.clone());
    let writer_connection = Arc::clone(&connection);
    let writer = async move {
        loop {
            let (frame, terminal) = tokio::select! {
                biased;
                _ = async {
                    match &revocation {
                        Some(token) => token.cancelled().await,
                        None => std::future::pending::<()>().await,
                    }
                } => (None, Some(SocketTerminal::SendClose {
                    code: 1008, reason: "API key revoked",
                })),
                terminal = terminal_rx.recv() => (None, terminal),
                frame = outbound.recv() => (frame, None),
            };
            if let Some(terminal) = terminal {
                match terminal {
                    SocketTerminal::SendClose { code, reason } => {
                        let _ = sink
                            .send(Message::Close(Some(CloseFrame {
                                code,
                                reason: Utf8Bytes::from(reason),
                            })))
                            .await;
                    }
                    SocketTerminal::FlushPeerClose => {}
                }
                let _ = sink.flush().await;
                break;
            }

            let Some(frame) = frame else { break };
            let state_frame = matches!(frame, Outbound::Text(_));
            let terminal_frame = matches!(frame, Outbound::Close { .. });
            let message = match frame {
                Outbound::Text(bytes) | Outbound::Reply(bytes) => {
                    let Ok(text) = Utf8Bytes::try_from(bytes) else {
                        let _ = sink
                            .send(Message::Close(Some(CloseFrame {
                                code: 1011,
                                reason: Utf8Bytes::from_static(""),
                            })))
                            .await;
                        let _ = sink.flush().await;
                        break;
                    };
                    Message::Text(text)
                }
                Outbound::Binary(bytes) => Message::Binary(bytes),
                Outbound::Pong(bytes) => Message::Pong(bytes),
                Outbound::Close { code, reason } => Message::Close(Some(CloseFrame {
                    code,
                    reason: Utf8Bytes::from(reason),
                })),
            };
            if sink.send(message).await.is_err() {
                break;
            }
            if state_frame {
                writer_connection.lock().await.write_completed();
            }
            if terminal_frame {
                let _ = sink.flush().await;
                break;
            }
        }
    };

    let reader_connection = Arc::clone(&connection);
    let reader = async move {
        while let Some(frame) = stream.next().await {
            let inbound = match frame {
                Ok(Message::Text(text)) => Inbound::Text {
                    data: Bytes::copy_from_slice(text.as_bytes()),
                    finished: true,
                },
                Ok(Message::Binary(data)) => Inbound::Binary {
                    data,
                    finished: true,
                },
                Ok(Message::Ping(data)) => Inbound::Ping(data),
                Ok(Message::Pong(data)) => Inbound::Pong(data),
                Ok(Message::Close(_)) => return Some(SocketTerminal::FlushPeerClose),
                Err(error) => {
                    let message = error.to_string().to_ascii_lowercase();
                    return Some(SocketTerminal::SendClose {
                        code: if message.contains("too long")
                            || message.contains("too large")
                            || message.contains("capacity")
                            || message.contains("message size")
                        {
                            1009
                        } else {
                            1002
                        },
                        reason: if message.contains("too long")
                            || message.contains("too large")
                            || message.contains("capacity")
                            || message.contains("message size")
                        {
                            "Message too big"
                        } else {
                            "Protocol error"
                        },
                    });
                }
            };
            let completed = reader_connection.lock().await.receive(inbound).await;
            let Some(data) = completed else { continue };
            let Ok(text) = std::str::from_utf8(&data) else {
                continue;
            };
            // streamCompanion sends filter arrays without the legacy command
            // envelope; normalize only that endpoint's command shape.
            let normalized;
            let text = if client_namespace == 2
                && !text.contains(':')
                && text.trim_start().starts_with('[')
            {
                normalized = format!("applyFilters:{text}");
                normalized.as_str()
            } else {
                text
            };
            // All legacy sockets can read settings and adjust their own filters.
            // Same-origin sockets and authenticated local command brokers may write.
            let authorized = key_grant.as_ref().map_or(mutation.0, super::api_keys::KeyGrant::allowed);
            if key_grant.is_some() && !authorized {
                return Some(SocketTerminal::SendClose { code: 1008, reason: "API key revoked" });
            }
            if !authorized && !matches!(text.split_once(':').map(|(command, _)| command),
                Some("getSettings" | "updateSettings" | "applyFilters"))
            {
                return Some(SocketTerminal::SendClose { code: 1008, reason: "Same-origin required" });
            }
            match processor.process(&client, text) {
                CommandAction::Reply(bytes) => {
                    if !reader_connection.lock().await.send_text(bytes).await {
                        break;
                    }
                }
                CommandAction::ApplyFilters(filters) => {
                    reader_connection.lock().await.set_filters(filters);
                }
                CommandAction::NoAck => {}
            }
        }
        None
    };
    tokio::pin!(writer);
    tokio::pin!(reader);

    tokio::select! {
        _ = &mut writer => {}
        close = &mut reader => {
            if let Some(close) = close {
                let _ = terminal_tx.send(close).await;
                writer.await;
            }
        }
    }
    connection.lock().await.cleanup();
}

