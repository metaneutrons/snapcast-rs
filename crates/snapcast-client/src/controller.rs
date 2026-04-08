//! Controller — main orchestrator that wires connection, decoder, stream, and player.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Result, bail};
use snapcast_proto::message::codec_header::CodecHeader;
use snapcast_proto::message::factory::{MessagePayload, TypedMessage};
use snapcast_proto::message::hello::{Auth, Hello};
use snapcast_proto::message::server_settings::ServerSettings;
use snapcast_proto::message::time::Time;
use snapcast_proto::{MessageType, SampleFormat};
use tokio::sync::mpsc;

use crate::config::ClientSettings;
use crate::connection::TcpConnection;
use crate::decoder::{self, Decoder, PcmDecoder};
use crate::stream::{PcmChunk, Stream};
use crate::time_provider::TimeProvider;
use crate::{ClientCommand, ClientEvent};

const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Main orchestrator wiring connection, decoder, stream, and audio output.
pub struct Controller {
    settings: ClientSettings,
    connection: TcpConnection,
    time_provider: Arc<Mutex<TimeProvider>>,
    stream: Option<Arc<Mutex<Stream>>>,
    decoder: Option<Box<dyn Decoder>>,
    sample_format: SampleFormat,
    server_settings: Option<ServerSettings>,
    event_tx: mpsc::Sender<ClientEvent>,
    command_rx: mpsc::Receiver<ClientCommand>,
    audio_tx: mpsc::Sender<crate::AudioFrame>,
    audio_pump_handle: Option<tokio::task::JoinHandle<()>>,
}

impl Controller {
    /// Create a new controller with the given settings and event channels.
    pub fn new(
        settings: ClientSettings,
        event_tx: mpsc::Sender<ClientEvent>,
        command_rx: mpsc::Receiver<ClientCommand>,
        #[allow(dead_code)] // TODO: wire in step 1
        audio_tx: mpsc::Sender<crate::AudioFrame>,
    ) -> Self {
        Self {
            connection: TcpConnection::new(&settings.server.host, settings.server.port),
            settings,
            time_provider: Arc::new(Mutex::new(TimeProvider::new())),
            stream: None,
            decoder: None,
            sample_format: SampleFormat::default(),
            server_settings: None,
            event_tx,
            command_rx,
            audio_tx,
            audio_pump_handle: None,
        }
    }

    /// Run the client, reconnecting on errors until stopped.
    pub async fn run(&mut self) -> Result<()> {
        let mut attempts = 0u32;
        loop {
            match self.session().await {
                Ok(()) => {
                    self.cleanup();
                    return Ok(());
                }
                Err(e) => {
                    if attempts == 0 {
                        tracing::warn!("Connection failed: {e}");
                    } else {
                        tracing::debug!("Reconnect attempt {attempts} failed: {e}");
                    }
                    self.emit(ClientEvent::Disconnected {
                        reason: e.to_string(),
                    });
                    attempts = attempts.saturating_add(1);
                }
            }
            self.cleanup();
            let delay = Duration::from_secs(attempts.min(30) as u64);
            tokio::time::sleep(delay).await;
        }
    }

    async fn session(&mut self) -> Result<()> {
        // mDNS discovery if host is empty or starts with "_"
        if self.settings.server.host.is_empty() || self.settings.server.host.starts_with('_') {
            #[cfg(feature = "mdns")]
            {
                tracing::info!(service = %self.settings.server.host, "Browsing mDNS...");
                let endpoint = crate::discovery::discover(Duration::from_secs(5)).await?;
                self.settings.server.host = endpoint.host;
                self.settings.server.port = endpoint.port;
                self.connection =
                    TcpConnection::new(&self.settings.server.host, self.settings.server.port);
            }
            #[cfg(not(feature = "mdns"))]
            bail!("mDNS not available — specify server URL");
        }

        self.connection.connect().await?;
        tracing::info!(
            host = %self.settings.server.host,
            port = self.settings.server.port,
            "Connected"
        );
        self.emit(ClientEvent::Connected {
            host: self.settings.server.host.clone(),
            port: self.settings.server.port,
        });

        self.send_hello().await?;
        self.receive_loop().await
    }

