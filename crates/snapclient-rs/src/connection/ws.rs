//! WebSocket connection to a snapserver.

use anyhow::{Context, Result};
use futures_util::{SinkExt, StreamExt};
use snapcast_proto::MessageType;
use snapcast_proto::message::base::BaseMessage;
use snapcast_proto::message::factory::{self, MessagePayload, TypedMessage};
use snapcast_proto::types::Timeval;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

type WsStream = WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>;

pub struct WsConnection {
    ws: Option<WsStream>,
    host: String,
    port: u16,
    next_id: u16,
}

impl WsConnection {
    pub fn new(host: &str, port: u16) -> Self {
        Self {
            ws: None,
            host: host.to_string(),
            port,
            next_id: 1,
        }
    }

    pub async fn connect(&mut self) -> Result<()> {
        let url = format!("ws://{}:{}/jsonrpc", self.host, self.port);
        let (ws, _) = tokio_tungstenite::connect_async(&url)
            .await
            .with_context(|| format!("WebSocket connect to {url}"))?;
        self.ws = Some(ws);
        self.next_id = 1;
        Ok(())
    }

    pub fn disconnect(&mut self) {
        self.ws = None;
    }

    pub async fn send(&mut self, msg_type: MessageType, payload: &MessagePayload) -> Result<()> {
        let ws = self.ws.as_mut().context("not connected")?;
        let mut base = BaseMessage {
            msg_type,
            id: 0,
            refers_to: 0,
            sent: Timeval::default(),
            received: Timeval::default(),
            size: 0,
        };
        let frame = factory::serialize(&mut base, payload)
            .map_err(|e| anyhow::anyhow!("serialize: {e}"))?;
        ws.send(Message::Binary(frame.into())).await?;
        Ok(())
    }

    pub async fn recv(&mut self) -> Result<TypedMessage> {
        let ws = self.ws.as_mut().context("not connected")?;
        loop {
            let msg = ws
                .next()
                .await
                .context("WebSocket stream ended")?
                .context("WebSocket error")?;
            match msg {
                Message::Binary(data) => {
                    if data.len() < BaseMessage::HEADER_SIZE {
                        continue;
                    }
                    let base = BaseMessage::read_from(&mut &data[..BaseMessage::HEADER_SIZE])
                        .map_err(|e| anyhow::anyhow!("parse header: {e}"))?;
                    let payload = &data[BaseMessage::HEADER_SIZE..];
                    return factory::deserialize(base, payload)
                        .map_err(|e| anyhow::anyhow!("deserialize: {e}"));
                }
                Message::Close(_) => anyhow::bail!("WebSocket closed"),
                _ => continue, // skip text/ping/pong
            }
        }
    }
}
