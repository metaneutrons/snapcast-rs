//! Client session management — binary protocol server for snapclients.

use std::collections::HashMap;
use std::sync::Arc;

use anyhow::{Context, Result};
use snapcast_proto::MessageType;
use snapcast_proto::message::base::BaseMessage;
use snapcast_proto::message::codec_header::CodecHeader;
use snapcast_proto::message::factory::{self, MessagePayload, TypedMessage};
use snapcast_proto::message::server_settings::ServerSettings;
use snapcast_proto::message::time::Time;
use snapcast_proto::message::wire_chunk::WireChunk;
use snapcast_proto::types::Timeval;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{Mutex, broadcast, mpsc};

use crate::ServerEvent;
use crate::stream::manager::WireChunkData;
use crate::time::now_usec;

/// Info about a connected streaming client.
#[derive(Debug, Clone)]
pub struct ClientInfo {
    /// Unique client ID (from Hello).
    pub id: String,
    /// Client hostname.
    pub host_name: String,
    /// MAC address.
    pub mac: String,
    /// Whether the client is currently connected.
    pub connected: bool,
}

/// Manages all streaming client sessions.
pub struct SessionServer {
    port: u16,
    buffer_ms: i32,
    clients: Arc<Mutex<HashMap<String, ClientInfo>>>,
}

