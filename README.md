# snapcast-rs

Rust implementation of [Snapcast](https://github.com/snapcast/snapcast) — synchronized multiroom audio.

100% pure Rust by default. Cross-platform: macOS, Linux, Windows.

## Architecture

```
snapcast-rs/
├── snapcast-proto      Protocol: binary message serialization (8 message types)
├── snapcast-client     Client library: embeddable, f32 audio output
├── snapcast-server     Server library: embeddable, f32 audio input
├── snapclient-rs       Client binary: cpal audio output
└── snapserver-rs       Server binary: stream readers, JSON-RPC, HTTP
```

Both libraries are pure audio engines — no device I/O, no HTTP, no config files.

## Client Library API

```rust
use snapcast_client::{SnapClient, ClientConfig, ClientEvent, ClientCommand, AudioFrame};

// Create client — returns event receiver and audio output receiver
let (mut client, events, audio_rx) = SnapClient::new(config);

// Shared state for direct audio device access
let stream = Arc::clone(&client.stream);           // time-synced PCM buffer
let time_provider = Arc::clone(&client.time_provider); // server clock sync

// Run (blocks, reconnects on error)
tokio::spawn(async move { client.run().await });

// Events
match event {
    ClientEvent::Connected { host, port } => {}
    ClientEvent::Disconnected { reason } => {}
    ClientEvent::StreamStarted { codec, format } => {}
    ClientEvent::ServerSettings { buffer_ms, latency, volume, muted } => {}
    ClientEvent::VolumeChanged { volume, muted } => {}
    ClientEvent::TimeSyncComplete { diff_ms } => {}
    ClientEvent::JsonRpc(value) => {}
}

// Commands
cmd.send(ClientCommand::SetVolume { volume: 80, muted: false }).await;
cmd.send(ClientCommand::Stop).await;
```

### Client Features

| Feature     | Default | C dep | Description |
|-------------|---------|-------|-------------|
| `f32lz4`    | ✅      | none  | f32 LZ4 codec (lz4_flex) |
| `mdns`      | ✅      | none  | mDNS server discovery |
| `websocket` | —       | none  | WebSocket connection |
| `tls`       | —       | none  | WSS (WebSocket + TLS) |
| `resampler` | —       | none  | Sample rate conversion (rubato) |

## Server Library API

```rust
use snapcast_server::{SnapServer, ServerConfig, ServerEvent, ServerCommand, AudioFrame};

// Create server — returns event receiver and audio input sender
let (mut server, events, audio_tx) = SnapServer::new(config);

// Configure stream manager (binary creates readers, passes to library)
let mut manager = StreamManager::new();
manager.add_stream_from_receiver("music", format, "flac", "", rx)?;
server.set_manager(manager);

// Register custom JSON-RPC methods (binary handles dispatch)
// Library handles: session protocol, encoding, mDNS, state

// Run (blocks until Stop)
tokio::spawn(async move { server.run().await });

// Typed commands (no JSON-RPC detour)
cmd.send(ServerCommand::SetClientVolume { client_id, volume: 80, muted: false }).await;
cmd.send(ServerCommand::SetClientLatency { client_id, latency: 50 }).await;
cmd.send(ServerCommand::SetClientName { client_id, name }).await;
cmd.send(ServerCommand::SetGroupStream { group_id, stream_id }).await;
cmd.send(ServerCommand::SetGroupMute { group_id, muted: true }).await;
cmd.send(ServerCommand::SetGroupName { group_id, name }).await;
cmd.send(ServerCommand::SetGroupClients { group_id, clients: vec![...] }).await;
cmd.send(ServerCommand::DeleteClient { client_id }).await;
let (tx, rx) = oneshot::channel();
cmd.send(ServerCommand::GetStatus { response_tx: tx }).await;
let status = rx.await?;

// Push f32 audio directly (alternative to stream readers)
audio_tx.send(AudioFrame { samples, sample_rate: 48000, channels: 2, timestamp_usec }).await;

// Reactive events
match event {
    ServerEvent::ClientConnected { id, name } => {}
    ServerEvent::ClientDisconnected { id } => {}
    ServerEvent::ClientVolumeChanged { client_id, volume, muted } => {}
    ServerEvent::ClientLatencyChanged { client_id, latency } => {}
    ServerEvent::ClientNameChanged { client_id, name } => {}
    ServerEvent::GroupStreamChanged { group_id, stream_id } => {}
    ServerEvent::GroupMuteChanged { group_id, muted } => {}
    ServerEvent::StreamStatus { stream_id, status } => {}
    ServerEvent::JsonRpc { client_id, request, response_tx } => {
        // Custom method — respond via oneshot
        if let Some(tx) = response_tx {
            tx.send(json!({"jsonrpc": "2.0", "id": request["id"], "result": "ok"})).ok();
        }
    }
}
```

### Server Features

| Feature  | Default | C dep     | Description |
|----------|---------|-----------|-------------|
| `f32lz4` | ✅      | none      | f32 LZ4 codec (lz4_flex) |
| `mdns`   | ✅      | none      | mDNS service advertisement |
| `flac`   | —       | libFLAC   | FLAC encoding |
| `opus`   | —       | libopus   | Opus encoding |
| `vorbis` | —       | libvorbis | Vorbis encoding |

### Network Ports

| Port | Protocol | Owner | Purpose |
|------|----------|-------|---------|
| 1704 | TCP | Library | Binary protocol (audio + time sync) |
| 1705 | TCP | Binary | JSON-RPC control |
| 1780 | HTTP/WS | Binary | JSON-RPC + Snapweb UI |

Libraries open only port 1704. JSON-RPC/HTTP are binary-only.

## Codecs

| Codec  | Default | C dep | Precision | Latency |
|--------|---------|-------|-----------|---------|
| PCM    | ✅ always | none | 16/24/32-bit | zero |
| f32lz4 | ✅ default | none | 32-bit float | zero |
| FLAC   | optional | libFLAC | 16/24-bit | 24ms (block size) |
| Opus   | optional | libopus | lossy | 20ms |
| Vorbis | optional | libvorbis | lossy | variable |

f32lz4 path (zero conversion, full precision):
```
f32 → LZ4 compress → network → LZ4 decompress → f32
```

## Building

```bash
cargo build --release                                    # default: f32lz4 + mdns
cargo build --release --features flac                    # + FLAC
cargo build --release --no-default-features --features f32lz4  # minimal, no mdns
```

## Usage

```bash
# Server
snapserver-rs --source "pipe:///tmp/snapfifo?name=Music"
snapserver-rs --codec flac --features flac
snapserver-rs --help

# Client
snapclient-rs tcp://192.168.1.50:1704
snapclient-rs                            # mDNS auto-discovery
snapclient-rs --help

# Feed audio
ffmpeg -re -i music.mp3 -f s16le -ar 48000 -ac 2 pipe:1 > /tmp/snapfifo
```

## Code Quality

- `#![forbid(unsafe_code)]` on protocol crate
- `#![deny(unsafe_code)]` on client/server libraries
- Zero `#[allow(dead_code)]`, zero TODOs, zero `#![allow]` blankets
- Structured tracing logging
- sccache enabled

## Releases

Pre-built binaries for every release:

| Platform | Client | Server | FLAC |
|----------|--------|--------|------|
| Linux x86_64 | ✅ | ✅ | ✅ |
| Linux aarch64 | ✅ | ✅ | ✅ |
| macOS x86_64 | ✅ | ✅ | ✅ |
| macOS aarch64 | ✅ | ✅ | ✅ |
| Windows x86_64 | ✅ | ✅ | — |

Download from [GitHub Releases](https://github.com/metaneutrons/snapcast-rs/releases).

Library crates published to [crates.io](https://crates.io): `snapcast-proto`, `snapcast-client`, `snapcast-server`.

## License

GPL-3.0-only — same as the original Snapcast.
