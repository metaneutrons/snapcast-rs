# snapcast-rs

Rust implementation of [Snapcast](https://github.com/snapcast/snapcast) — synchronized multiroom audio.

100% pure Rust by default. Cross-platform: macOS, Linux, Windows.

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
Audio:    source → StreamReader → mpsc → Encoder → broadcast → SessionServer → snapclient
Control:  control client → HTTP/WS/TCP JSON-RPC → dispatch → state → notification broadcast
Custom:   unknown method → ServerEvent::JsonRpc → app handles → ServerCommand::SendJsonRpc
```

### Embeddable Library API

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
# Just works on macOS, Linux, and Windows — no flags needed
cargo build --release
```

Binaries: `target/release/snapclient-rs`, `target/release/snapserver-rs`

### Platform Defaults

| Platform | Client Audio Backend | Server |
|----------|---------------------|--------|
| macOS    | CoreAudio (native)  | ✅     |
| Linux    | ALSA (native)       | ✅     |
| Windows  | cpal (WASAPI)       | ✅     |

### Optional Features (client)

| Feature     | Description                        |
|-------------|------------------------------------|
| `coreaudio` | CoreAudio playback (macOS default) |
| `alsa`      | ALSA playback (Linux default)      |
| `cpal`      | Cross-platform via cpal (Windows default, also works on macOS/Linux) |
| `pulse`     | PulseAudio playback (Linux)        |
| `mdns`      | mDNS server discovery (default)    |
| `websocket` | WebSocket connection               |
| `tls`       | WSS (WebSocket + TLS)              |
| `resampler` | Sample rate conversion (rubato)    |

### Dependencies

Zero C libraries by default. Optional:

| Feature  | C Library  | For                    |
|----------|-----------|------------------------|
| `opus`   | libopus   | Opus encoding (server) |
| `vorbis` | libvorbis | Vorbis encoding (server) |

## Usage

### Server

```bash
# Default: reads /etc/snapserver.conf, pipe source on /tmp/snapfifo
snapserver-rs

# Custom source
snapserver-rs --source "pipe:///tmp/snapfifo?name=Music&sampleformat=48000:16:2"

# Multiple sources
snapserver-rs --source "pipe:///tmp/snapfifo?name=MPD" \
              --source "librespot:///librespot?name=Spotify&bitrate=320"

# With Snapweb UI
snapserver-rs --doc-root /usr/share/snapserver/snapweb

# All options
snapserver-rs --help
```

Supported stream sources: `pipe://`, `file://`, `process://`, `tcp://`, `librespot://`, `airplay://`

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

### Feed Audio

```bash
# From MPD (configure output to /tmp/snapfifo)
# From ffmpeg
ffmpeg -i music.mp3 -f s16le -ar 48000 -ac 2 - > /tmp/snapfifo
# Test with noise
cat /dev/urandom > /tmp/snapfifo
```

## Server Config File

`/etc/snapserver.conf` (INI format, same as C++ server):

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

CLI flags override config file values.

## JSON-RPC Control API

All 25 C++ methods implemented: `Client.{GetStatus,SetVolume,SetLatency,SetName}`,
`Group.{GetStatus,SetMute,SetStream,SetClients,SetName}`,
`Server.{GetRPCVersion,GetStatus,DeleteClient,GetToken,Authenticate}`,
`Stream.{AddStream,RemoveStream,SetProperty,Control}`.

Three transports: HTTP POST `/jsonrpc`, WebSocket at `/jsonrpc`, raw TCP on port 1705.

JWT authentication (optional, disabled by default).

## Testing

```bash
cargo test --all           # 151 tests
cargo clippy -- -D warnings  # zero warnings
```

## Code Quality

- `#![forbid(unsafe_code)]` on protocol crate
- `#![deny(unsafe_code)]` on client/server (only monotonic clock FFI allowed)
- `#![warn(missing_docs)]` — 100% doc coverage
- Zero `#[allow(dead_code)]`
- Zero TODOs in codebase
- Structured tracing logging at all levels

## License

GPL-3.0-only — same as the original Snapcast.
