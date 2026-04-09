# snapcast-rs

Rust implementation of [Snapcast](https://github.com/snapcast/snapcast) — synchronized multiroom audio.

100% pure Rust by default. Cross-platform: macOS, Linux, Windows.

## Architecture

```
snapcast-rs/
├── snapcast-proto      Protocol: binary message serialization (8 message types)
├── snapcast-client     Client library: embeddable, f32 audio output channel
├── snapcast-server     Server library: embeddable, f32 audio input channel
├── snapclient-rs       Client binary: cpal audio output
└── snapserver-rs       Server binary: stream readers, JSON-RPC, HTTP, config
```

### Library Design

Both libraries are pure audio engines — no device I/O, no HTTP, no config files:

```
snapcast-client:  protocol + decode + sync → f32 AudioFrame channel
snapcast-server:  f32 AudioFrame channel → encode + protocol → clients
```

### Typed API (no JSON-RPC detour)

```rust
// Server: direct control
cmd.send(ServerCommand::SetClientVolume { client_id: "wz".into(), volume: 80, muted: false }).await;
cmd.send(ServerCommand::SetGroupStream { group_id: "eg".into(), stream_id: "music".into() }).await;

// Server: reactive events
match event {
    ServerEvent::ClientVolumeChanged { client_id, volume, muted } => { /* update UI */ }
    ServerEvent::ClientConnected { id, name } => { /* new client */ }
    _ => {}
}

// Client: receive f32 audio
let (mut client, events, audio_rx) = SnapClient::new(config);
// audio_rx: Receiver<AudioFrame> — feed to EQ, resampler, DAC
```

### Data Flow

```
Audio:    source → StreamReader → Encoder → broadcast → SessionServer → snapclient
Control:  Snapweb → HTTP/WS/TCP JSON-RPC → ServerCommand → library → state + push + event
Custom:   registered method → ServerEvent::JsonRpc → app → oneshot response
f32 path: SnapDog f32 → audio_tx → LZ4 compress → network → LZ4 decompress → f32 → SnapDog
```

## Building

```bash
# Just works on macOS, Linux, and Windows — no flags needed
cargo build --release
```

### Platform Defaults

| Platform | Client Audio | Server |
|----------|-------------|--------|
| macOS    | cpal (CoreAudio) | ✅ |
| Linux    | cpal (ALSA)      | ✅ |
| Windows  | cpal (WASAPI)    | ✅ |

### Codec Features

| Codec  | Default | C dep   | Feature  |
|--------|---------|---------|----------|
| PCM    | ✅ always | none  | —        |
| f32lz4 | ✅ default | none (lz4_flex) | `f32lz4` |
| FLAC   | optional | libFLAC | `flac`   |
| Opus   | optional | libopus | `opus`   |
| Vorbis | optional | libvorbis | `vorbis` |

Default build: zero C dependencies. Pure Rust.

```bash
cargo build -p snapserver-rs --features flac      # + FLAC
cargo build -p snapserver-rs --features opus       # + Opus
cargo build -p snapserver-rs --no-default-features  # PCM only
```

### Other Features (client)

| Feature     | Description                        |
|-------------|------------------------------------|
| `mdns`      | mDNS server discovery (default)    |
| `f32lz4`    | f32 LZ4 codec (default)            |
| `websocket` | WebSocket connection               |
| `tls`       | WSS (WebSocket + TLS)              |
| `resampler` | Sample rate conversion (rubato)    |

## Usage

### Server

```bash
snapserver-rs                                          # default: pipe source, f32lz4 codec
snapserver-rs --codec pcm                              # PCM codec
snapserver-rs --codec flac                             # FLAC (needs --features flac)
snapserver-rs --source "pipe:///tmp/snapfifo?name=Music"
snapserver-rs --doc-root /usr/share/snapserver/snapweb  # Snapweb UI
snapserver-rs --help
```

Sources: `pipe://`, `file://`, `process://`, `tcp://`, `librespot://`, `airplay://`

### Client

```bash
snapclient-rs tcp://192.168.1.50:1704   # connect to server
snapclient-rs                            # mDNS auto-discovery
snapclient-rs --list                     # list audio devices
snapclient-rs --help
```

### Config File

`/etc/snapserver.conf` (INI format, compatible with C++ server):

```ini
[stream]
source = pipe:///tmp/snapfifo?name=default

[http]
port = 1780
doc_root = /usr/share/snapserver/snapweb

[tcp-streaming]
port = 1704

[tcp-control]
port = 1705
```

## JSON-RPC Control API

25 methods implemented. Three transports: HTTP POST `/jsonrpc`, WebSocket `/jsonrpc`, TCP port 1705.

Custom method registry with proper request/response lifecycle:

```rust
server.register_method("Client.SetEq");      // app handles, waits for response (5s timeout)
server.register_notification("Client.EqChanged"); // app handles, no response
// unregistered methods → -32601 immediately
```

JWT authentication (optional, disabled by default).

## Embedding (SnapDog Integration)

```rust
// Client: receive f32 audio → EQ → resample → DAC
let (mut client, events, audio_rx) = SnapClient::new(config);
tokio::spawn(async move { client.run().await });
while let Some(frame) = audio_rx.recv().await {
    zone_player.send(PcmMessage::Audio(frame.samples)).await;
}

// Server: push f32 audio from any source
let (mut server, events, audio_tx) = SnapServer::new(config);
audio_tx.send(AudioFrame { samples: f32_data, sample_rate: 48000, channels: 2, timestamp_usec: now }).await;

// Server: typed control (no JSON-RPC detour)
cmd.send(ServerCommand::SetClientVolume { client_id, volume: 80, muted: false }).await;
```

## f32lz4 Codec

Lossless compressed f32 transmission — skips PCM conversion entirely:

```
FLAC:    f32 → i16 → FLAC → network → FLAC → i16 → f32  (6 conversions, 16-bit precision)
f32lz4:  f32 → LZ4 → network → LZ4 → f32               (2 conversions, 32-bit precision)
```

Pure Rust (lz4_flex). Feature-gated. Not compatible with C++ Snapcast clients.

## Testing

```bash
cargo test -p snapcast-proto -p snapcast-client -p snapcast-server  # 114 tests
cargo clippy --all-targets -- -D warnings                            # zero warnings
```

## Code Quality

- `#![forbid(unsafe_code)]` on protocol crate
- `#![deny(unsafe_code)]` on client/server (only monotonic clock FFI + FLAC encoder FFI)
- `#![warn(missing_docs)]` — doc coverage on all library crates
- Structured tracing logging at all levels
- sccache enabled for fast rebuilds
- 10,083 lines · 135 commits

## License

GPL-3.0-only — same as the original Snapcast.
