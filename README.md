# snapcast-rs

Rust implementation of [Snapcast](https://github.com/snapcast/snapcast) — synchronized multiroom audio.

## Architecture

```
snapcast-rs/
├── snapcast-proto      Protocol: binary message serialization (8 message types)
├── snapcast-client     Client library: embeddable, channel-based API
├── snapcast-server     Server library: embeddable, channel-based API
├── snapclient-rs       Client binary: thin CLI wrapper
└── snapserver-rs       Server binary: thin CLI wrapper
```

### Data Flow

```
Audio:    source → PipeStream → mpsc → Encoder → broadcast → SessionServer → snapclient
Control:  control client → TCP JSON-RPC → dispatch → state mutation → notification broadcast
Extension: unknown method → ServerEvent::JsonRpc → app → ServerCommand::SendJsonRpc → broadcast
```

### Library API

Both libraries use the same channel-based pattern:

```rust
// Client
let (mut client, mut events) = SnapClient::new(config);
let cmd = client.command_sender();
// events: ClientEvent::{Connected, VolumeChanged, JsonRpc, ...}
// commands: ClientCommand::{SetVolume, SendJsonRpc, Stop}

// Server
let (mut server, mut events) = SnapServer::new(config);
let cmd = server.command_sender();
// events: ServerEvent::{ClientConnected, JsonRpc, ...}
// commands: ServerCommand::{SendJsonRpc, Stop}
```

The `JsonRpc` event + `SendJsonRpc` command enable custom extensions (e.g. EQ control)
without modifying the library code.

## Building

```bash
cargo build --release
```

Binaries: `target/release/snapclient-rs`, `target/release/snapserver-rs`

### Dependencies

- **libFLAC** — FLAC encoding/decoding (`brew install flac` / `apt install libflac-dev`)
- **libopus** — Opus encoding/decoding (`brew install opus` / `apt install libopus-dev`)
- **libvorbis** — Vorbis encoding (`brew install libvorbis` / `apt install libvorbis-dev`)

### Features (client)

| Feature | Default | Description |
|---------|---------|-------------|
| `coreaudio` | macOS | CoreAudio playback |
| `alsa` | — | ALSA playback (Linux) |
| `pulse` | — | PulseAudio playback (Linux) |
| `mdns` | ✓ | mDNS server discovery |
| `websocket` | — | WebSocket connection |
| `tls` | — | WSS (WebSocket + TLS) |
| `resampler` | — | Sample rate conversion (rubato) |

## Usage

### Client

```bash
# Connect to server
snapclient-rs tcp://192.168.1.50:1704

# mDNS auto-discovery
snapclient-rs

# List audio devices
snapclient-rs --list

# All options
snapclient-rs --help
```

### Server

```bash
# Default: pipe source on /tmp/snapfifo
snapserver-rs

# Custom source
snapserver-rs --source "pipe:///tmp/snapfifo?name=Music&sampleformat=48000:16:2"

# Multiple sources
snapserver-rs --source "pipe:///tmp/snapfifo?name=MPD" --source "tcp://0.0.0.0:4953?name=TCP"

# All options
snapserver-rs --help
```

## Testing

```bash
# All tests (146)
cargo test --all

# Clippy (zero warnings)
cargo clippy --all-targets -- -D warnings

# Quick smoke test: server + client
snapserver-rs --stream-port 11704 --codec pcm &
snapclient-rs tcp://127.0.0.1:11704
```

## License

GPL-3.0-only — same as the original Snapcast.
