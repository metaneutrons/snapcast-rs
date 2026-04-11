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
use tokio::sync::{Mutex, broadcast, mpsc, watch};

use crate::ClientSettingsUpdate;
use crate::ServerEvent;
use crate::WireChunkData;
use crate::time::now_usec;

// ── Routing ───────────────────────────────────────────────────

/// Per-session routing state — updated when groups/streams/mute change.
#[derive(Debug, Clone, PartialEq)]
pub struct SessionRouting {
    /// Stream ID this client should receive audio from.
    pub stream_id: String,
    /// Client is muted.
    pub client_muted: bool,
    /// Client's group is muted.
    pub group_muted: bool,
}

// ── Codec registry ────────────────────────────────────────────

/// Per-stream codec header, stored at stream registration time.
#[derive(Debug, Clone)]
pub struct StreamCodecInfo {
    /// Codec name (e.g. "flac", "pcm", "f32lz4").
    pub codec: String,
    /// Encoded codec header bytes.
    pub header: Vec<u8>,
}

// ── Session context ───────────────────────────────────────────

/// Shared context for all sessions — replaces argument explosion.
pub struct SessionContext {
    pub buffer_ms: i32,
    pub auth: Option<Arc<dyn crate::auth::AuthValidator>>,
    pub send_audio_to_muted: bool,
    pub settings_senders: Mutex<HashMap<String, mpsc::Sender<ClientSettingsUpdate>>>,
    #[cfg(feature = "custom-protocol")]
    pub custom_senders: Mutex<HashMap<String, mpsc::Sender<CustomOutbound>>>,
    pub routing_senders: Mutex<HashMap<String, watch::Sender<SessionRouting>>>,
    pub codec_headers: Mutex<HashMap<String, StreamCodecInfo>>,
    pub shared_state: Arc<tokio::sync::Mutex<crate::state::ServerState>>,
    pub default_stream: String,
}

// ── Session server ────────────────────────────────────────────

