//! WebSocket + TLS (WSS) connection to a snapserver.

use std::sync::Arc;

use anyhow::{Context, Result};
use futures_util::{SinkExt, StreamExt};
use rustls::ClientConfig;
use snapcast_proto::MessageType;
use snapcast_proto::message::base::BaseMessage;
use snapcast_proto::message::factory::{self, MessagePayload, TypedMessage};
use snapcast_proto::types::Timeval;
use tokio_tungstenite::Connector;
use tokio_tungstenite::tungstenite::Message;

pub struct WssConnection {
    ws: Option<
        tokio_tungstenite::WebSocketStream<
            tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
        >,
    >,
    host: String,
    port: u16,
}

impl WssConnection {
    pub fn new(host: &str, port: u16) -> Self {
        Self {
            ws: None,
            host: host.to_string(),
            port,
        }
    }

    pub async fn connect(&mut self) -> Result<()> {
        let url = format!("wss://{}:{}/jsonrpc", self.host, self.port);

        let mut root_store = rustls::RootCertStore::empty();
        for cert in rustls_native_certs::load_native_certs().expect("load native certs") {
            root_store.add(cert).ok();
        }

        let config = ClientConfig::builder()
            .with_root_certificates(root_store)
            .with_no_client_auth();

        let connector = Connector::Rustls(Arc::new(config));

        let (ws, _) =
            tokio_tungstenite::connect_async_tls_with_config(&url, None, false, Some(connector))
                .await
                .with_context(|| format!("WSS connect to {url}"))?;

        self.ws = Some(ws);
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
                .context("WSS stream ended")?
                .context("WSS error")?;
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
                Message::Close(_) => anyhow::bail!("WSS closed"),
                _ => continue,
            }
        }
    }
}
