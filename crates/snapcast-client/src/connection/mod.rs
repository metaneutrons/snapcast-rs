//! Connection layer.
//!
//! TCP is the supported Snapcast audio transport. The WebSocket modules are
//! kept feature-gated for future interoperability work, but they are not
//! selected by [`SnapConnection::new`] until the server and client can speak a
//! verified binary audio-streaming WebSocket contract.

#[cfg(feature = "websocket")]
pub mod ws;
#[cfg(feature = "tls")]
pub mod wss;

use std::collections::HashMap;
use std::time::Duration;

use anyhow::{Context, Result};
use snapcast_proto::MessageType;
use snapcast_proto::message::base::BaseMessage;
use snapcast_proto::message::factory::{self, MessagePayload, TypedMessage};
use snapcast_proto::types::Timeval;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::oneshot;

/// Read a complete frame (header + payload) from an async reader.
async fn read_frame<R: AsyncReadExt + Unpin>(reader: &mut R) -> Result<TypedMessage> {
    // Read 26-byte header
    let mut header_buf = [0u8; BaseMessage::HEADER_SIZE];
    reader
        .read_exact(&mut header_buf)
        .await
        .context("reading base message header")?;

    let mut base = BaseMessage::read_from(&mut &header_buf[..])
        .map_err(|e| anyhow::anyhow!("parsing header: {e}"))?;

    // Stamp received time using steady clock (matching C++ steadytimeofday)
    base.received = steady_time_of_day();
    ensure_payload_size(base.size)?;

    // Read payload
    let mut payload_buf = vec![0u8; base.size as usize];
    if !payload_buf.is_empty() {
        reader
            .read_exact(&mut payload_buf)
            .await
            .context("reading payload")?;
    }

    factory::deserialize(base, &payload_buf).map_err(|e| anyhow::anyhow!("deserializing: {e}"))
}

pub(crate) fn ensure_payload_size(size: u32) -> Result<()> {
    anyhow::ensure!(
        size <= snapcast_proto::DEFAULT_MAX_PAYLOAD_SIZE,
        "payload too large: {size} bytes"
    );
    Ok(())
}

/// Write a complete frame (header + payload) to an async writer.
async fn write_frame<W: AsyncWriteExt + Unpin>(
    writer: &mut W,
    base: &mut BaseMessage,
    payload: &MessagePayload,
) -> Result<()> {
    let frame =
        factory::serialize(base, payload).map_err(|e| anyhow::anyhow!("serializing: {e}"))?;
    writer.write_all(&frame).await.context("writing frame")?;
    Ok(())
}

/// Pending request waiting for a response.
struct PendingRequest {
    tx: oneshot::Sender<TypedMessage>,
}

/// TCP connection to a snapserver.
pub struct TcpConnection {
    stream: Option<TcpStream>,
    host: String,
    port: u16,
    pending: HashMap<u16, PendingRequest>,
    next_id: u16,
}

/// Unified connection over supported transports.
pub enum SnapConnection {
    /// Plain TCP connection.
    Tcp(TcpConnection),
    #[cfg(feature = "websocket")]
    /// WebSocket (non-secure) connection.
    Ws(ws::WsConnection),
    #[cfg(feature = "tls")]
    /// WebSocket over TLS (secure) connection.
    Wss(wss::WssConnection),
}

impl SnapConnection {
    /// Create a new connection based on the scheme.
    pub fn new(scheme: &str, host: &str, port: u16) -> Result<Self> {
        match scheme {
            snapcast_proto::SCHEME_TCP => Ok(Self::Tcp(TcpConnection::new(host, port))),
            snapcast_proto::SCHEME_WS | snapcast_proto::SCHEME_WSS => anyhow::bail!(
                "websocket audio transport is not supported yet; use tcp:// for Snapcast audio"
            ),
            _ => anyhow::bail!("unsupported scheme: {scheme}"),
        }
    }

    /// Establish the connection.
    pub async fn connect(&mut self) -> Result<()> {
        match self {
            Self::Tcp(c) => c.connect().await,
            #[cfg(feature = "websocket")]
            Self::Ws(c) => c.connect().await,
            #[cfg(feature = "tls")]
            Self::Wss(c) => c.connect().await,
        }
    }

