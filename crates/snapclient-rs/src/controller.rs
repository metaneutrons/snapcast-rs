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

use crate::config::ClientSettings;
use crate::connection::TcpConnection;
use crate::decoder::{self, Decoder, PcmDecoder};
use crate::player::{Player, Volume};
use crate::stream::{PcmChunk, Stream};
use crate::time_provider::TimeProvider;

#[cfg(feature = "coreaudio")]
use crate::player::coreaudio::CoreAudioPlayer;

const VERSION: &str = env!("CARGO_PKG_VERSION");

pub struct Controller {
    settings: ClientSettings,
    connection: TcpConnection,
    time_provider: Arc<Mutex<TimeProvider>>,
    stream: Option<Arc<Mutex<Stream>>>,
    decoder: Option<Box<dyn Decoder>>,
    player: Option<Box<dyn Player>>,
    sample_format: SampleFormat,
    server_settings: Option<ServerSettings>,
}

impl Controller {
    pub fn new(settings: ClientSettings) -> Self {
        Self {
            connection: TcpConnection::new(&settings.server.host, settings.server.port),
            settings,
            time_provider: Arc::new(Mutex::new(TimeProvider::new())),
            stream: None,
            decoder: None,
            player: None,
            sample_format: SampleFormat::default(),
            server_settings: None,
        }
    }

    pub async fn run(&mut self) -> Result<()> {
        loop {
            match self.session().await {
                Ok(()) => tracing::info!("Session ended"),
                Err(e) => tracing::error!("Session error: {e}"),
            }
            self.cleanup();
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
    }

    async fn session(&mut self) -> Result<()> {
        self.connection.connect().await?;
        tracing::info!(
            host = %self.settings.server.host,
            port = self.settings.server.port,
            "Connected"
        );

        self.send_hello().await?;
        self.initial_time_sync().await?;
        self.receive_loop().await
    }

    async fn send_hello(&mut self) -> Result<()> {
        let mac = "00:00:00:00:00:00".to_string();
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

        // Read response — may get ServerSettings or Error
        let response = self.recv_timeout(Duration::from_secs(5)).await?;
        match response.payload {
            MessagePayload::ServerSettings(ss) => {
                tracing::info!(
                    buffer_ms = ss.buffer_ms,
                    volume = ss.volume,
                    muted = ss.muted,
                    "ServerSettings received"
                );
                self.server_settings = Some(ss);
                Ok(())
            }
            MessagePayload::Error(e) => {
                bail!("Server rejected Hello: {} (code {})", e.error, e.code)
            }
            _ => bail!("Unexpected response to Hello: {:?}", response.base.msg_type),
        }
    }

    async fn initial_time_sync(&mut self) -> Result<()> {
        // Drain any pushed messages first (e.g. CodecHeader)
        self.drain_pushed_messages().await?;

        for i in 0..50 {
            self.do_time_sync().await?;
            if i < 49 {
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        }
        let diff = self.time_provider.lock().unwrap().diff_to_server_usec();
        tracing::info!(diff_ms = diff as f64 / 1000.0, "Time sync complete");
        Ok(())
    }

    async fn do_time_sync(&mut self) -> Result<()> {
        self.connection
            .send(MessageType::Time, &MessagePayload::Time(Time::new()))
            .await?;

        // Read messages until we get a Time response, handling others along the way
        loop {
            let msg = self.recv_timeout(Duration::from_secs(2)).await?;
            if let MessagePayload::Time(t) = msg.payload {
                let s2c = msg.base.received - msg.base.sent;
                self.time_provider
                    .lock()
                    .unwrap()
                    .set_diff(&t.latency, &s2c);
                return Ok(());
            }
            // Handle pushed messages (CodecHeader, ServerSettings) during sync
            self.handle_pushed(msg).await?;
        }
    }

    /// Drain any messages the server pushes after Hello (CodecHeader, etc.)
    async fn drain_pushed_messages(&mut self) -> Result<()> {
        loop {
            match tokio::time::timeout(Duration::from_millis(500), self.connection.recv()).await {
                Ok(Ok(msg)) => self.handle_pushed(msg).await?,
                _ => return Ok(()), // timeout or error — done draining
            }
        }
    }

    async fn handle_pushed(&mut self, msg: TypedMessage) -> Result<()> {
        match msg.payload {
            MessagePayload::CodecHeader(ch) => self.init_audio_pipeline(&ch)?,
            MessagePayload::ServerSettings(ss) => {
                tracing::info!(
                    volume = ss.volume,
                    muted = ss.muted,
                    "ServerSettings update"
                );
                self.apply_server_settings(&ss);
                self.server_settings = Some(ss);
            }
            MessagePayload::Error(e) => {
                tracing::error!(code = e.code, error = %e.error, message = %e.message, "Server error");
            }
            _ => {
                tracing::debug!(msg_type = ?msg.base.msg_type, "Skipping message during sync");
            }
        }
        Ok(())
    }

    async fn receive_loop(&mut self) -> Result<()> {
        let mut sync_timer = tokio::time::interval(Duration::from_secs(1));

        loop {
            tokio::select! {
                msg = self.connection.recv() => {
                    let msg = msg?;
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
                            tracing::info!(volume = ss.volume, muted = ss.muted, "ServerSettings update");
                            self.apply_server_settings(&ss);
                            self.server_settings = Some(ss);
                        }
                        MessagePayload::CodecHeader(ch) => {
                            self.init_audio_pipeline(&ch)?;
                        }
                        MessagePayload::Time(t) => {
                            let s2c = msg.base.received - msg.base.sent;
                            self.time_provider.lock().unwrap().set_diff(&t.latency, &s2c);
                        }
                        MessagePayload::Error(e) => {
                            tracing::error!(code = e.code, error = %e.error, "Server error");
                        }
                        _ => {}
                    }
                }
                _ = sync_timer.tick() => {
                    // Send time sync — response will be handled in the recv branch
                    self.connection
                        .send(MessageType::Time, &MessagePayload::Time(Time::new()))
                        .await
                        .ok();
                }
            }
        }
    }

