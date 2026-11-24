use super::subscriptions::ApiV1SubscriptionConnection;
use crate::server::subscription_wire::{Outbound, Receiver};
use axum::extract::ws::{CloseFrame, Message, Utf8Bytes, WebSocket};
use futures_util::{SinkExt, StreamExt};
use std::{sync::Arc, time::Duration};

pub(super) async fn bridge(
    socket: WebSocket,
    (connection, mut outbound): (ApiV1SubscriptionConnection, Receiver),
) {
    let connection = Arc::new(connection);
    let (mut sink, mut stream) = socket.split();
    let peer_closed = Arc::new(tokio::sync::Notify::new());
    let peer_close = Arc::clone(&peer_closed);
    let writer_connection = Arc::clone(&connection);
    let writer = async move {
        loop {
            let frame = tokio::select! {
                biased;
                _ = peer_close.notified() => { let _ = sink.flush().await; break; }
                frame = outbound.recv() => frame,
            };
            let Some(frame) = frame else {
                break;
            };
            let data = matches!(frame, Outbound::Text(_));
            let terminal = matches!(frame, Outbound::Close { .. });
            let message = match frame {
                Outbound::Text(bytes) | Outbound::Error(bytes) => {
                    match Utf8Bytes::try_from(bytes) {
                        Ok(text) => Message::Text(text),
                        Err(_) => break,
                    }
                }
                Outbound::Pong(bytes) => Message::Pong(bytes),
                Outbound::Close { code } => Message::Close(Some(CloseFrame {
                    code,
                    reason: Utf8Bytes::from_static(""),
                })),
            };
            if sink.send(message).await.is_err() {
                break;
            }
            if data {
                writer_connection.write_completed();
            }
            if terminal {
                let _ = sink.flush().await;
                let _ = tokio::time::timeout(Duration::from_secs(1), peer_close.notified()).await;
                break;
            }
        }
    };
    let reader_connection = Arc::clone(&connection);
    let reader = async move {
        while let Some(frame) = stream.next().await {
            match frame {
                Ok(Message::Text(text)) => reader_connection.text(text.as_bytes()),
                Ok(Message::Ping(bytes)) => reader_connection.ping(bytes),
                Ok(Message::Pong(_)) => {}
                Ok(Message::Binary(_)) => reader_connection.unsupported_data(),
                Ok(Message::Close(_)) => {
                    peer_closed.notify_one();
                    break;
                }
                Err(error) => {
                    let error = error.to_string().to_ascii_lowercase();
                    reader_connection.close(
                        if error.contains("capacity")
                            || error.contains("too long")
                            || error.contains("too large")
                            || error.contains("message size")
                        {
                            1009
                        } else {
                            1002
                        },
                    );
                    break;
                }
            }
        }
    };
    tokio::pin!(writer);
    tokio::pin!(reader);
    tokio::select! {
        _ = &mut writer => {},
        _ = &mut reader => { let _ = tokio::time::timeout(Duration::from_secs(1), &mut writer).await; },
    }
    connection.close(1000);
}