    /// Close the connection.
    pub fn disconnect(&mut self) {
        match self {
            Self::Tcp(c) => c.disconnect(),
            #[cfg(feature = "websocket")]
            Self::Ws(c) => c.disconnect(),
            #[cfg(feature = "tls")]
            Self::Wss(c) => c.disconnect(),
        }
    }

    /// Send a message.
    pub async fn send(&mut self, msg_type: MessageType, payload: &MessagePayload) -> Result<()> {
        match self {
            Self::Tcp(c) => c.send(msg_type, payload).await,
            #[cfg(feature = "websocket")]
            Self::Ws(c) => c.send(msg_type, payload).await,
            #[cfg(feature = "tls")]
            Self::Wss(c) => c.send(msg_type, payload).await,
        }
    }

    /// Receive the next message.
    pub async fn recv(&mut self) -> Result<TypedMessage> {
        match self {
            Self::Tcp(c) => c.recv().await,
            #[cfg(feature = "websocket")]
            Self::Ws(c) => c.recv().await,
            #[cfg(feature = "tls")]
            Self::Wss(c) => c.recv().await,
        }
    }
}

impl TcpConnection {
    /// Create a new connection to the given host and port.
    pub fn new(host: &str, port: u16) -> Self {
        Self {
            stream: None,
            host: host.to_string(),
            port,
            pending: HashMap::new(),
            next_id: 1,
        }
    }

    /// Establish the TCP connection.
    pub async fn connect(&mut self) -> Result<()> {
        let addr = format!("{}:{}", self.host, self.port);
        let stream = TcpStream::connect(&addr)
            .await
            .with_context(|| format!("connecting to {addr}"))?;
        self.stream = Some(stream);
        self.pending.clear();
        self.next_id = 1;
        Ok(())
    }

    /// Close the connection.
    pub fn disconnect(&mut self) {
        self.stream = None;
        self.pending.clear();
    }

    fn stream_mut(&mut self) -> Result<&mut TcpStream> {
        self.stream.as_mut().context("not connected")
    }

    /// Send a message without waiting for a response.
    pub async fn send(&mut self, msg_type: MessageType, payload: &MessagePayload) -> Result<()> {
        let stream = self.stream_mut()?;
        let mut base = BaseMessage {
            msg_type,
            id: 0,
            refers_to: 0,
            sent: Timeval::default(),
            received: Timeval::default(),
            size: 0,
        };
        stamp_sent(&mut base);
        write_frame(stream, &mut base, payload).await
    }

    /// Send a request and wait for the response (matched by `refersTo`).
    pub async fn send_request(
        &mut self,
        msg_type: MessageType,
        payload: &MessagePayload,
        timeout: Duration,
    ) -> Result<TypedMessage> {
        let id = self.next_id;
        self.next_id = self.next_id.wrapping_add(1);

        let (tx, rx) = oneshot::channel();
        self.pending.insert(id, PendingRequest { tx });

        let stream = self.stream_mut()?;
        let mut base = BaseMessage {
            msg_type,
            id,
            refers_to: 0,
            sent: Timeval::default(),
            received: Timeval::default(),
            size: 0,
        };
        stamp_sent(&mut base);
        write_frame(stream, &mut base, payload).await?;

        tokio::time::timeout(timeout, rx)
            .await
            .context("request timed out")?
            .context("response channel closed")
    }

    /// Receive the next message. If it's a response to a pending request,
    /// deliver it to the waiting caller and receive again.
    pub async fn recv(&mut self) -> Result<TypedMessage> {
        loop {
            let stream = self.stream_mut()?;
            let msg = read_frame(stream).await?;

            if msg.base.refers_to != 0
                && let Some(pending) = self.pending.remove(&msg.base.refers_to)
            {
                let _ = pending.tx.send(msg);
                continue;
            }
            return Ok(msg);
        }
    }
}

pub(super) fn stamp_sent(base: &mut BaseMessage) {
    let tv = steady_time_of_day();
    base.sent = tv;
}

/// Matches the C++ `chronos::steadytimeofday` — monotonic clock time.
/// On macOS/Linux this samples the same clock domain as the C++ snapserver.
pub(super) fn steady_time_of_day() -> Timeval {
    let usec = snapcast_proto::time::now_usec();
    Timeval {
        sec: (usec / 1_000_000) as i32,
        usec: (usec % 1_000_000) as i32,
    }
}

