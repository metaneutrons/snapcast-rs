# snapcast-rs

A Rust implementation of the [Snapcast](https://github.com/snapcast/snapcast) client, providing byte-level protocol compatibility with the C++ snapserver.

## What is this?

Snapcast is a multiroom client-server audio player where all clients play audio in perfect sync. This project rewrites the client (`snapclient`) in Rust, targeting the same binary protocol and achieving the same synchronized playback.

## Architecture

```
snapcast-rs/
├── crates/
│   ├── snapcast-proto/    # Binary protocol: message types, serialization
│   └── snapclient-rs/     # Client: connection, decoders, stream, player
```

The client pipeline:

```
TCP/WS/WSS → Hello handshake → Time sync → CodecHeader → Decoder → Stream → Player
```

- **Connection**: TCP, WebSocket, WebSocket+TLS
- **Decoders**: PCM (passthrough), FLAC (symphonia), Vorbis (symphonia), Opus (libopus)
- **Stream**: Time-synchronized PCM buffer with drift correction (sample insertion/removal)
- **Players**: CoreAudio (macOS), ALSA (Linux), PulseAudio (Linux)
- **Discovery**: mDNS/ZeroConf via mdns-sd

## Building

```bash
# macOS (default features: coreaudio + mdns)
cargo build --release

# With all features
cargo build --release --features websocket,tls,resampler

# Linux (when ALSA/Pulse backends are available)
cargo build --release --no-default-features --features alsa,mdns
```

## Usage

```bash
# Connect to snapserver via TCP
snapclient-rs tcp://192.168.1.100:1704

# Connect via WebSocket
snapclient-rs ws://homeserver.local:1780

# With authentication
snapclient-rs tcp://user:password@myserver:1704

# Options
snapclient-rs --help
snapclient-rs --instance 2 --latency 50 --mixer software tcp://server:1704
```

## Feature Flags

| Feature | Default | Description |
|---|---|---|
| `coreaudio` | ✅ (macOS) | CoreAudio audio backend |
| `mdns` | ✅ | mDNS/ZeroConf server discovery |
| `websocket` | | WebSocket connection support |
| `tls` | | WSS (WebSocket + TLS) support |
| `resampler` | | Sample rate conversion (rubato) |

## CLI Options

| Option | Description |
|---|---|
| `<url>` | Server URL: `tcp://`, `ws://`, or `wss://` |
| `--instance <N>` | Instance ID for multiple clients on same host |
| `--hostID <ID>` | Unique host identifier |
| `--player <name[:params]>` | Audio backend |
| `--latency <ms>` | Additional DAC latency |
| `--sampleformat <rate:bits:channels>` | Resample format (`*` = same as source) |
| `--mixer <mode[:params]>` | Volume control: software, hardware, script, none |
| `--soundcard <name>` | PCM device name or index |
| `--logsink <sink>` | Log output: stdout, stderr, null, file:path |
| `--logfilter <filter>` | Log filter: `*:info`, `Stream:debug` |

## Protocol Compatibility

The `snapcast-proto` crate implements the full Snapcast binary protocol with byte-level compatibility to the C++ implementation. All message types are tested with hardcoded byte vectors derived from the C++ serialization code.

## License

GPL-3.0 — same as the original Snapcast project.
