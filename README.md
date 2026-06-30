# snapcast-rs

[![CI](https://github.com/metaneutrons/snapcast-rs/actions/workflows/ci.yml/badge.svg)](https://github.com/metaneutrons/snapcast-rs/actions/workflows/ci.yml)
[![crates.io](https://img.shields.io/crates/v/snapcast-client.svg)](https://crates.io/crates/snapcast-client)
[![crates.io](https://img.shields.io/crates/v/snapcast-server.svg)](https://crates.io/crates/snapcast-server)
[![docs.rs](https://docs.rs/snapcast-client/badge.svg)](https://docs.rs/snapcast-client)
[![docs.rs](https://docs.rs/snapcast-server/badge.svg)](https://docs.rs/snapcast-server)
[![License: GPL-3.0](https://img.shields.io/badge/license-GPL--3.0-blue.svg)](LICENSE)

> **⚠️ Pre-1.0 — APIs may break on minor version bumps.** Until version 1.0, minor releases (e.g. 0.3 → 0.4) may contain breaking changes to the public API. Pin your dependency to a specific minor version if you need stability.

> **🚨 Breaking changes in 0.17.0 (library embedders).** The `snapcast-server` library now opens **no port** and reads/writes **no files**: `SnapServer::run()` is replaced by `serve(listener)`, and `ServerConfig.state_file` is replaced by `initial_state` plus a new `ServerEvent::StateChanged` event. `snapcast_proto::ProtoError` is now `#[non_exhaustive]`. The `snapserver-rs` binary is unaffected for end users. **See [Migrating to 0.17.0](#migrating-to-0170) for copy-paste mitigations.**

A Rust reimplementation of [Snapcast](https://github.com/snapcast/snapcast), the excellent multiroom audio system created by [Johannes Pohl (badaix)](https://github.com/badaix). Snapcast synchronizes audio playback across multiple devices with sub-millisecond precision — turning any collection of speakers into a perfectly synced whole-home audio system.

This project exists primarily to serve as a native Rust dependency for [SnapDog](https://github.com/SnapDogRocks/snapdog), a multiroom audio appliance. Rather than shelling out to C++ binaries or bridging through FFI, SnapDog embeds the Snapcast protocol directly as a library — receiving audio, encoding it, distributing it to clients, and controlling playback, all within a single Rust process.

To make this possible, snapcast-rs separates the protocol engine from the application shell. The **library crates** (`snapcast-client`, `snapcast-server`) implement the Snapcast binary protocol, audio encoding/decoding, and time synchronization — but own no audio devices, open no HTTP ports, and read no config files. They communicate exclusively through typed Rust channels, making them straightforward to embed in any application.

The **binary crates** (`snapclient-rs`, `snapserver-rs`) are thin wrappers around these libraries. They add the things a standalone application needs: reading audio from pipes and processes, serving the JSON-RPC control API over HTTP and TCP, hosting the Snapweb UI, and outputting audio through platform-native backends via cpal. They are intended as standalone replacements for TCP-based Snapcast audio workflows; WebSocket audio streaming is not implemented yet.

The result is a Snapcast implementation that works as a TCP-audio replacement for common Snapcast deployments and as an embeddable building block for Rust applications that need synchronized multiroom audio.

snapcast-rs is compatible with the original C++ Snapcast over the TCP audio transport when using standard codecs (PCM, FLAC, Opus, Vorbis). However, three optional features break audio compatibility:

| Feature | What it does | C++ behavior |
|---------|-------------|--------------|
| `f32lz4` | 32-bit float LZ4 codec | C++ clients reject unknown codec |
| `custom-protocol` | Application-defined message types (9+) | C++ clients silently ignore |
| `encryption` | ChaCha20-Poly1305 encrypted f32lz4 | C++ clients reject unknown codec |

If you enable `f32lz4` or `encryption` on the server, C++ clients cannot decode the audio. To prevent them from auto-connecting via mDNS, change the service type in your application binary (mDNS is the application's responsibility, not the library's):

```rust
// Use astro-dnssd or any DNS-SD crate in your binary
let _mdns = astro_dnssd::DNSServiceBuilder::new("_myapp._tcp", port)
    .with_name("MyServer")
    .register()?;
```

For full interoperability with C++ clients, use `--codec flac` or `--codec pcm` and leave `custom-protocol` and `encryption` disabled.

## Install

The library crates are published on [crates.io](https://crates.io) (requires Rust **1.88+**):

```bash
cargo add snapcast-client   # embeddable client engine
cargo add snapcast-server   # embeddable server engine
```

`snapcast-proto` is pulled in transitively — add it directly only if you use the wire types. The `snapclient-rs` / `snapserver-rs` binaries are **not** on crates.io; grab a pre-built one from [Releases](https://github.com/metaneutrons/snapcast-rs/releases) or build from source (see [Building](#building)).

## Key Features

- **Dynamic Audio Pipeline**: The client automatically re-initializes the audio device when the server changes sample rate or channels.
- **Integrated Resampling**: Automatic fallback to `rubato`-based resampling if the local hardware doesn't support the server's native format.
- **Single Source of Truth Defaults**: Ports, schemes, codec names, sample format defaults, bind addresses, and payload limits live in `snapcast-proto`.
- **Bounded Protocol Reads**: Client and server reject oversized binary-protocol payloads before allocation.
- **Per-Stream Format Ownership**: Each server stream owns its codec/sample-format encoder state instead of leaking global defaults into per-stream paths.
- **Lossless f32 Decode Path**: FLAC and f32lz4 decoders output native f32 samples — no intermediate 16-bit quantization. 24-bit FLAC preserves full resolution end-to-end.
- **Configurable Bind Addresses**: The binary's listeners bind loopback, IPv4, IPv6, or deployment-specific interfaces — the libraries open no listeners, so the embedder supplies the already-bound socket.
- **Systemd Integration**: Native `sd-notify` support on Linux for service readiness and real-time status reporting (volume, codec, format).
- **Public Audio Monitoring**: Public `audio_rx` channel in the library for real-time PCM monitoring and analysis.
- **End-to-End Testing**: Robust integration test suite verifying the entire audio transmission path from server to client.

## Architecture

```text
snapcast-rs/
├── snapcast-proto      Protocol: binary message serialization
├── snapcast-client     Client library: embeddable, f32 audio output
├── snapcast-server     Server library: embeddable, f32 audio input
├── snapclient-rs       Client binary: cpal audio, software + hardware (ALSA) mixer
├── snapserver-rs       Server binary: stream readers, JSON-RPC, HTTP
└── snapcast-tests      Integration tests
```

| Crate | Role | Docs |
|-------|------|------|
| [snapcast-proto](crates/snapcast-proto) | Binary protocol, message serialization | [docs.rs](https://docs.rs/snapcast-proto) |
| [snapcast-client](crates/snapcast-client) | Client library: embeddable, f32 audio output | [docs.rs](https://docs.rs/snapcast-client) |
| [snapcast-server](crates/snapcast-server) | Server library: embeddable, f32 audio input | [docs.rs](https://docs.rs/snapcast-server) |
| [snapclient-rs](crates/snapclient-rs) | Client binary: cpal audio, software + hardware (ALSA) mixer | — |
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
let (mut client, events, mut audio_rx) = SnapClient::new(config);

// Run (blocks, reconnects on error)
tokio::spawn(async move { client.run().await });

// Monitor decoded audio frames in real-time
tokio::spawn(async move {
    while let Some(frame) = audio_rx.recv().await {
        // frame.samples is Vec<f32> (interleaved)
        // frame.timestamp_usec is server-time
    }
});

// Shared state for direct audio device access (used by snapclient-rs)
let stream = Arc::clone(&client.stream);           // time-synced PCM buffer
let time_provider = Arc::clone(&client.time_provider); // server clock sync

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
    scheme: String,            // "tcp" only for audio streaming
    host: String,              // server host (empty = mDNS discovery)
    port: u16,                 // default: 1704
    auth: Option<Auth>,        // Basic auth for Hello handshake
    instance: u32,             // for multiple clients on one host
    host_id: String,           // unique identifier (default: MAC)
    latency: i32,              // additional latency offset (ms)
    client_name: String,       // default: "Snapclient"
    encryption_psk: Option<String>, // f32lz4 encryption (feature: encryption)
}
```

### Client Features

| Feature     | Default | C dep | Description |
|-------------|---------|-------|-------------|
| `f32lz4`    | —       | none  | f32 LZ4 codec (lz4_flex) |
| `websocket` | —       | none  | Experimental transport module only; `SnapConnection::new` rejects `ws://` until binary audio WS is implemented |
| `tls`       | —       | none  | Experimental WSS module only; `SnapConnection::new` rejects `wss://` until binary audio WS is implemented |
| `resampler` | —       | none  | Sample rate conversion (rubato) |
| `custom-protocol` | — | none | Custom binary messages (type 9+) |
| `encryption` | — | none | ChaCha20-Poly1305 encrypted f32lz4 |

## Server Library API

```rust
use snapcast_server::{SnapServer, ServerConfig, ServerEvent, ServerCommand, AudioFrame, AudioData, StreamConfig};

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

// The library opens no port: bind the audio listener and hand it to serve().
let listener = tokio::net::TcpListener::bind("0.0.0.0:1704").await?;
tokio::spawn(async move { server.serve(listener).await });

// Typed commands
cmd.send(ServerCommand::SetClientVolume { client_id, volume: 80, muted: false }).await;
cmd.send(ServerCommand::SetClientLatency { client_id, latency: 50 }).await;
cmd.send(ServerCommand::SetClientName { client_id, name }).await;
cmd.send(ServerCommand::SetGroupStream { group_id, stream_id }).await;
cmd.send(ServerCommand::SetGroupMute { group_id, muted: true }).await;
cmd.send(ServerCommand::DeleteClient { client_id }).await;
let (tx, rx) = oneshot::channel();
cmd.send(ServerCommand::GetStatus { response_tx: tx }).await;

// Push f32 audio directly
audio_tx.send(AudioFrame {
    data: AudioData::F32(samples),
    timestamp_usec,
}).await;

// Reactive events
match event {
    ServerEvent::ClientConnected { id, hello } => {} // hello: Hello (mac, host_name, …)
    ServerEvent::ClientDisconnected { id } => {}
    ServerEvent::ClientVolumeChanged { client_id, volume, muted } => {}
    ServerEvent::ClientLatencyChanged { client_id, latency } => {}
    ServerEvent::ClientNameChanged { client_id, name } => {}
    ServerEvent::GroupStreamChanged { group_id, stream_id } => {}
    ServerEvent::GroupMuteChanged { group_id, muted } => {}
    ServerEvent::StreamStatus { stream_id, status } => {}
    ServerEvent::StateChanged(state) => { /* persist `state` if you want durability */ }
    _ => {} // ServerEvent is #[non_exhaustive]
}
```

### Server Config

```rust
ServerConfig {
    buffer_ms: u32,            // default: 1000
    codec: String,             // default: "flac" (feature-dependent: flac > f32lz4 > pcm)
    sample_format: String,     // default: "48000:16:2"
    auth: Option<Arc<dyn AuthValidator>>, // default: None (no auth)
    client_filter: Option<Arc<dyn ClientFilter>>, // default: None (accept all)
    encryption_psk: Option<String>, // f32lz4 encryption (feature: encryption)
    initial_state: Option<ServerState>, // seed clients/groups on startup (None = empty)
    send_audio_to_muted: bool, // default: false
}
```

The library opens no listener and persists nothing: bind the audio port yourself
and pass it to `server.serve(listener)`, and persist `ServerState` from
`ServerEvent::StateChanged` if you need durability (the bind address/port live in
the application, e.g. `snapserver-rs`'s config, not in `ServerConfig`).

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
| `flac`   | ✅      | none      | FLAC encoding (pure Rust, flacenc) |
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

### Client Filtering

Filter clients at connection time based on MAC address, hostname, or any Hello field:

```rust
use snapcast_server::auth::ClientFilter;
use snapcast_server::Hello;

/// Only accept clients whose MAC is in the allowlist.
struct MacAllowlist(Vec<String>);

impl ClientFilter for MacAllowlist {
    fn accept(&self, hello: &Hello) -> bool {
        // Empty list = accept all (backwards compatible)
        self.0.is_empty() || self.0.iter().any(|m| m.eq_ignore_ascii_case(&hello.mac))
    }
}

let config = ServerConfig {
    client_filter: Some(Arc::new(MacAllowlist(vec!["aa:bb:cc:dd:ee:ff".into()]))),
    ..ServerConfig::default()
};
```

Rejected clients are disconnected immediately after Hello with a warning log.

### Network Ports

| Port | Protocol | Owner | Purpose |
|------|----------|-------|---------|
| 1704 | TCP | App/Binary | Binary protocol (audio + time sync) |
| 1705 | TCP | Binary | JSON-RPC control |
| 1780 | HTTP/WS | Binary | JSON-RPC + Snapweb UI |

The library crates open **no** listeners. The embedding application (or the `snapserver-rs` binary) binds every port and hands the audio listener to `server.serve(listener)`. JSON-RPC/HTTP are binary-only.

Bind addresses are configurable:

```bash
snapserver-rs --stream-bind-address 127.0.0.1 --control-bind-address 127.0.0.1 --http-bind-address ::1
```

Equivalent config file keys are `bind_to_address` or `bind_address` under `[tcp-streaming]`, `[tcp-control]`, and `[http]`.

## Codecs

| Codec  | Default | C dep | Precision | Latency |
|--------|---------|-------|-----------|---------|
| PCM    | ✅ always | none | 16/24/32-bit | zero |
| f32lz4 | optional | none | 32-bit float | zero |
| FLAC   | ✅ default | none | 16/24-bit (decoded to f32) | 24ms (block size) |
| Opus   | optional | libopus | 16-bit | 20ms |
| Vorbis | optional | libvorbis | lossy | variable |

f32lz4 path (zero conversion, full precision):

```text
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

A Rust-based multiroom system (e.g. [SnapDog](https://github.com/SnapDogRocks/snapdog)) can push per-client EQ settings through the binary protocol — no JSON-RPC, no HTTP, no extra connections:

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
cargo build --release                                    # default: flac
cargo build --release --features f32lz4                  # + f32lz4 (pure Rust)
cargo build --release --no-default-features --features f32lz4  # pure Rust, no C deps
```

PCM, **FLAC**, and f32lz4 are pure Rust — no system library or C toolchain
needed. The remaining native codec features need their system libraries
available to the linker:

| Feature | Linux package examples | macOS package examples |
|---------|------------------------|------------------------|
| `opus` | `libopus-dev pkg-config` | `opus pkg-config` |
| `vorbis` | `libvorbis-dev` | `libvorbis` |

The CI workflow validates the default build plus custom protocol, encryption, client transport/resampler features, and the Linux native codec feature set with those packages installed.

## Usage

```bash
# Server
snapserver-rs --source "pipe:///tmp/snapfifo?name=Music"
snapserver-rs --codec flac
snapserver-rs --codec f32lz4e                            # encrypted f32lz4 (default key)
snapserver-rs --codec f32lz4e --encryption-psk "secret"  # custom key
snapserver-rs --stream-bind-address 127.0.0.1             # bind audio listener to loopback
snapserver-rs --help

# Client
snapclient-rs tcp://192.168.1.50:1704
snapclient-rs tcp://[::1]:1704
snapclient-rs                                            # mDNS auto-discovery
snapclient-rs --encryption-psk "secret"                  # custom key
snapclient-rs --help

# Feed audio
ffmpeg -re -i music.mp3 -f s16le -ar 48000 -ac 2 pipe:1 > /tmp/snapfifo
```

## Code Quality

- `#![deny(unsafe_code)]` on all library crates; the only `unsafe` is one narrow FFI exception for the platform monotonic clock in `snapcast-proto` — `snapcast-client` and `snapcast-server` are entirely `unsafe`-free
- Warning-clean `cargo check`, `cargo test`, and `cargo clippy -- -D warnings` gates
- No crate-level `#![allow]` blankets and no TODO markers in production code
- Shared defaults/constants in `snapcast-proto` instead of duplicated magic strings
- Constant-time password comparison (subtle crate)
- Bounded binary-frame payload allocation on both client and server
- Structured tracing logging

## Migrating to 0.17.0

`0.17.0` makes the `snapcast-server` library fully I/O-free — it now **opens no port and reads/writes no files**, completing the "protocol engine, not application shell" separation. Three breaking changes affect library embedders (the `snapserver-rs` binary already owns its I/O, so end users are unaffected):

**1. The server no longer binds a port — inject a listener.**

```rust
// Before (0.16)
let (mut server, events) = SnapServer::new(config); // config.stream_bind_address / stream_port
server.run().await?;

// After (0.17)
let (mut server, events) = SnapServer::new(config); // those fields are gone
let listener = tokio::net::TcpListener::bind("0.0.0.0:1704").await?;
server.serve(listener).await?;
```

`ServerConfig` lost `stream_bind_address` and `stream_port`; the embedder binds the `tokio::net::TcpListener` and passes it to `serve()`. As a bonus the session handler is now generic over the transport and unit-testable over `tokio::io::duplex`.

**2. The server no longer persists state — load/save it yourself.**

```rust
// Before (0.16): the library read/wrote a JSON file itself
let config = ServerConfig { state_file: Some("/var/lib/snap/state.json".into()), ..Default::default() };

// After (0.17): supply the initial snapshot, and persist change events
let initial = std::fs::read_to_string("state.json").ok()
    .and_then(|s| serde_json::from_str::<ServerState>(&s).ok());
let config = ServerConfig { initial_state: initial, ..Default::default() };
let (mut server, mut events) = SnapServer::new(config);

// Persist off the event loop (debounce to the latest snapshot):
while let Some(ev) = events.recv().await {
    if let ServerEvent::StateChanged(state) = ev {
        // e.g. tokio::task::spawn_blocking: write to a temp file + atomic rename
    }
}
```

`ServerState::load`/`save` are removed and `ServerState` is now a public `Serialize`/`Deserialize` type. Besides honoring the no-files contract, this fixes a latency bug: the old code wrote the file **while holding the shared-state lock**, stalling command dispatch on slow storage (e.g. an SD card).

**3. `ProtoError` is now `#[non_exhaustive]`.**

Add a wildcard arm to any `match` on `snapcast_proto::ProtoError` so future variants don't break you:

```rust
match err {
    ProtoError::Io(e) => { /* ... */ }
    ProtoError::Json(e) => { /* ... */ }
    _ => { /* PayloadTooLarge and future variants */ }
}
```

## Releases

Pre-built binaries for every release, all with FLAC support (pure Rust — no system library or C toolchain needed on any platform):

| Platform | Target triple | Client | Server |
|----------|----------------|--------|--------|
| Linux x86_64 | `x86_64-unknown-linux-gnu` | ✅ | ✅ |
| Linux aarch64 | `aarch64-unknown-linux-gnu` | ✅ | ✅ |
| macOS x86_64 (Intel) | `x86_64-apple-darwin` | ✅ | ✅ |
| macOS aarch64 (Apple Silicon) | `aarch64-apple-darwin` | ✅ | ✅ |
| Windows x86_64 | `x86_64-pc-windows-msvc` | ✅ | ✅ |

Download from [GitHub Releases](https://github.com/metaneutrons/snapcast-rs/releases) — assets are named `snapclient-rs-<target>` / `snapserver-rs-<target>` (`.exe` on Windows), e.g. `snapserver-rs-aarch64-apple-darwin` or `snapclient-rs-x86_64-pc-windows-msvc.exe`.

Library crates published to [crates.io](https://crates.io): `snapcast-proto`, `snapcast-client`, `snapcast-server`.

## Known Limitations

- **No WebSocket audio transport** — the server exposes JSON-RPC WebSockets at `/jsonrpc`, but binary audio streaming is TCP-only. The CLI rejects `ws://` and `wss://` for audio clients until a verified binary audio WebSocket contract is implemented.
- **Dynamic `Stream.AddStream` is application-owned** — the embeddable server can expose and route streams created before `serve()`. Runtime `Stream.AddStream` returns an explicit error because the library does not own source readers after startup.
- **Opus is a native optional feature** — `--features opus` requires system Opus discoverable by `pkg-config` or the native build tools needed by `audiopus_sys`.

## License

GPL-3.0-only — same as the original Snapcast.