/// Current time in microseconds using the steady clock.
///
/// Single source of truth lives in [`snapcast_proto::time`] so the client and
/// server cannot drift onto different clock domains.
pub fn now_usec() -> i64 {
    snapcast_proto::time::now_usec()
}

#[cfg(test)]
mod tests {
    use super::*;
    use snapcast_proto::message::time::Time;

    /// Test frame read/write with in-memory buffers (no network needed).
    #[tokio::test]
    async fn write_and_read_frame() {
        let payload = MessagePayload::Time(Time {
            latency: Timeval { sec: 0, usec: 1234 },
        });
        let mut base = BaseMessage {
            msg_type: MessageType::Time,
            id: 42,
            refers_to: 0,
            sent: Timeval { sec: 1, usec: 0 },
            received: Timeval::default(),
            size: 0,
        };

        // Write to buffer
        let mut buf = Vec::new();
        write_frame(&mut buf, &mut base, &payload).await.unwrap();

        // Size should be header + payload
        assert_eq!(buf.len(), BaseMessage::HEADER_SIZE + Time::SIZE as usize);

        // Read back
        let mut cursor = std::io::Cursor::new(&buf);
        let msg = read_frame(&mut cursor).await.unwrap();
        assert_eq!(msg.base.msg_type, MessageType::Time);
        assert_eq!(msg.base.id, 42);
        match msg.payload {
            MessagePayload::Time(t) => assert_eq!(t.latency.usec, 1234),
            _ => panic!("expected Time"),
        }
    }

    #[tokio::test]
    async fn write_and_read_error_frame() {
        use snapcast_proto::message::error::Error;

        let payload = MessagePayload::Error(Error {
            code: 401,
            error: "Unauthorized".into(),
            message: "bad auth".into(),
        });
        let mut base = BaseMessage {
            msg_type: MessageType::Error,
            id: 0,
            refers_to: 7,
            sent: Timeval::default(),
            received: Timeval::default(),
            size: 0,
        };

        let mut buf = Vec::new();
        write_frame(&mut buf, &mut base, &payload).await.unwrap();

        let mut cursor = std::io::Cursor::new(&buf);
        let msg = read_frame(&mut cursor).await.unwrap();
        assert_eq!(msg.base.refers_to, 7);
        match msg.payload {
            MessagePayload::Error(e) => {
                assert_eq!(e.code, 401);
                assert_eq!(e.error, "Unauthorized");
            }
            _ => panic!("expected Error"),
        }
    }

    #[tokio::test]
    async fn write_and_read_multiple_frames() {
        let frames: Vec<(MessageType, MessagePayload)> = vec![
            (MessageType::Time, MessagePayload::Time(Time::default())),
            (
                MessageType::ClientInfo,
                MessagePayload::ClientInfo(snapcast_proto::message::client_info::ClientInfo {
                    volume: 80,
                    muted: false,
                }),
            ),
        ];

        let mut buf = Vec::new();
        for (mt, payload) in &frames {
            let mut base = BaseMessage {
                msg_type: *mt,
                id: 0,
                refers_to: 0,
                sent: Timeval::default(),
                received: Timeval::default(),
                size: 0,
            };
            write_frame(&mut buf, &mut base, payload).await.unwrap();
        }

        // Read both back
        let mut cursor = std::io::Cursor::new(&buf);
        let msg1 = read_frame(&mut cursor).await.unwrap();
        assert_eq!(msg1.base.msg_type, MessageType::Time);
        let msg2 = read_frame(&mut cursor).await.unwrap();
        assert_eq!(msg2.base.msg_type, MessageType::ClientInfo);
    }

    #[test]
    fn tcp_connection_new() {
        let conn = TcpConnection::new("localhost", 1704);
        assert!(conn.stream.is_none());
        assert_eq!(conn.host, "localhost");
        assert_eq!(conn.port, 1704);
    }

    #[test]
    fn rejects_oversized_payload() {
        let too_large = snapcast_proto::DEFAULT_MAX_PAYLOAD_SIZE + 1;
        assert!(ensure_payload_size(too_large).is_err());
    }

    #[test]
    fn rejects_websocket_audio_scheme() {
        assert!(SnapConnection::new("ws", "localhost", 1780).is_err());
        assert!(SnapConnection::new("wss", "localhost", 1788).is_err());
    }
}