impl SessionServer {
    /// Create a new session server.
    pub fn new(port: u16, buffer_ms: i32) -> Self {
        Self {
            port,
            buffer_ms,
            clients: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Run the session server — accepts connections and spawns per-client tasks.
    pub async fn run(
        &self,
        chunk_rx: broadcast::Sender<WireChunkData>,
        codec: String,
        codec_header: Vec<u8>,
        event_tx: mpsc::Sender<ServerEvent>,
    ) -> Result<()> {
        let listener = TcpListener::bind(format!("0.0.0.0:{}", self.port)).await?;
        tracing::info!(port = self.port, "Stream server listening");

        loop {
            let (stream, peer) = listener.accept().await?;
            tracing::info!(%peer, "Client connecting");

            let chunk_sub = chunk_rx.subscribe();
            let clients = Arc::clone(&self.clients);
            let event_tx = event_tx.clone();
            let buffer_ms = self.buffer_ms;
            let codec = codec.clone();
            let codec_header = codec_header.clone();

            tokio::spawn(async move {
                if let Err(e) = handle_client(
                    stream,
                    chunk_sub,
                    clients,
                    event_tx,
                    buffer_ms,
                    &codec,
                    &codec_header,
                )
                .await
                {
                    tracing::debug!(%peer, error = %e, "Client session ended");
                }
            });
        }
    }

    /// Get list of connected clients.
    pub async fn connected_clients(&self) -> Vec<ClientInfo> {
        self.clients
            .lock()
            .await
            .values()
            .filter(|c| c.connected)
            .cloned()
            .collect()
    }
}

async fn handle_client(
    mut stream: TcpStream,
    mut chunk_rx: broadcast::Receiver<WireChunkData>,
    clients: Arc<Mutex<HashMap<String, ClientInfo>>>,
    event_tx: mpsc::Sender<ServerEvent>,
    buffer_ms: i32,
    codec: &str,
    codec_header: &[u8],
) -> Result<()> {
    // 1. Read Hello
    let hello_msg = read_frame(&mut stream).await?;
    let hello = match hello_msg.payload {
        MessagePayload::Hello(h) => h,
        _ => anyhow::bail!("expected Hello, got {:?}", hello_msg.base.msg_type),
    };

    let client_id = hello.id.clone();
    tracing::info!(id = %client_id, name = %hello.host_name, mac = %hello.mac, "Client hello");

    // Register client
    {
        let mut map = clients.lock().await;
        map.insert(
            client_id.clone(),
            ClientInfo {
                id: client_id.clone(),
                host_name: hello.host_name.clone(),
                mac: hello.mac.clone(),
                connected: true,
            },
        );
    }

    let _ = event_tx
        .send(ServerEvent::ClientConnected {
            id: client_id.clone(),
            name: hello.host_name.clone(),
        })
        .await;

    // 2. Send ServerSettings
    let ss = ServerSettings {
        buffer_ms,
        latency: 0,
        volume: 100,
        muted: false,
    };
    send_msg(
        &mut stream,
        MessageType::ServerSettings,
        &MessagePayload::ServerSettings(ss),
    )
    .await?;

    // 3. Send CodecHeader
    let ch = CodecHeader {
        codec: codec.to_string(),
        payload: codec_header.to_vec(),
    };
    send_msg(
        &mut stream,
        MessageType::CodecHeader,
        &MessagePayload::CodecHeader(ch),
    )
    .await?;

    // 4. Main loop: forward chunks + handle time sync
    let result = session_loop(&mut stream, &mut chunk_rx).await;

    // Cleanup
    {
        let mut map = clients.lock().await;
        if let Some(c) = map.get_mut(&client_id) {
            c.connected = false;
        }
    }
    let _ = event_tx
        .send(ServerEvent::ClientDisconnected { id: client_id })
        .await;

    result
}

async fn session_loop(
    stream: &mut TcpStream,
    chunk_rx: &mut broadcast::Receiver<WireChunkData>,
) -> Result<()> {
    let (mut reader, mut writer) = stream.split();

    loop {
        tokio::select! {
            // Forward encoded chunks to client
            chunk = chunk_rx.recv() => {
                let chunk = chunk.context("broadcast closed")?;
                let ts_usec = chunk.timestamp_usec;
                let wc = WireChunk {
                    timestamp: Timeval {
                        sec: (ts_usec / 1_000_000) as i32,
                        usec: (ts_usec % 1_000_000) as i32,
                    },
                    payload: chunk.data,
                };
                let mut base = BaseMessage {
                    msg_type: MessageType::WireChunk,
                    id: 0,
                    refers_to: 0,
                    sent: now_timeval(),
                    received: Timeval::default(),
                    size: 0,
                };
                let frame = factory::serialize(&mut base, &MessagePayload::WireChunk(wc))
                    .map_err(|e| anyhow::anyhow!("serialize: {e}"))?;
                writer.write_all(&frame).await.context("write chunk")?;
            }
            // Handle incoming messages (Time sync)
            msg = read_frame_from(&mut reader) => {
                let msg = msg?;
                match msg.payload {
                    MessagePayload::Time(t) => {
                        // Respond with server timestamps
                        let response = Time { latency: t.latency };
                        let mut base = BaseMessage {
                            msg_type: MessageType::Time,
                            id: 0,
                            refers_to: msg.base.id,
                            sent: now_timeval(),
                            received: Timeval::default(),
                            size: 0,
                        };
                        let frame = factory::serialize(&mut base, &MessagePayload::Time(response))
                            .map_err(|e| anyhow::anyhow!("serialize: {e}"))?;
                        writer.write_all(&frame).await.context("write time")?;
                    }
                    _ => {
                        tracing::trace!(msg_type = ?msg.base.msg_type, "Ignoring client message");
                    }
                }
            }
        }
    }
}

async fn send_msg(
    stream: &mut TcpStream,
    msg_type: MessageType,
    payload: &MessagePayload,
) -> Result<()> {
    let mut base = BaseMessage {
        msg_type,
        id: 0,
        refers_to: 0,
        sent: now_timeval(),
        received: Timeval::default(),
        size: 0,
    };
    let frame =
        factory::serialize(&mut base, payload).map_err(|e| anyhow::anyhow!("serialize: {e}"))?;
    stream.write_all(&frame).await.context("write message")?;
    Ok(())
}

async fn read_frame_from<R: AsyncReadExt + Unpin>(reader: &mut R) -> Result<TypedMessage> {
    let mut header_buf = [0u8; BaseMessage::HEADER_SIZE];
    reader
        .read_exact(&mut header_buf)
        .await
        .context("read header")?;
    let mut base = BaseMessage::read_from(&mut &header_buf[..])
        .map_err(|e| anyhow::anyhow!("parse header: {e}"))?;
    base.received = now_timeval();
    let mut payload_buf = vec![0u8; base.size as usize];
    if !payload_buf.is_empty() {
        reader
            .read_exact(&mut payload_buf)
            .await
            .context("read payload")?;
    }
    factory::deserialize(base, &payload_buf).map_err(|e| anyhow::anyhow!("deserialize: {e}"))
}

async fn read_frame(stream: &mut TcpStream) -> Result<TypedMessage> {
    read_frame_from(stream).await
}

fn now_timeval() -> Timeval {
    let usec = now_usec();
    Timeval {
        sec: (usec / 1_000_000) as i32,
        usec: (usec % 1_000_000) as i32,
    }
}
