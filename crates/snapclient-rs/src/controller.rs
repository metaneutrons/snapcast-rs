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
const TIME_SYNC_INTERVAL: Duration = Duration::from_secs(1);
const QUICK_SYNC_INTERVAL: Duration = Duration::from_millis(100);
const QUICK_SYNC_COUNT: u32 = 50;

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

    /// Main run loop with automatic reconnection.
    pub async fn run(&mut self) -> Result<()> {
        loop {
            match self.session().await {
                Ok(()) => tracing::info!("Session ended cleanly"),
                Err(e) => tracing::error!("Session error: {e}"),
            }
            self.cleanup();
            tracing::info!("Reconnecting in 1s...");
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

        let response = self
            .connection
            .send_request(
                MessageType::Hello,
                &MessagePayload::Hello(hello),
                Duration::from_secs(2),
            )
            .await?;

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
            _ => bail!("Unexpected response to Hello"),
        }
    }

    async fn initial_time_sync(&mut self) -> Result<()> {
        for i in 0..QUICK_SYNC_COUNT {
            self.do_time_sync().await?;
            if i + 1 < QUICK_SYNC_COUNT {
                tokio::time::sleep(QUICK_SYNC_INTERVAL).await;
            }
        }
        let diff = self.time_provider.lock().unwrap().diff_to_server_usec();
        tracing::info!(diff_ms = diff as f64 / 1000.0, "Time sync complete");
        Ok(())
    }

    async fn do_time_sync(&mut self) -> Result<()> {
        let time_msg = Time::new();
        let response = self
            .connection
            .send_request(
                MessageType::Time,
                &MessagePayload::Time(time_msg),
                Duration::from_secs(2),
            )
            .await?;

        if let MessagePayload::Time(t) = response.payload {
            let s2c = response.base.received - response.base.sent;
            self.time_provider
                .lock()
                .unwrap()
                .set_diff(&t.latency, &s2c);
        }
        Ok(())
    }

    async fn receive_loop(&mut self) -> Result<()> {
        let mut sync_interval = tokio::time::interval(TIME_SYNC_INTERVAL);

        loop {
            tokio::select! {
                msg = self.connection.recv() => {
                    self.handle_message(msg?).await?;
                }
                _ = sync_interval.tick() => {
                    self.do_time_sync().await.ok();
                }
            }
        }
    }

    async fn handle_message(&mut self, msg: TypedMessage) -> Result<()> {
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
                tracing::info!(
                    volume = ss.volume,
                    muted = ss.muted,
                    "ServerSettings update"
                );
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
                self.server_settings = Some(ss);
            }
            MessagePayload::CodecHeader(ch) => {
                self.init_audio_pipeline(&ch)?;
            }
            MessagePayload::Error(e) => {
                tracing::error!(code = e.code, error = %e.error, message = %e.message, "Server error");
            }
            _ => {
                tracing::warn!(msg_type = ?msg.base.msg_type, "Unexpected message");
            }
        }
        Ok(())
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