    async fn send_hello(&mut self) -> Result<()> {
        let mac = get_mac_address();
        let host_id = if self.settings.host_id.is_empty() {
            mac.clone()
        } else {
            self.settings.host_id.clone()
        };

        let auth = self.settings.server.auth.as_ref().map(|a| Auth {
            scheme: a.scheme.clone(),
            param: a.param.clone(),
        });

        let hello = Hello {
            mac: mac.clone(),
            host_name: hostname(),
            version: VERSION.to_string(),
            client_name: "Snapclient".to_string(),
            os: std::env::consts::OS.to_string(),
            arch: std::env::consts::ARCH.to_string(),
            instance: self.settings.instance,
            id: host_id,
            snap_stream_protocol_version: 2,
            auth,
        };

        self.connection
            .send(MessageType::Hello, &MessagePayload::Hello(hello))
            .await?;

        let response = self.recv_timeout(Duration::from_secs(5)).await?;
        match response.payload {
            MessagePayload::ServerSettings(ss) => {
                self.emit(ClientEvent::ServerSettings {
                    buffer_ms: ss.buffer_ms,
                    latency: ss.latency,
                    volume: ss.volume,
                    muted: ss.muted,
                });
                self.server_settings = Some(ss);
                Ok(())
            }
            MessagePayload::Error(e) => {
                bail!("Server rejected Hello: {} (code {})", e.error, e.code)
            }
            _ => bail!("Unexpected response to Hello: {:?}", response.base.msg_type),
        }
    }

    async fn receive_loop(&mut self) -> Result<()> {
        let mut sync_timer = tokio::time::interval(Duration::from_secs(1));
        let mut quick_syncs_remaining: u32 = 50;
        let mut quick_sync_timer = tokio::time::interval(Duration::from_millis(100));

        self.connection
            .send(MessageType::Time, &MessagePayload::Time(Time::new()))
            .await
            .ok();

        loop {
            tokio::select! {
                msg = self.connection.recv() => {
                    let msg = msg?;
                    self.handle_message(msg)?;
                }
                cmd = self.command_rx.recv() => {
                    match cmd {
                        Some(ClientCommand::Stop) | None => {
                            tracing::info!("Stop command received");
                            return Ok(());
                        }
                        Some(ClientCommand::SetVolume { volume, muted }) => {
                            tracing::debug!(volume, muted, "Volume change (applied by binary)");
                        }
                        Some(ClientCommand::SendJsonRpc(_msg)) => {
                            // JSON-RPC is handled server-side; client binary protocol
                            // does not support JSON-RPC. This is reserved for future use.
                        }
                    }
                }
                _ = quick_sync_timer.tick(), if quick_syncs_remaining > 0 => {
                    quick_syncs_remaining -= 1;
                    self.connection
                        .send(MessageType::Time, &MessagePayload::Time(Time::new()))
                        .await
                        .ok();
                    if quick_syncs_remaining == 0 {
                        let diff = self.time_provider.lock().unwrap().diff_to_server_usec();
                        let diff_ms = diff as f64 / 1000.0;
                        tracing::info!(diff_ms, "Time sync complete");
                        self.emit(ClientEvent::TimeSyncComplete { diff_ms });
                    }
                }
                _ = sync_timer.tick(), if quick_syncs_remaining == 0 => {
                    self.connection
                        .send(MessageType::Time, &MessagePayload::Time(Time::new()))
                        .await
                        .ok();
                }
            }
        }
    }

    fn handle_message(&mut self, msg: TypedMessage) -> Result<()> {
        match msg.payload {
            MessagePayload::WireChunk(wc) => {
                if let Some(ref mut dec) = self.decoder {
                    let mut data = wc.payload;
                    if dec.decode(&mut data)? {
                        let chunk = PcmChunk::new(wc.timestamp, data, self.sample_format);
                        if let Some(ref stream) = self.stream {
                            stream.lock().unwrap().add_chunk(chunk);
                        }
                    }
                }
            }
            MessagePayload::ServerSettings(ss) => {
                self.emit(ClientEvent::ServerSettings {
                    buffer_ms: ss.buffer_ms,
                    latency: ss.latency,
                    volume: ss.volume,
                    muted: ss.muted,
                });
                self.emit(ClientEvent::VolumeChanged {
                    volume: ss.volume,
                    muted: ss.muted,
                });
                self.apply_server_settings(&ss);
                self.server_settings = Some(ss);
            }
            MessagePayload::CodecHeader(ch) => {
                self.init_audio_pipeline(&ch)?;
            }
            MessagePayload::Time(t) => {
                let s2c = msg.base.received - msg.base.sent;
                self.time_provider
                    .lock()
                    .unwrap()
                    .set_diff(&t.latency, &s2c);
            }
            MessagePayload::Error(e) => {
                tracing::error!(code = e.code, error = %e.error, "Server error");
            }
            _ => {}
        }
        Ok(())
    }

    fn apply_server_settings(&mut self, ss: &ServerSettings) {
        if let Some(ref stream) = self.stream {
            let buf_ms = (ss.buffer_ms - ss.latency - self.settings.player.latency).max(0);
            stream.lock().unwrap().set_buffer_ms(buf_ms as i64);
        }
    }

