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

use crate::ClientSettingsUpdate;
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
    auth: Option<Arc<dyn crate::auth::AuthValidator>>,
    clients: Arc<Mutex<HashMap<String, ClientInfo>>>,
    settings_senders: Arc<Mutex<HashMap<String, mpsc::Sender<ClientSettingsUpdate>>>>,
    #[cfg(feature = "custom-protocol")]
    custom_senders: Arc<Mutex<HashMap<String, mpsc::Sender<CustomOutbound>>>>,
}

/// Outbound custom message to a specific client.
#[cfg(feature = "custom-protocol")]
#[derive(Debug, Clone)]
pub struct CustomOutbound {
    /// Message type ID (9+).
    pub type_id: u16,
    /// Raw payload.
    pub payload: Vec<u8>,
}

impl SessionServer {
    /// Create a new session server.
    pub fn new(
        port: u16,
        buffer_ms: i32,
        auth: Option<Arc<dyn crate::auth::AuthValidator>>,
    ) -> Self {
        Self {
            port,
            buffer_ms,
            auth,
            clients: Arc::new(Mutex::new(HashMap::new())),
            settings_senders: Arc::new(Mutex::new(HashMap::new())),
            #[cfg(feature = "custom-protocol")]
            custom_senders: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Push a settings update to a specific streaming client.
    pub async fn push_settings(&self, update: ClientSettingsUpdate) {
        let senders = self.settings_senders.lock().await;
        if let Some(tx) = senders.get(&update.client_id) {
            let _ = tx.send(update).await;
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
            let settings_senders = Arc::clone(&self.settings_senders);
            #[cfg(feature = "custom-protocol")]
            let custom_senders = Arc::clone(&self.custom_senders);
            let event_tx = event_tx.clone();
            let buffer_ms = self.buffer_ms;
            let auth = self.auth.clone();
            let codec = codec.clone();
            let codec_header = codec_header.clone();

            tokio::spawn(async move {
                let (settings_tx, settings_rx) = mpsc::channel(16);
                #[cfg(feature = "custom-protocol")]
                let (custom_tx, custom_rx) = mpsc::channel(64);
                let result = handle_client(
                    stream,
                    chunk_sub,
                    settings_rx,
                    #[cfg(feature = "custom-protocol")]
                    custom_rx,
                    &clients,
                    &settings_senders,
                    #[cfg(feature = "custom-protocol")]
                    &custom_senders,
                    settings_tx,
                    #[cfg(feature = "custom-protocol")]
                    custom_tx,
                    event_tx,
                    auth.as_deref(),
                    buffer_ms,
                    &codec,
                    &codec_header,
                )
                .await;
                if let Err(e) = result {
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

    /// Send a custom binary protocol message to a specific client.
    #[cfg(feature = "custom-protocol")]
    pub async fn send_custom(&self, client_id: &str, type_id: u16, payload: Vec<u8>) {
        let senders = self.custom_senders.lock().await;
        if let Some(tx) = senders.get(client_id) {
            let _ = tx.send(CustomOutbound { type_id, payload }).await;
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn handle_client(
    mut stream: TcpStream,
    chunk_rx: broadcast::Receiver<WireChunkData>,
    settings_rx: mpsc::Receiver<ClientSettingsUpdate>,
    #[cfg(feature = "custom-protocol")] custom_rx: mpsc::Receiver<CustomOutbound>,
    clients: &Mutex<HashMap<String, ClientInfo>>,
    settings_senders: &Mutex<HashMap<String, mpsc::Sender<ClientSettingsUpdate>>>,
    #[cfg(feature = "custom-protocol")] custom_senders: &Mutex<
        HashMap<String, mpsc::Sender<CustomOutbound>>,
    >,
    settings_tx: mpsc::Sender<ClientSettingsUpdate>,
    #[cfg(feature = "custom-protocol")] custom_tx: mpsc::Sender<CustomOutbound>,
    event_tx: mpsc::Sender<ServerEvent>,
    auth: Option<&dyn crate::auth::AuthValidator>,
    buffer_ms: i32,
    codec: &str,
    codec_header: &[u8],
) -> Result<()> {
    // 1. Read Hello
    let hello_msg = read_frame_from(&mut stream).await?;
    let hello = match hello_msg.payload {
        MessagePayload::Hello(h) => h,
        _ => anyhow::bail!("expected Hello, got {:?}", hello_msg.base.msg_type),
    };

    let client_id = hello.id.clone();
    tracing::info!(id = %client_id, name = %hello.host_name, mac = %hello.mac, "Client hello");

    // 1b. Validate auth if required
    if let Some(validator) = auth {
        let auth_result = match &hello.auth {
            Some(a) => validator.validate(&a.scheme, &a.param),
            None => Err(crate::auth::AuthError::Unauthorized(
                "Authentication required".into(),
            )),
        };
        match auth_result {
            Ok(result) => {
                if !result.permissions.iter().any(|p| p == "Streaming") {
                    let err = snapcast_proto::message::error::Error {
                        code: 403,
                        message: "Forbidden".into(),
                        error: "Permission 'Streaming' missing".into(),
                    };
                    send_msg(&mut stream, MessageType::Error, &MessagePayload::Error(err)).await?;
                    anyhow::bail!("Client {client_id}: missing Streaming permission");
                }
                tracing::info!(id = %client_id, user = %result.username, "Authenticated");
            }
            Err(e) => {
                let err = snapcast_proto::message::error::Error {
                    code: e.code() as u32,
                    message: e.message().to_string(),
                    error: e.message().to_string(),
                };
                send_msg(&mut stream, MessageType::Error, &MessagePayload::Error(err)).await?;
                anyhow::bail!("Client {client_id}: {e}");
            }
        }
    }

    // Register client + settings channel
    {
        clients.lock().await.insert(
            client_id.clone(),
            ClientInfo {
                id: client_id.clone(),
                host_name: hello.host_name.clone(),
                mac: hello.mac.clone(),
                connected: true,
            },
        );
        settings_senders
            .lock()
            .await
            .insert(client_id.clone(), settings_tx);
        #[cfg(feature = "custom-protocol")]
        custom_senders
            .lock()
            .await
            .insert(client_id.clone(), custom_tx);
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

    // 4. Main loop
    let result = session_loop(
        &mut stream,
        chunk_rx,
        settings_rx,
        #[cfg(feature = "custom-protocol")]
        custom_rx,
        #[cfg(feature = "custom-protocol")]
        event_tx.clone(),
        #[cfg(feature = "custom-protocol")]
        client_id.clone(),
    )
    .await;

    // Cleanup
    {
        let mut map = clients.lock().await;
        if let Some(c) = map.get_mut(&client_id) {
            c.connected = false;
        }
    }
    settings_senders.lock().await.remove(&client_id);
    #[cfg(feature = "custom-protocol")]
    custom_senders.lock().await.remove(&client_id);
    let _ = event_tx
        .send(ServerEvent::ClientDisconnected { id: client_id })
        .await;

    result
}

async fn session_loop(
    stream: &mut TcpStream,
    mut chunk_rx: broadcast::Receiver<WireChunkData>,
    mut settings_rx: mpsc::Receiver<ClientSettingsUpdate>,
    #[cfg(feature = "custom-protocol")] mut custom_rx: mpsc::Receiver<CustomOutbound>,
    #[cfg(feature = "custom-protocol")] event_tx: mpsc::Sender<ServerEvent>,
    #[cfg(feature = "custom-protocol")] client_id: String,
) -> Result<()> {
    let (mut reader, mut writer) = stream.split();

    #[cfg(not(feature = "custom-protocol"))]
    let (mut custom_rx, _event_tx, _client_id): (mpsc::Receiver<()>, Option<()>, String) = {
        let (_tx, rx) = mpsc::channel(1);
        (rx, None, String::new())
    };

    loop {
        tokio::select! {
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
                let frame = serialize_msg(MessageType::WireChunk, &MessagePayload::WireChunk(wc), 0)?;
                writer.write_all(&frame).await.context("write chunk")?;
            }
            msg = read_frame_from(&mut reader) => {
                let msg = msg?;
                match msg.payload {
                    MessagePayload::Time(t) => {
                        let response = Time { latency: t.latency };
                        let frame = serialize_msg(MessageType::Time, &MessagePayload::Time(response), msg.base.id)?;
                        writer.write_all(&frame).await.context("write time")?;
                    }
                    #[cfg(feature = "custom-protocol")]
                    MessagePayload::Custom(payload) => {
                        if let MessageType::Custom(type_id) = msg.base.msg_type {
                            let _ = event_tx.send(ServerEvent::CustomMessage {
                                client_id: client_id.clone(),
                                message: snapcast_proto::CustomMessage::new(type_id, payload),
                            }).await;
                        }
                    }
                    _ => {}
                }
            }
            update = settings_rx.recv() => {
                let Some(update) = update else { continue };
                let ss = ServerSettings {
                    buffer_ms: update.buffer_ms,
                    latency: update.latency,
                    volume: update.volume,
                    muted: update.muted,
                };
                let frame = serialize_msg(
                    MessageType::ServerSettings,
                    &MessagePayload::ServerSettings(ss),
                    0,
                )?;
                writer.write_all(&frame).await.context("write settings")?;
                tracing::debug!(volume = update.volume, latency = update.latency, "Pushed settings to client");
            }
            outbound = custom_rx.recv() => {
                #[cfg(feature = "custom-protocol")]
                if let Some(msg) = outbound {
                    let frame = serialize_msg(
                        MessageType::Custom(msg.type_id),
                        &MessagePayload::Custom(msg.payload),
                        0,
                    )?;
                    writer.write_all(&frame).await.context("write custom")?;
                }
                #[cfg(not(feature = "custom-protocol"))]
                let _ = outbound;
            }
        }
    }
}

fn serialize_msg(
    msg_type: MessageType,
    payload: &MessagePayload,
    refers_to: u16,
) -> Result<Vec<u8>> {
    let mut base = BaseMessage {
        msg_type,
        id: 0,
        refers_to,
        sent: now_timeval(),
        received: Timeval::default(),
        size: 0,
    };
    factory::serialize(&mut base, payload).map_err(|e| anyhow::anyhow!("serialize: {e}"))
}

async fn send_msg(
    stream: &mut TcpStream,
    msg_type: MessageType,
    payload: &MessagePayload,
) -> Result<()> {
    let frame = serialize_msg(msg_type, payload, 0)?;
    stream.write_all(&frame).await.context("write message")
}

async fn read_frame_from<R: AsyncReadExt + Unpin>(reader: &mut R) -> Result<TypedMessage> {
    let mut header_buf = [0u8; BaseMessage::HEADER_SIZE];
    reader
        .read_exact(&mut header_buf)
        .await
        .context("read header")?;
    let mut base =
        BaseMessage::read_from(&mut &header_buf[..]).map_err(|e| anyhow::anyhow!("parse: {e}"))?;
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

fn now_timeval() -> Timeval {
    let usec = now_usec();
    Timeval {
        sec: (usec / 1_000_000) as i32,
        usec: (usec % 1_000_000) as i32,
    }
}
