//! WebSocket + TLS (WSS) connection to a snapserver.
//!
//! Frame send/receive is shared with the plain-WS transport (see [`super::ws`]);
//! only connection establishment (the TLS handshake) differs here.

use std::sync::Arc;

use anyhow::{Context, Result};
use rustls::ClientConfig;
use snapcast_proto::MessageType;
use snapcast_proto::message::factory::{MessagePayload, TypedMessage};
use tokio_tungstenite::Connector;

use super::ws::{WsStream, recv_frame, send_frame};

/// WebSocket-over-TLS transport for Snapcast binary frames.
pub struct WssConnection {
    ws: Option<WsStream>,
    host: String,
    port: u16,
}

impl WssConnection {
    /// Create a new WSS connection descriptor.
    pub fn new(host: &str, port: u16) -> Self {
        Self {
            ws: None,
            host: host.to_string(),
            port,
        }
    }

    /// Establish the WSS connection.
    pub async fn connect(&mut self) -> Result<()> {
        let url = format!("wss://{}:{}/jsonrpc", self.host, self.port);

        let mut root_store = rustls::RootCertStore::empty();
        let loader = rustls_native_certs::load_native_certs();
        if !loader.errors.is_empty() {
            tracing::warn!("errors loading some native certs: {:?}", loader.errors);
        }
        for cert in loader.certs {
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

    /// Close the WSS connection.
    pub fn disconnect(&mut self) {
        self.ws = None;
    }

    /// Send one binary Snapcast frame over WSS.
    pub async fn send(&mut self, msg_type: MessageType, payload: &MessagePayload) -> Result<()> {
        send_frame(
            self.ws.as_mut().context("not connected")?,
            msg_type,
            payload,
        )
        .await
    }

    /// Receive one binary Snapcast frame over WSS.
    pub async fn recv(&mut self) -> Result<TypedMessage> {
        recv_frame(self.ws.as_mut().context("not connected")?).await
    }
}
