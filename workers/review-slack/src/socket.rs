// Ported from Snitchgate's bounded Socket Mode reader; source history is preserved
// under services/snitchgate. No firewall or broker types are imported.
use anyhow::{Context, Result, anyhow, ensure};
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use std::time::Duration;
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::Message;

pub async fn forward_socket<S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin>(
    socket: &mut tokio_tungstenite::WebSocketStream<S>,
    events: mpsc::Sender<Value>,
) -> Result<()> {
    loop {
        let message = tokio::time::timeout(Duration::from_secs(90), socket.next())
            .await
            .map_err(|_| anyhow!("Slack socket idle timeout"))?
            .context("Slack socket closed")?
            .map_err(|_| anyhow!("Slack socket read failed"))?;
        match message {
            Message::Text(text) => {
                let envelope: Value = serde_json::from_str(&text)
                    .map_err(|_| anyhow!("invalid Slack socket JSON"))?;
                if let Some(id) = envelope["envelope_id"].as_str() {
                    ensure!(id.len() <= 128, "invalid Slack envelope ID");
                    // Ack in the reader, independently of classifier/storage/HTTP latency.
                    tokio::time::timeout(
                        Duration::from_secs(2),
                        socket.send(Message::Text(json!({"envelope_id":id}).to_string().into())),
                    )
                    .await
                    .map_err(|_| anyhow!("Slack acknowledgement timed out"))?
                    .map_err(|_| anyhow!("Slack acknowledgement failed"))?;
                    if events.try_send(envelope).is_err() {
                        eprintln!("Slack event queue full; no decision made. Retry the reply.");
                    }
                } else if envelope["type"] == "disconnect" {
                    return Ok(());
                }
            }
            Message::Ping(data) => {
                socket
                    .send(Message::Pong(data))
                    .await
                    .map_err(|_| anyhow!("Slack socket heartbeat failed"))?;
            }
            Message::Close(_) => return Ok(()),
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio_tungstenite::{WebSocketStream, tungstenite::protocol::Role};
    #[tokio::test]
    async fn acknowledges_even_when_the_event_consumer_is_full() {
        let (left, right) = tokio::io::duplex(16_384);
        let mut client = WebSocketStream::from_raw_socket(left, Role::Client, None).await;
        let mut server = WebSocketStream::from_raw_socket(right, Role::Server, None).await;
        let (send, _receive) = mpsc::channel(1);
        let reader = tokio::spawn(async move { forward_socket(&mut server, send).await });
        for id in ["first", "second"] {
            client
                .send(Message::Text(
                    json!({"envelope_id":id,"type":"events_api","payload":{}})
                        .to_string()
                        .into(),
                ))
                .await
                .unwrap();
            let reply = tokio::time::timeout(Duration::from_secs(1), client.next())
                .await
                .unwrap()
                .unwrap()
                .unwrap();
            let value: Value = serde_json::from_str(reply.to_text().unwrap()).unwrap();
            assert_eq!(value["envelope_id"], id);
        }
        client.close(None).await.unwrap();
        tokio::time::timeout(Duration::from_secs(1), reader)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
    }
}
