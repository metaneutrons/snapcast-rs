# snapcast-rs

Rust implementation of [Snapcast](https://github.com/snapcast/snapcast) — synchronized multiroom audio.

100% pure Rust client. Cross-platform: macOS, Linux, Windows.

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

// Server
let (mut server, mut events) = SnapServer::new(config);
let cmd = server.command_sender();
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

Client: zero C libraries. Server: libflac for FLAC encoding (default).

| Feature  | C Library  | For                    |
|----------|-----------|------------------------|
| `flac`   | libFLAC   | FLAC encoding (server default) |
| `opus`   | libopus   | Opus encoding (server optional) |
| `vorbis` | libvorbis | Vorbis encoding (server optional) |

Server without C deps: `cargo build -p snapserver-rs --no-default-features` (PCM only).

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
# From ffmpeg
ffmpeg -re -i music.mp3 -f s16le -ar 48000 -ac 2 pipe:1 > /tmp/snapfifo
# Test with noise
cat /dev/urandom > /tmp/snapfifo
```

## Server Config File

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

CLI flags override config file values.

## JSON-RPC Control API

All 25 C++ methods implemented. Three transports: HTTP POST `/jsonrpc`, WebSocket at `/jsonrpc`, raw TCP on port 1705. JWT authentication (optional, disabled by default).

## Verified Working

- ✅ Rust server → FLAC encoding → Rust client → CoreAudio (macOS) — clean audio, 40s+ stable
- ✅ Rust client → C++ server — clean audio with drift correction
- ✅ Time sync: -0.04ms precision
- ✅ JSON-RPC via HTTP, TCP (tested with curl and nc)
- ✅ Server builds and runs on Linux (Arch x86_64)
- ✅ Client builds and runs on Linux with ALSA
- ✅ Client + server compile on Windows (aarch64-msvc)
- ✅ PCM codec end-to-end
- ✅ FLAC codec end-to-end
- ✅ mDNS service advertisement + client discovery
- ✅ Config file parsing (/etc/snapserver.conf)
- ✅ Ctrl-C graceful shutdown

## Not Yet Tested

- ⬜ Opus codec end-to-end (encoder + decoder exist, not integration tested)
- ⬜ Vorbis codec end-to-end (encoder + decoder exist, not integration tested)
- ⬜ WebSocket JSON-RPC transport (handler exists, not tested with a WS client)
- ⬜ Snapweb static file serving (code exists, not tested with actual Snapweb files)
- ⬜ ALSA playback quality on Linux (builds, not audio-tested)
- ⬜ PulseAudio backend (builds, not tested)
- ⬜ cpal/WASAPI backend on Windows (compiles, not runtime tested)
- ⬜ librespot stream reader (code exists, needs librespot installed)
- ⬜ airplay stream reader (code exists, needs shairport-sync installed)
- ⬜ JWT authentication flow (token generation works, middleware not integration tested)
- ⬜ Multiroom sync (two clients playing in sync)
- ⬜ Client connecting to Rust server from a different machine (only localhost tested)
- ⬜ State persistence (server.json save/load)
- ⬜ Resampler (rubato, feature-gated, not tested)
- ⬜ WebSocket/TLS client connection
- ⬜ Group/stream management via JSON-RPC (methods exist, not tested with Snapweb)

## Testing

```bash
cargo test --all           # 154 tests
cargo clippy -- -D warnings  # zero warnings
```

## Code Quality

- `#![forbid(unsafe_code)]` on protocol crate
- `#![deny(unsafe_code)]` on client/server (only monotonic clock FFI + FLAC encoder FFI)
- `#![warn(missing_docs)]` — 100% doc coverage
- Zero `#[allow(dead_code)]`
- Zero TODOs in codebase
- Structured tracing logging at all levels
- 10,469 lines · 154 tests · 100 commits

## License

GPL-3.0-only — same as the original Snapcast.