    fn init_audio_pipeline(&mut self, header: &CodecHeader) -> Result<()> {
        self.stream = None;

        let mut dec: Box<dyn Decoder> = match header.codec.as_str() {
            "pcm" => Box::new(PcmDecoder::new()),
            "flac" => Box::new(decoder::flac::create(header)?),
            "ogg" => Box::new(decoder::vorbis::create(header)?),
            "opus" => Box::new(decoder::opus::create(header)?),
            other => bail!("unsupported codec: {other}"),
        };

        self.sample_format = dec.set_header(header)?;
        tracing::info!(codec = %header.codec, format = %self.sample_format, "Codec initialized");

        self.emit(ClientEvent::StreamStarted {
            codec: header.codec.clone(),
            format: self.sample_format,
        });

        let stream = Arc::new(Mutex::new(Stream::new(self.sample_format)));
        if let Some(ref ss) = self.server_settings {
            let buf_ms = (ss.buffer_ms - ss.latency - self.settings.player.latency).max(0);
            stream.lock().unwrap().set_buffer_ms(buf_ms as i64);
        }
        self.stream = Some(Arc::clone(&stream));

        // Start audio pump — reads time-synced PCM from Stream, converts to f32, sends via channel
        if let Some(handle) = self.audio_pump_handle.take() {
            handle.abort();
        }
        let pump_stream = Arc::clone(&stream);
        let pump_tp = Arc::clone(&self.time_provider);
        let pump_tx = self.audio_tx.clone();
        let pump_format = self.sample_format;
        let pump_handle = tokio::task::spawn_blocking(move || {
            audio_pump(pump_stream, pump_tp, pump_tx, pump_format);
        });
        self.audio_pump_handle = Some(pump_handle);

        self.decoder = Some(dec);
        Ok(())
    }

    async fn recv_timeout(&mut self, timeout: Duration) -> Result<TypedMessage> {
        tokio::time::timeout(timeout, self.connection.recv())
            .await
            .map_err(|_| anyhow::anyhow!("receive timed out"))?
    }

    fn cleanup(&mut self) {
        if let Some(handle) = self.audio_pump_handle.take() {
            handle.abort();
        }
        self.stream = None;
        self.decoder = None;
        self.connection.disconnect();
    }

    fn emit(&self, event: ClientEvent) {
        let _ = self.event_tx.try_send(event);
    }

    /// Graceful shutdown — stops player and closes connection.
    pub fn shutdown(&mut self) {
        tracing::info!("Shutting down");
        self.cleanup();
    }
}

/// Timer-driven audio pump: reads time-synced PCM from Stream, converts to f32, sends via channel.
fn audio_pump(
    stream: Arc<Mutex<Stream>>,
    time_provider: Arc<Mutex<TimeProvider>>,
    tx: mpsc::Sender<crate::AudioFrame>,
    format: SampleFormat,
) {
    let frame_size = format.frame_size() as usize;
    let channels = format.channels() as usize;
    let sample_size = format.sample_size() as usize;
    // ~5ms chunks (matching CoreAudio callback size)
    let num_frames = (format.rate() as usize) / 200;
    let buf_bytes = num_frames * frame_size;
    let chunk_duration =
        std::time::Duration::from_micros((num_frames as u64 * 1_000_000) / format.rate() as u64);
    // DAC delay estimate for the audio pump (no real device, so use buffer duration)
    let dac_usec = chunk_duration.as_micros() as i64;

    let mut pcm_buf = vec![0u8; buf_bytes];

    loop {
        let start = std::time::Instant::now();

        let server_now = {
            let tp = time_provider.lock().unwrap();
            crate::connection::now_usec() + tp.diff_to_server_usec()
        };

        {
            let mut s = stream.lock().unwrap();
            s.get_player_chunk_or_silence(server_now, dac_usec, &mut pcm_buf, num_frames as u32);
        }

        // Convert PCM bytes to interleaved f32
        let total_samples = num_frames * channels;
        let mut samples = Vec::with_capacity(total_samples);
        match sample_size {
            2 => {
                for chunk in pcm_buf.chunks_exact(2) {
                    let s = i16::from_le_bytes([chunk[0], chunk[1]]);
                    samples.push(s as f32 / i16::MAX as f32);
                }
            }
            4 => {
                for chunk in pcm_buf.chunks_exact(4) {
                    let s = i32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
                    samples.push(s as f32 / i32::MAX as f32);
                }
            }
            _ => {}
        }

        let frame = crate::AudioFrame {
            samples,
            sample_rate: format.rate(),
            channels: format.channels(),
            timestamp_usec: server_now,
        };

        if tx.blocking_send(frame).is_err() {
            break; // receiver dropped
        }

        // Sleep for remaining chunk duration
        let elapsed = start.elapsed();
        if elapsed < chunk_duration {
            std::thread::sleep(chunk_duration - elapsed);
        }
    }
}

fn hostname() -> String {
    hostname::get()
        .map(|h| h.to_string_lossy().into_owned())
        .unwrap_or_else(|_| "unknown".to_string())
}

fn get_mac_address() -> String {
    mac_address::get_mac_address()
        .ok()
        .flatten()
        .map(|mac| mac.to_string().to_lowercase())
        .unwrap_or_else(|| "00:00:00:00:00:00".to_string())
}
