# snapcast-rs

[![CI](https://github.com/metaneutrons/snapcast-rs/actions/workflows/ci.yml/badge.svg)](https://github.com/metaneutrons/snapcast-rs/actions/workflows/ci.yml)
[![crates.io](https://img.shields.io/crates/v/snapcast-client.svg)](https://crates.io/crates/snapcast-client)
[![crates.io](https://img.shields.io/crates/v/snapcast-server.svg)](https://crates.io/crates/snapcast-server)
[![docs.rs](https://docs.rs/snapcast-client/badge.svg)](https://docs.rs/snapcast-client)
[![docs.rs](https://docs.rs/snapcast-server/badge.svg)](https://docs.rs/snapcast-server)
[![License: GPL-3.0](https://img.shields.io/badge/license-GPL--3.0-blue.svg)](LICENSE)

> **⚠️ Pre-1.0 — APIs may break on minor version bumps.** Until version 1.0, minor releases (e.g. 0.3 → 0.4) may contain breaking changes to the public API. Pin your dependency to a specific minor version if you need stability.

A Rust reimplementation of [Snapcast](https://github.com/snapcast/snapcast), the excellent multiroom audio system created by [Johannes Pohl (badaix)](https://github.com/badaix). Snapcast synchronizes audio playback across multiple devices with sub-millisecond precision — turning any collection of speakers into a perfectly synced whole-home audio system.

This project exists primarily to serve as a native Rust dependency for [SnapDog](https://github.com/metaneutrons/SnapDogRust), a multiroom audio appliance. Rather than shelling out to C++ binaries or bridging through FFI, SnapDog embeds the Snapcast protocol directly as a library — receiving audio, encoding it, distributing it to clients, and controlling playback, all within a single Rust process.

To make this possible, snapcast-rs separates the protocol engine from the application shell. The **library crates** (`snapcast-client`, `snapcast-server`) implement the Snapcast binary protocol, audio encoding/decoding, time synchronization, and mDNS discovery — but own no audio devices, open no HTTP ports, and read no config files. They communicate exclusively through typed Rust channels, making them straightforward to embed in any application.

The **binary crates** (`snapclient-rs`, `snapserver-rs`) are thin wrappers around these libraries. They add the things a standalone application needs: reading audio from pipes and processes, serving the JSON-RPC control API over HTTP and TCP, hosting the Snapweb UI, and outputting audio through platform-native backends via cpal. They are fully functional replacements for the original C++ `snapserver` and `snapclient`.

The result is a Snapcast implementation that works both as a drop-in replacement for the original and as an embeddable building block for Rust applications that need synchronized multiroom audio.


snapcast-rs is fully compatible with the original C++ Snapcast when using standard codecs (PCM, FLAC, Opus, Vorbis). However, three optional features break compatibility:

| Feature | What it does | C++ behavior |
|---------|-------------|--------------|
| `f32lz4` | 32-bit float LZ4 codec | C++ clients reject unknown codec |
| `custom-protocol` | Application-defined message types (9+) | C++ clients silently ignore |
| `encryption` | ChaCha20-Poly1305 encrypted f32lz4 | C++ clients reject unknown codec |

If you enable `f32lz4` or `encryption` on the server, C++ clients cannot decode the audio. To prevent them from auto-connecting via mDNS, change the service type:

```rust
let config = ServerConfig {
    mdns_service_type: "_myapp._tcp.local.".into(),
    ..ServerConfig::default()
};
```

For full interoperability with C++ clients, use `--codec flac` or `--codec pcm` and leave `custom-protocol` and `encryption` disabled.

## Architecture

```
snapcast-rs/
├── snapcast-proto      Protocol: binary message serialization
├── snapcast-client     Client library: embeddable, f32 audio output
├── snapcast-server     Server library: embeddable, f32 audio input
├── snapclient-rs       Client binary: cpal audio output
├── snapserver-rs       Server binary: stream readers, JSON-RPC, HTTP
└── snapcast-tests      Integration tests
```

| Crate | Role | Docs |
|-------|------|------|
| [snapcast-proto](crates/snapcast-proto) | Binary protocol, message serialization | [docs.rs](https://docs.rs/snapcast-proto) |
| [snapcast-client](crates/snapcast-client) | Client library: embeddable, f32 audio output | [docs.rs](https://docs.rs/snapcast-client) |
| [snapcast-server](crates/snapcast-server) | Server library: embeddable, f32 audio input | [docs.rs](https://docs.rs/snapcast-server) |
| [snapclient-rs](crates/snapclient-rs) | Client binary: cpal audio output | — |
| [snapserver-rs](crates/snapserver-rs) | Server binary: stream readers, JSON-RPC, HTTP | — |
| [snapcast-tests](crates/snapcast-tests) | Integration tests | — |

Both libraries are pure audio engines — no device I/O, no HTTP, no config files.

## Client Library API

```rust
use snapcast_client::{SnapClient, ClientConfig, ClientEvent, ClientCommand};

let config = ClientConfig {
    host: "192.168.1.50".into(),
    port: 1704,
    ..ClientConfig::default()
};

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
}

// Commands
cmd.send(ClientCommand::SetVolume { volume: 80, muted: false }).await;
cmd.send(ClientCommand::Stop).await;
```

### Client Config

```rust
ClientConfig {
    scheme: String,            // "tcp", "ws", or "wss" (default: "tcp")
    host: String,              // server host (empty = mDNS discovery)
    port: u16,                 // default: 1704
    auth: Option<Auth>,        // Basic auth for Hello handshake
    server_certificate: Option<PathBuf>, // CA cert for TLS verification
    certificate: Option<PathBuf>,        // client cert (PEM)
    certificate_key: Option<PathBuf>,    // client key (PEM)
    key_password: Option<String>,        // key password
    instance: u32,             // for multiple clients on one host
    host_id: String,           // unique identifier (default: MAC)
    latency: i32,              // additional latency offset (ms)
    mdns_service_type: String, // default: "_snapcast._tcp.local."
    client_name: String,       // default: "Snapclient"
    encryption_psk: Option<String>, // f32lz4 encryption (feature: encryption)
}
```

### Client Features

| Feature     | Default | C dep | Description |
|-------------|---------|-------|-------------|
| `f32lz4`    | —       | none  | f32 LZ4 codec (lz4_flex) |
| `mdns`      | ✅      | none  | mDNS server discovery |
| `websocket` | —       | none  | WebSocket connection |
| `tls`       | —       | none  | WSS (WebSocket + TLS) |
| `resampler` | —       | none  | Sample rate conversion (rubato) |
| `custom-protocol` | — | none | Custom binary messages (type 9+) |
| `encryption` | — | none | ChaCha20-Poly1305 encrypted f32lz4 |

## Server Library API

```rust
use snapcast_server::{SnapServer, ServerConfig, ServerEvent, ServerCommand, AudioFrame, StreamConfig};

let config = ServerConfig {
    codec: "flac".into(),
    auth: Some(Arc::new(StaticAuthValidator::new(users, roles))),
    ..ServerConfig::default()
};

// Create server — returns event receiver
let (mut server, events) = SnapServer::new(config);

// Add audio streams (each gets its own encoder)
let audio_tx = server.add_stream("default");

// Per-stream codec override
let zone2_tx = server.add_stream_with_config("Zone2", StreamConfig {
    codec: Some("f32lz4".into()),
    ..Default::default()
});

// Run (blocks until Stop)
tokio::spawn(async move { server.run().await });

// Typed commands
cmd.send(ServerCommand::SetClientVolume { client_id, volume: 80, muted: false }).await;
cmd.send(ServerCommand::SetClientLatency { client_id, latency: 50 }).await;
cmd.send(ServerCommand::SetClientName { client_id, name }).await;
cmd.send(ServerCommand::SetGroupStream { group_id, stream_id }).await;
cmd.send(ServerCommand::SetGroupMute { group_id, muted: true }).await;
cmd.send(ServerCommand::DeleteClient { client_id }).await;
let (tx, rx) = oneshot::channel();
cmd.send(ServerCommand::GetStatus { response_tx: tx }).await;

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
}
```

### Server Config

```rust
ServerConfig {
    stream_port: u16,          // default: 1704
    buffer_ms: u32,            // default: 1000
    codec: String,             // default: "flac" (feature-dependent: flac > f32lz4 > pcm)
    sample_format: String,     // default: "48000:16:2"
    mdns_service_type: String, // default: "_snapcast._tcp.local."
    auth: Option<Arc<dyn AuthValidator>>, // default: None (no auth)
    encryption_psk: Option<String>, // f32lz4 encryption (feature: encryption)
}
```

### Per-Stream Config

```rust
StreamConfig {
    codec: Option<String>,         // override server codec (e.g. "f32lz4", "flac")
    sample_format: Option<String>, // override server format (e.g. "48000:32:2")
}
```

### Server Features

| Feature  | Default | C dep     | Description |
|----------|---------|-----------|-------------|
| `f32lz4` | —       | none      | f32 LZ4 codec (lz4_flex) |
| `mdns`   | ✅      | none      | mDNS service advertisement |
| `flac`   | ✅      | libFLAC   | FLAC encoding |
| `opus`   | —       | libopus   | Opus encoding |
| `vorbis` | —       | libvorbis | Vorbis encoding |
| `custom-protocol` | — | none | Custom binary messages (type 9+) |
| `encryption` | — | none | ChaCha20-Poly1305 encrypted f32lz4 |

### Authentication

The server supports streaming client authentication matching the C++ implementation:

```rust
use snapcast_server::auth::{AuthValidator, StaticAuthValidator, User, Role};

// Config-based auth (users/roles from config file)
let auth = StaticAuthValidator::new(
    vec![User { name: "player".into(), password: "secret".into(), role: "streaming".into() }],
    vec![Role { name: "streaming".into(), permissions: vec!["Streaming".into()] }],
);
let config = ServerConfig {
    auth: Some(Arc::new(auth)),
    ..ServerConfig::default()
};

// Or implement AuthValidator for custom auth (database, LDAP, etc.)
impl AuthValidator for MyValidator {
    fn validate(&self, scheme: &str, param: &str) -> Result<AuthResult, AuthError> {
        // your logic here
    }
}
```

Clients send Basic auth in the Hello handshake. The server validates credentials and checks the `"Streaming"` permission. Unauthenticated or unauthorized clients receive Error(401/403) and are disconnected.

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
| f32lz4 | optional | none | 32-bit float | zero |
| FLAC   | ✅ default | libFLAC | 16/24-bit | 24ms (block size) |
| Opus   | optional | libopus | lossy | 20ms |
| Vorbis | optional | libvorbis | lossy | variable |

f32lz4 path (zero conversion, full precision):
```
f32 → LZ4 compress → network → LZ4 decompress → f32
```

### Bandwidth Comparison

**48 kHz, 16-bit, stereo:**

| Codec  | Precision | Bandwidth | vs PCM |
|--------|-----------|-----------|--------|
| PCM    | 16-bit    | 1,536 kbit/s | 100% |
| FLAC   | 16-bit    | ~700 kbit/s | ~45% |
| f32lz4 | 32-bit float | ~1,800 kbit/s | ~120% |

**96 kHz, 24-bit, stereo:**

| Codec  | Precision | Bandwidth | vs PCM |
|--------|-----------|-----------|--------|
| PCM    | 24-bit    | 4,608 kbit/s | 100% |
| FLAC   | 24-bit    | ~2,500 kbit/s | ~55% |
| f32lz4 | 32-bit float | ~3,600 kbit/s | ~78% |

f32lz4 trades bandwidth for precision (32-bit float) and zero conversion latency. On a LAN (100+ Mbit/s) the extra bandwidth is negligible. On WiFi it's still fine.

For bandwidth-constrained networks: use FLAC. For quality + simplicity: f32lz4.

> ⚠️ **f32lz4 is not compatible with the original C++ Snapcast.** C++ clients/servers do not recognize this codec. Use `--codec flac` or `--codec pcm` for interoperability with C++ Snapcast.

## Custom Binary Protocol (`--features custom-protocol`)

> ⚠️ **snapcast-rs only.** This feature extends the Snapcast binary protocol with application-defined message types. It is not part of the original C++ Snapcast.

C++ Snapcast safely ignores unknown message types — the factory returns `nullptr` for any unrecognized type, and the connection logs a warning and continues:

```cpp
// C++ snapcast: common/message/factory.hpp
switch (base_message.type) {
    case message_type::kCodecHeader: ...
    case message_type::kTime: ...
    // ...
    default:
        return nullptr;  // unknown types silently skipped
}
```

This means Rust servers can send custom messages to Rust clients while C++ clients on the same server simply ignore them.

### Use Case: Client-Side EQ

A Rust-based multiroom system (e.g. [SnapDog](https://github.com/metaneutrons/SnapDogRust)) can push per-client EQ settings through the binary protocol — no JSON-RPC, no HTTP, no extra connections:

```rust
use snapcast_proto::CustomMessage;

// Server pushes EQ to a specific client
cmd.send(ServerCommand::SendToClient {
    client_id: "kitchen".into(),
    message: CustomMessage::new(9, serde_json::to_vec(&EqConfig {
        bands: vec![Band { freq: 100, gain: 3.0 }, Band { freq: 10000, gain: -2.0 }],
    })?),
}).await;

// Client receives and applies
match event {
    ClientEvent::CustomMessage(msg) if msg.type_id == 9 => {
        let eq: EqConfig = serde_json::from_slice(&msg.payload)?;
        equalizer.update(eq);
    }
}
```

Message types 0–8 are reserved by the Snapcast protocol. Types 9+ are available for application use. The payload format is opaque — the library passes raw bytes, the application chooses JSON, bincode, protobuf, or any other format.

## Encryption (`--features encryption`)

Optional ChaCha20-Poly1305 authenticated encryption for f32lz4 audio chunks. Pure Rust (RustCrypto), zero C dependencies.

### Binary usage

The binaries support `f32lz4e` as a codec alias — it selects `f32lz4` with encryption using a built-in default key:

```bash
# Server — just works, default PSK
snapserver-rs --codec f32lz4e

# Client — just works, default PSK matches
snapclient-rs tcp://192.168.1.50:1704

# Custom PSK (must match on both sides)
snapserver-rs --codec f32lz4e --encryption-psk "my-secret"
snapclient-rs --encryption-psk "my-secret" tcp://192.168.1.50:1704
```

### Library usage

```rust
// Server
let config = ServerConfig {
    codec: "f32lz4".into(),
    encryption_psk: Some("my-secret-key".into()),
    ..ServerConfig::default()
};

// Client
let config = ClientConfig {
    encryption_psk: Some("my-secret-key".into()),
    ..ClientConfig::default()
};
```

- Key derivation: HKDF-SHA256 from PSK + random session salt
- Per-chunk: 12-byte nonce (counter) + 16-byte auth tag = 28 bytes overhead (~0.6%)
- Protects audio content and integrity — metadata (time sync, settings) stays plaintext
- Wrong key: client connects but audio chunks are silently dropped

## Documentation

API documentation: [snapcast-client](https://docs.rs/snapcast-client) · [snapcast-server](https://docs.rs/snapcast-server) · [snapcast-proto](https://docs.rs/snapcast-proto)

Generate locally: `cargo doc --open --no-deps`

## Building

```bash
cargo build --release                                    # default: flac + mdns
cargo build --release --features f32lz4                  # + f32lz4 (pure Rust)
cargo build --release --no-default-features --features f32lz4  # pure Rust, no C deps
```

## Usage

```bash
# Server
snapserver-rs --source "pipe:///tmp/snapfifo?name=Music"
snapserver-rs --codec flac
snapserver-rs --codec f32lz4e                            # encrypted f32lz4 (default key)
snapserver-rs --codec f32lz4e --encryption-psk "secret"  # custom key
snapserver-rs --help

# Client
snapclient-rs tcp://192.168.1.50:1704
snapclient-rs                                            # mDNS auto-discovery
snapclient-rs --encryption-psk "secret"                  # custom key
snapclient-rs --help

# Feed audio
ffmpeg -re -i music.mp3 -f s16le -ar 48000 -ac 2 pipe:1 > /tmp/snapfifo
```

## Code Quality

- `#![forbid(unsafe_code)]` on protocol crate
- `#![deny(unsafe_code)]` on client/server libraries
- Zero `#[allow(dead_code)]`, zero TODOs, zero `#![allow]` blankets
- Constant-time password comparison (subtle crate)
- Structured tracing logging

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