/// Manages all streaming client sessions.
pub struct SessionServer {
    port: u16,
    ctx: Arc<SessionContext>,
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
        shared_state: Arc<tokio::sync::Mutex<crate::state::ServerState>>,
        default_stream: String,
        send_audio_to_muted: bool,
    ) -> Self {
        Self {
            port,
            ctx: Arc::new(SessionContext {
                buffer_ms,
                auth,
                send_audio_to_muted,
                settings_senders: Mutex::new(HashMap::new()),
                #[cfg(feature = "custom-protocol")]
                custom_senders: Mutex::new(HashMap::new()),
                routing_senders: Mutex::new(HashMap::new()),
                codec_headers: Mutex::new(HashMap::new()),
                shared_state,
                default_stream,
            }),
        }
    }

    /// Register a stream's codec header (called during stream setup).
    pub async fn register_stream_codec(&self, stream_id: &str, codec: &str, header: &[u8]) {
        self.ctx.codec_headers.lock().await.insert(
            stream_id.to_string(),
            StreamCodecInfo {
                codec: codec.to_string(),
                header: header.to_vec(),
            },
        );
    }

    /// Push a settings update to a specific streaming client.
    pub async fn push_settings(&self, update: ClientSettingsUpdate) {
        let senders = self.ctx.settings_senders.lock().await;
        if let Some(tx) = senders.get(&update.client_id) {
            let _ = tx.send(update).await;
        }
    }

    /// Update routing for a single client from current server state.
    pub async fn update_routing_for_client(
        &self,
        client_id: &str,
        shared_state: &tokio::sync::Mutex<crate::state::ServerState>,
    ) {
        let s = shared_state.lock().await;
        let senders = self.ctx.routing_senders.lock().await;
        if let Some(tx) = senders.get(client_id)
            && let Some(group) = s
                .groups
                .iter()
                .find(|g| g.clients.contains(&client_id.to_string()))
        {
            let client_muted = s
                .clients
                .get(client_id)
                .map(|c| c.config.volume.muted)
                .unwrap_or(false);
            let _ = tx.send(SessionRouting {
                stream_id: group.stream_id.clone(),
                client_muted,
                group_muted: group.muted,
            });
        }
    }

    /// Update routing for all clients in a group.
    pub async fn update_routing_for_group(
        &self,
        group_id: &str,
        shared_state: &tokio::sync::Mutex<crate::state::ServerState>,
    ) {
        let s = shared_state.lock().await;
        let senders = self.ctx.routing_senders.lock().await;
        if let Some(group) = s.groups.iter().find(|g| g.id == group_id) {
            for client_id in &group.clients {
                if let Some(tx) = senders.get(client_id) {
                    let client_muted = s
                        .clients
                        .get(client_id)
                        .map(|c| c.config.volume.muted)
                        .unwrap_or(false);
                    let _ = tx.send(SessionRouting {
                        stream_id: group.stream_id.clone(),
                        client_muted,
                        group_muted: group.muted,
                    });
                }
            }
        }
    }

    /// Update routing for all connected sessions (after structural changes).
    pub async fn update_routing_all(
        &self,
        shared_state: &tokio::sync::Mutex<crate::state::ServerState>,
    ) {
        let s = shared_state.lock().await;
        let senders = self.ctx.routing_senders.lock().await;
        for group in &s.groups {
            for client_id in &group.clients {
                if let Some(tx) = senders.get(client_id) {
                    let client_muted = s
                        .clients
                        .get(client_id)
                        .map(|c| c.config.volume.muted)
                        .unwrap_or(false);
                    let _ = tx.send(SessionRouting {
                        stream_id: group.stream_id.clone(),
                        client_muted,
                        group_muted: group.muted,
                    });
                }
            }
        }
    }

    /// Run the session server — accepts connections and spawns per-client tasks.
    pub async fn run(
        &self,
        chunk_rx: broadcast::Sender<WireChunkData>,
        event_tx: mpsc::Sender<ServerEvent>,
    ) -> Result<()> {
        let listener = TcpListener::bind(format!("0.0.0.0:{}", self.port)).await?;
        tracing::info!(port = self.port, "Stream server listening");

        loop {
            let (stream, peer) = listener.accept().await?;
            tracing::info!(%peer, "Client connecting");

            let chunk_sub = chunk_rx.subscribe();
            let ctx = Arc::clone(&self.ctx);
            let event_tx = event_tx.clone();

            tokio::spawn(async move {
                let result = handle_client(stream, chunk_sub, &ctx, event_tx).await;
                if let Err(e) = result {
                    tracing::debug!(%peer, error = %e, "Client session ended");
                }
            });
        }
    }

    /// Send a custom binary protocol message to a specific client.
    #[cfg(feature = "custom-protocol")]
    pub async fn send_custom(&self, client_id: &str, type_id: u16, payload: Vec<u8>) {
        let senders = self.ctx.custom_senders.lock().await;
        if let Some(tx) = senders.get(client_id) {
            let _ = tx.send(CustomOutbound { type_id, payload }).await;
        }
    }
}

// ── Client handler ────────────────────────────────────────────

