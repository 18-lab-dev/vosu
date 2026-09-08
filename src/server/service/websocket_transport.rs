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
    (query, mutation): (Option<&str>, super::auth::MutationAuthorization),
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
    let connection = Arc::new(tokio::sync::Mutex::new(connection));
    let _relay_registration = relays.register(client_namespace, client.clone(), &connection);
    let (mut sink, mut stream) = socket.split();
    let (terminal_tx, mut terminal_rx) = tokio::sync::mpsc::channel::<SocketTerminal>(1);

    let writer_connection = Arc::clone(&connection);
    let writer = async move {
        loop {
            let (frame, terminal) = tokio::select! {
                biased;
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
            // All legacy sockets can read settings and adjust their own filters.
            // Only same-origin sockets may reach state-changing commands.
            if !mutation.0 && !matches!(text.split_once(':').map(|(command, _)| command),
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

const V2_SUBSCRIPTION_HANDSHAKE_TIMEOUT: std::time::Duration =
    std::time::Duration::from_secs(5);
const V2_SUBSCRIPTION_TERMINAL_DRAIN_TIMEOUT: std::time::Duration =
    std::time::Duration::from_secs(1);

async fn drain_v2_subscription_writer(writer: Pin<&mut impl Future<Output = ()>>) {
    let _ = tokio::time::timeout(V2_SUBSCRIPTION_TERMINAL_DRAIN_TIMEOUT, writer).await;
}

async fn bridge_v2_subscription_socket(socket: WebSocket, hub: V2SubscriptionHub) {
    let deadline = tokio::time::Instant::now() + V2_SUBSCRIPTION_HANDSHAKE_TIMEOUT;
    let (connection, mut outbound) = hub.connect();
    let connection = Arc::new(connection);
    let (mut sink, mut stream) = socket.split();
    let (peer_closed_tx, mut peer_closed_rx) = tokio::sync::mpsc::channel::<()>(1);
    let (terminal_sent_tx, mut terminal_sent_rx) = tokio::sync::oneshot::channel::<()>();

    let writer_connection = Arc::clone(&connection);
    let mut terminal_sent_tx = Some(terminal_sent_tx);
    let writer = async move {
        loop {
            let frame = tokio::select! {
                biased;
                peer_closed = peer_closed_rx.recv() => {
                    if peer_closed.is_some() {
                        let _ = sink.flush().await;
                    }
                    break;
                }
                frame = outbound.recv() => frame,
            };
            let Some(frame) = frame else { break };
            let data_frame = matches!(frame, V2SubscriptionOutbound::Text(_));
            let terminal = matches!(frame, V2SubscriptionOutbound::Close { .. });
            let message = match frame {
                V2SubscriptionOutbound::Text(bytes) | V2SubscriptionOutbound::Error(bytes) => {
                    match Utf8Bytes::try_from(bytes) {
                        Ok(text) => Message::Text(text),
                        Err(_) => {
                            if sink
                                .send(Message::Close(Some(CloseFrame {
                                    code: 1011,
                                    reason: Utf8Bytes::from_static(""),
                                })))
                                .await
                                .is_ok()
                            {
                                let _ = sink.flush().await;
                                if let Some(terminal_sent_tx) = terminal_sent_tx.take() {
                                    let _ = terminal_sent_tx.send(());
                                }
                                let _ = tokio::time::timeout(
                                    V2_SUBSCRIPTION_TERMINAL_DRAIN_TIMEOUT,
                                    peer_closed_rx.recv(),
                                )
                                .await;
                            }
                            break;
                        }
                    }
                }
                V2SubscriptionOutbound::Pong(bytes) => Message::Pong(bytes),
                V2SubscriptionOutbound::Close { code } => {
                    Message::Close(Some(CloseFrame {
                        code,
                        reason: Utf8Bytes::from_static(""),
                    }))
                }
            };
            if sink.send(message).await.is_err() {
                break;
            }
            if data_frame {
                writer_connection.write_completed();
            }
            if terminal {
                let _ = sink.flush().await;
                if let Some(terminal_sent_tx) = terminal_sent_tx.take() {
                    let _ = terminal_sent_tx.send(());
                }
                let _ = tokio::time::timeout(
                    V2_SUBSCRIPTION_TERMINAL_DRAIN_TIMEOUT,
                    peer_closed_rx.recv(),
                )
                .await;
                break;
            }
        }
    };

    let reader_connection = Arc::clone(&connection);
    let reader = async move {
        let handshake = async {
            loop {
                let Some(frame) = stream.next().await else {
                    return Err(true);
                };
                match frame {
                    Ok(Message::Text(text)) => {
                        let pointers = match parse_subscription_request(text.as_bytes()) {
                            Ok(pointers) => pointers,
                            Err(SubscriptionRequestError::InvalidRequest) => {
                                reader_connection.invalid_request();
                                return Err(false);
                            }
                            Err(SubscriptionRequestError::InvalidPointer) => {
                                reader_connection.invalid_pointer();
                                return Err(false);
                            }
                            Err(SubscriptionRequestError::AlreadySubscribed) => unreachable!(),
                        };
                        match reader_connection.subscribe(pointers) {
                            Ok(()) => return Ok(()),
                            Err(SubscriptionRequestError::InvalidRequest) => {
                                reader_connection.invalid_request();
                                return Err(false);
                            }
                            Err(SubscriptionRequestError::InvalidPointer) => {
                                reader_connection.invalid_pointer();
                                return Err(false);
                            }
                            Err(SubscriptionRequestError::AlreadySubscribed) => return Err(false),
                        }
                    }
                    Ok(Message::Binary(_)) => {
                        reader_connection.unsupported_data();
                        return Err(false);
                    }
                    Ok(Message::Ping(data)) => {
                        if !reader_connection.send_pong(data) {
                            reader_connection.close(1011);
                            return Err(false);
                        }
                    }
                    Ok(Message::Pong(_)) => {}
                    Ok(Message::Close(_)) => return Err(true),
                    Err(error) => {
                        let message = error.to_string().to_ascii_lowercase();
                        reader_connection.protocol_error(if message.contains("too long")
                            || message.contains("too large")
                            || message.contains("capacity")
                            || message.contains("message size")
                        {
                            1009
                        } else {
                            1002
                        });
                        return Err(false);
                    }
                }
            }
        };
        match tokio::time::timeout_at(deadline, handshake).await {
            Ok(Ok(())) => {}
            Ok(Err(true)) => return true,
            Ok(Err(false)) => {
                if tokio::time::timeout(
                    V2_SUBSCRIPTION_TERMINAL_DRAIN_TIMEOUT,
                    &mut terminal_sent_rx,
                )
                .await
                .is_err()
                {
                    return false;
                }
                while let Some(frame) = stream.next().await {
                    match frame {
                        Ok(Message::Close(_)) => return true,
                        Err(_) => return false,
                        _ => {}
                    }
                }
                return false;
            }
            Err(_) => {
                reader_connection.subscription_timed_out();
                if tokio::time::timeout(
                    V2_SUBSCRIPTION_TERMINAL_DRAIN_TIMEOUT,
                    &mut terminal_sent_rx,
                )
                .await
                .is_err()
                {
                    return false;
                }
                while let Some(frame) = stream.next().await {
                    match frame {
                        Ok(Message::Close(_)) => return true,
                        Err(_) => return false,
                        _ => {}
                    }
                }
                return false;
            }
        }

        loop {
            let Some(frame) = stream.next().await else {
                return true;
            };
            match frame {
                Ok(Message::Text(_)) => {
                    let _ = reader_connection.subscribe(Vec::new());
                    break;
                }
                Ok(Message::Binary(_)) => {
                    reader_connection.unsupported_data();
                    break;
                }
                Ok(Message::Ping(data)) => {
                    if !reader_connection.send_pong(data) {
                        reader_connection.close(1011);
                        break;
                    }
                }
                Ok(Message::Pong(_)) => {}
                Ok(Message::Close(_)) => return true,
                Err(error) => {
                    let message = error.to_string().to_ascii_lowercase();
                    reader_connection.protocol_error(if message.contains("too long")
                        || message.contains("too large")
                        || message.contains("capacity")
                        || message.contains("message size")
                    {
                        1009
                    } else {
                        1002
                    });
                    break;
                }
            }
        }
        if tokio::time::timeout(
            V2_SUBSCRIPTION_TERMINAL_DRAIN_TIMEOUT,
            &mut terminal_sent_rx,
        )
        .await
        .is_err()
        {
            return false;
        }
        while let Some(frame) = stream.next().await {
            if let Ok(Message::Close(_)) = frame {
                return true;
            }
        }
        false
    };
    tokio::pin!(writer);
    tokio::pin!(reader);

    tokio::select! {
        _ = &mut writer => {}
        peer_closed = &mut reader => {
            if peer_closed {
                let _ = peer_closed_tx.try_send(());
            }
            drain_v2_subscription_writer(writer.as_mut()).await;
        }
    }
    connection.cleanup();
}