    fn apply_server_settings(&mut self, ss: &ServerSettings) {
        if let Some(ref mut p) = self.player {
            p.set_volume(Volume {
                volume: ss.volume as f64 / 100.0,
                muted: ss.muted,
            });
        }
        if let Some(ref stream) = self.stream {
            let buf_ms = (ss.buffer_ms - ss.latency - self.settings.player.latency).max(0);
            stream.lock().unwrap().set_buffer_ms(buf_ms as i64);
        }
    }

    fn init_audio_pipeline(&mut self, header: &CodecHeader) -> Result<()> {
        if let Some(ref mut p) = self.player {
            let _ = p.stop();
        }
        self.player = None;
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

        let stream = Arc::new(Mutex::new(Stream::new(self.sample_format)));
        if let Some(ref ss) = self.server_settings {
            let buf_ms = (ss.buffer_ms - ss.latency - self.settings.player.latency).max(0);
            stream.lock().unwrap().set_buffer_ms(buf_ms as i64);
        }
        self.stream = Some(Arc::clone(&stream));

        #[cfg(feature = "coreaudio")]
        let mut player: Box<dyn Player> = Box::new(CoreAudioPlayer::new(
            stream,
            Arc::clone(&self.time_provider),
            self.sample_format,
        ));

        #[cfg(not(feature = "coreaudio"))]
        bail!("no audio backend available");

        if let Some(ref ss) = self.server_settings {
            player.set_volume(Volume {
                volume: ss.volume as f64 / 100.0,
                muted: ss.muted,
            });
        }

        player.start()?;
        self.player = Some(player);
        self.decoder = Some(dec);
        Ok(())
    }

    async fn recv_timeout(&mut self, timeout: Duration) -> Result<TypedMessage> {
        tokio::time::timeout(timeout, self.connection.recv())
            .await
            .map_err(|_| anyhow::anyhow!("receive timed out"))?
    }

    fn cleanup(&mut self) {
        if let Some(ref mut p) = self.player {
            let _ = p.stop();
        }
        self.player = None;
        self.stream = None;
        self.decoder = None;
        self.connection.disconnect();
    }
}

fn hostname() -> String {
    hostname::get()
        .map(|h| h.to_string_lossy().into_owned())
        .unwrap_or_else(|_| "unknown".to_string())
}