async fn handle_client(
    mut stream: TcpStream,
    chunk_rx: broadcast::Receiver<WireChunkData>,
    ctx: &SessionContext,
    event_tx: mpsc::Sender<ServerEvent>,
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
    if let Some(validator) = &ctx.auth {
        validate_auth(validator.as_ref(), &hello, &mut stream, &client_id).await?;
    }

    // 2. Register + determine initial routing
    let (settings_tx, settings_rx) = mpsc::channel(16);
    #[cfg(feature = "custom-protocol")]
    let (custom_tx, custom_rx) = mpsc::channel(64);

    let initial_routing;
    let initial_stream_id;
    {
        ctx.settings_senders
            .lock()
            .await
            .insert(client_id.clone(), settings_tx);
        #[cfg(feature = "custom-protocol")]
        ctx.custom_senders
            .lock()
            .await
            .insert(client_id.clone(), custom_tx);

        let mut s = ctx.shared_state.lock().await;
        let c = s.get_or_create_client(&client_id, &hello.host_name, &hello.mac);
        c.connected = true;
        s.group_for_client(&client_id, &ctx.default_stream);

        let group = s.groups.iter().find(|g| g.clients.contains(&client_id));
        initial_stream_id = group
            .map(|g| g.stream_id.clone())
            .unwrap_or_else(|| ctx.default_stream.clone());
        initial_routing = SessionRouting {
            stream_id: initial_stream_id.clone(),
            client_muted: s
                .clients
                .get(&client_id)
                .map(|c| c.config.volume.muted)
                .unwrap_or(false),
            group_muted: group.map(|g| g.muted).unwrap_or(false),
        };
    }

    let (routing_tx, routing_rx) = watch::channel(initial_routing);
    ctx.routing_senders
        .lock()
        .await
        .insert(client_id.clone(), routing_tx);

    let _ = event_tx
        .send(ServerEvent::ClientConnected {
            id: client_id.clone(),
            name: hello.host_name.clone(),
            mac: hello.mac.clone(),
        })
        .await;

    // 3. Send ServerSettings
    let ss = ServerSettings {
        buffer_ms: ctx.buffer_ms,
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

    // 4. Send CodecHeader for the client's stream
    send_codec_header_for_stream(&mut stream, &initial_stream_id, ctx).await?;

    // 5. Main loop
    #[cfg(feature = "custom-protocol")]
    let result = session_loop(
        &mut stream,
        chunk_rx,
        settings_rx,
        routing_rx,
        custom_rx,
        event_tx.clone(),
        client_id.clone(),
        ctx,
    )
    .await;
    #[cfg(not(feature = "custom-protocol"))]
    let result = session_loop(&mut stream, chunk_rx, settings_rx, routing_rx, ctx).await;

    // Cleanup
    ctx.settings_senders.lock().await.remove(&client_id);
    ctx.routing_senders.lock().await.remove(&client_id);
    #[cfg(feature = "custom-protocol")]
    ctx.custom_senders.lock().await.remove(&client_id);
    {
        let mut s = ctx.shared_state.lock().await;
        if let Some(c) = s.clients.get_mut(&client_id) {
            c.connected = false;
        }
    }
    let _ = event_tx
        .send(ServerEvent::ClientDisconnected { id: client_id })
        .await;

    result
}

async fn validate_auth(
    validator: &dyn crate::auth::AuthValidator,
    hello: &snapcast_proto::message::hello::Hello,
    stream: &mut TcpStream,
    client_id: &str,
) -> Result<()> {
    let auth_result = match &hello.auth {
        Some(a) => validator.validate(&a.scheme, &a.param),
        None => Err(crate::auth::AuthError::Unauthorized(
            "Authentication required".into(),
        )),
    };
    match auth_result {
        Ok(result) => {
            if !result
                .permissions
                .iter()
                .any(|p| p == crate::auth::PERM_STREAMING)
            {
                let err = snapcast_proto::message::error::Error {
                    code: 403,
                    message: "Forbidden".into(),
                    error: "Permission 'Streaming' missing".into(),
                };
                send_msg(stream, MessageType::Error, &MessagePayload::Error(err)).await?;
                anyhow::bail!("Client {client_id}: missing Streaming permission");
            }
            tracing::info!(id = %client_id, user = %result.username, "Authenticated");
            Ok(())
        }
        Err(e) => {
            let err = snapcast_proto::message::error::Error {
                code: e.code() as u32,
                message: e.message().to_string(),
                error: e.message().to_string(),
            };
            send_msg(stream, MessageType::Error, &MessagePayload::Error(err)).await?;
            anyhow::bail!("Client {client_id}: {e}");
        }
    }
}

/// Send the codec header for a specific stream (or default).
async fn send_codec_header_for_stream(
    stream: &mut TcpStream,
    stream_id: &str,
    ctx: &SessionContext,
) -> Result<()> {
    let headers = ctx.codec_headers.lock().await;
    let info = headers
        .get(stream_id)
        .or_else(|| headers.get(&ctx.default_stream));
    if let Some(info) = info {
        let ch = CodecHeader {
            codec: info.codec.clone(),
            payload: info.header.clone(),
        };
        send_msg(
            stream,
            MessageType::CodecHeader,
            &MessagePayload::CodecHeader(ch),
        )
        .await?;
    }
    Ok(())
}

// ── Session loop ──────────────────────────────────────────────

#[cfg(not(feature = "custom-protocol"))]
async fn session_loop(
    stream: &mut TcpStream,
    mut chunk_rx: broadcast::Receiver<WireChunkData>,
    mut settings_rx: mpsc::Receiver<ClientSettingsUpdate>,
    mut routing_rx: watch::Receiver<SessionRouting>,
    ctx: &SessionContext,
) -> Result<()> {
    let (mut reader, mut writer) = stream.split();
    let mut routing = routing_rx.borrow().clone();

    loop {
        tokio::select! {
            chunk = chunk_rx.recv() => {
                let chunk = chunk.context("broadcast closed")?;
                if !should_send_chunk(&chunk, &routing, ctx.send_audio_to_muted) {
                    continue;
                }
                write_chunk(&mut writer, chunk).await?;
            }
            Ok(()) = routing_rx.changed() => {
                let new = routing_rx.borrow().clone();
                if new.stream_id != routing.stream_id {
                    tracing::debug!(old = %routing.stream_id, new = %new.stream_id, "Stream switch");
                    // Send new codec header for the new stream
                    let headers = ctx.codec_headers.lock().await;
                    if let Some(info) = headers.get(&new.stream_id) {
                        let ch = CodecHeader { codec: info.codec.clone(), payload: info.header.clone() };
                        let frame = serialize_msg(MessageType::CodecHeader, &MessagePayload::CodecHeader(ch), 0)?;
                        writer.write_all(&frame).await.context("write codec header")?;
                    }
                }
                routing = new;
            }
            msg = read_frame_from(&mut reader) => {
                let msg = msg?;
                if let MessagePayload::Time(t) = msg.payload {
                    let response = Time { latency: t.latency };
                    let frame = serialize_msg(MessageType::Time, &MessagePayload::Time(response), msg.base.id)?;
                    writer.write_all(&frame).await.context("write time")?;
                }
            }
            update = settings_rx.recv() => {
                let Some(update) = update else { continue };
                write_settings(&mut writer, update).await?;
            }
        }
    }
}

#[cfg(feature = "custom-protocol")]
async fn session_loop(
    stream: &mut TcpStream,
    mut chunk_rx: broadcast::Receiver<WireChunkData>,
    mut settings_rx: mpsc::Receiver<ClientSettingsUpdate>,
    mut routing_rx: watch::Receiver<SessionRouting>,
    mut custom_rx: mpsc::Receiver<CustomOutbound>,
    event_tx: mpsc::Sender<ServerEvent>,
    client_id: String,
    ctx: &SessionContext,
) -> Result<()> {
    let (mut reader, mut writer) = stream.split();
    let mut routing = routing_rx.borrow().clone();

    loop {
        tokio::select! {
            chunk = chunk_rx.recv() => {
                let chunk = chunk.context("broadcast closed")?;
                if !should_send_chunk(&chunk, &routing, ctx.send_audio_to_muted) {
                    continue;
                }
                write_chunk(&mut writer, chunk).await?;
            }
            Ok(()) = routing_rx.changed() => {
                let new = routing_rx.borrow().clone();
                if new.stream_id != routing.stream_id {
                    tracing::debug!(old = %routing.stream_id, new = %new.stream_id, "Stream switch");
                    let headers = ctx.codec_headers.lock().await;
                    if let Some(info) = headers.get(&new.stream_id) {
                        let ch = CodecHeader { codec: info.codec.clone(), payload: info.header.clone() };
                        let frame = serialize_msg(MessageType::CodecHeader, &MessagePayload::CodecHeader(ch), 0)?;
                        writer.write_all(&frame).await.context("write codec header")?;
                    }
                }
                routing = new;
            }
            msg = read_frame_from(&mut reader) => {
                let msg = msg?;
                match msg.payload {
                    MessagePayload::Time(t) => {
                        let response = Time { latency: t.latency };
                        let frame = serialize_msg(MessageType::Time, &MessagePayload::Time(response), msg.base.id)?;
                        writer.write_all(&frame).await.context("write time")?;
                    }
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
                write_settings(&mut writer, update).await?;
            }
            outbound = custom_rx.recv() => {
                if let Some(msg) = outbound {
                    let frame = serialize_msg(
                        MessageType::Custom(msg.type_id),
                        &MessagePayload::Custom(msg.payload),
                        0,
                    )?;
                    writer.write_all(&frame).await.context("write custom")?;
                }
            }
        }
    }
}

// ── Helpers ───────────────────────────────────────────────────

/// Decide whether to send a chunk to this session.
#[inline]
fn should_send_chunk(
    chunk: &WireChunkData,
    routing: &SessionRouting,
    send_audio_to_muted: bool,
) -> bool {
    // Wrong stream → skip
    if chunk.stream_id != routing.stream_id {
        return false;
    }
    // Muted → skip (unless config says otherwise)
    if !send_audio_to_muted && (routing.client_muted || routing.group_muted) {
        return false;
    }
    true
}

async fn write_chunk<W: AsyncWriteExt + Unpin>(writer: &mut W, chunk: WireChunkData) -> Result<()> {
    let wc = WireChunk {
        timestamp: Timeval::from_usec(chunk.timestamp_usec),
        payload: chunk.data,
    };
    let frame = serialize_msg(MessageType::WireChunk, &MessagePayload::WireChunk(wc), 0)?;
    writer.write_all(&frame).await.context("write chunk")
}

async fn write_settings<W: AsyncWriteExt + Unpin>(
    writer: &mut W,
    update: ClientSettingsUpdate,
) -> Result<()> {
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
    tracing::debug!(
        volume = update.volume,
        latency = update.latency,
        "Pushed settings"
    );
    Ok(())
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
    Timeval::from_usec(now_usec())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chunk(stream_id: &str) -> WireChunkData {
        WireChunkData {
            stream_id: stream_id.to_string(),
            timestamp_usec: 0,
            data: vec![0u8; 64],
        }
    }

    fn routing(stream_id: &str, client_muted: bool, group_muted: bool) -> SessionRouting {
        SessionRouting {
            stream_id: stream_id.to_string(),
            client_muted,
            group_muted,
        }
    }

    #[test]
    fn matching_stream_unmuted_sends() {
        let r = routing("zone1", false, false);
        assert!(should_send_chunk(&chunk("zone1"), &r, false));
    }

    #[test]
    fn wrong_stream_skips() {
        let r = routing("zone1", false, false);
        assert!(!should_send_chunk(&chunk("zone2"), &r, false));
    }

    #[test]
    fn client_muted_skips() {
        let r = routing("zone1", true, false);
        assert!(!should_send_chunk(&chunk("zone1"), &r, false));
    }

    #[test]
    fn group_muted_skips() {
        let r = routing("zone1", false, true);
        assert!(!should_send_chunk(&chunk("zone1"), &r, false));
    }

    #[test]
    fn muted_but_send_audio_to_muted_sends() {
        let r = routing("zone1", true, false);
        assert!(should_send_chunk(&chunk("zone1"), &r, true));
    }

    #[test]
    fn group_muted_but_send_audio_to_muted_sends() {
        let r = routing("zone1", false, true);
        assert!(should_send_chunk(&chunk("zone1"), &r, true));
    }

    #[test]
    fn wrong_stream_always_skips_even_with_send_audio_to_muted() {
        let r = routing("zone1", false, false);
        assert!(!should_send_chunk(&chunk("zone2"), &r, true));
    }

    #[test]
    fn routing_watch_updates() {
        let r1 = routing("zone1", false, false);
        let r2 = routing("zone2", false, false);
        let (tx, rx) = watch::channel(r1.clone());
        assert_eq!(*rx.borrow(), r1);
        tx.send(r2.clone()).unwrap();
        assert_eq!(*rx.borrow(), r2);
    }

    #[test]
    fn unmute_restores_routing() {
        let muted = routing("zone1", true, false);
        assert!(!should_send_chunk(&chunk("zone1"), &muted, false));
        let unmuted = routing("zone1", false, false);
        assert!(should_send_chunk(&chunk("zone1"), &unmuted, false));
    }

    #[test]
    fn stream_switch_changes_filter() {
        let r1 = routing("zone1", false, false);
        assert!(should_send_chunk(&chunk("zone1"), &r1, false));
        assert!(!should_send_chunk(&chunk("zone2"), &r1, false));
        let r2 = routing("zone2", false, false);
        assert!(!should_send_chunk(&chunk("zone1"), &r2, false));
        assert!(should_send_chunk(&chunk("zone2"), &r2, false));
    }
}
