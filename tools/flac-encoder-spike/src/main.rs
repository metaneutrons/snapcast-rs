//! Verification spike for the pure-Rust FLAC encoder evaluation.
//!
//! See `docs/flac-encoder-evaluation.md` for the full write-up. In short:
//! does a chunk-by-chunk "header once, frames per chunk" encode via flacenc /
//! oxideav-flac actually round-trip correctly, matching how snapcast-rs's
//! production server feeds FLAC (1152-frame blocks, live stream, placeholder
//! STREAMINFO stats)? This is a standalone evaluation tool, not part of the
//! snapcast-rs workspace or build — see the `[workspace]` marker in
//! `Cargo.toml`. Run with `cargo run --release` from this directory.

use std::fs;
use std::io::Cursor;

const SAMPLE_RATE: u32 = 48_000;
const CHANNELS: usize = 2;
const BITS: usize = 16;
const BLOCK_FRAMES: usize = 1152; // matches snapserver-rs's FLAC_BLOCK_FRAMES
const DURATION_SECS: f64 = 3.005; // deliberately NOT a multiple of BLOCK_FRAMES — exercises the partial final block

/// Deterministic test signal: a few mixed sine tones (not pure silence/DC,
/// which some encoders special-case) at a known sample count.
fn generate_pcm() -> Vec<i32> {
    let total_frames = (SAMPLE_RATE as f64 * DURATION_SECS) as usize;
    let mut out = Vec::with_capacity(total_frames * CHANNELS);
    for n in 0..total_frames {
        let t = n as f64 / SAMPLE_RATE as f64;
        let l = (0.3 * (2.0 * std::f64::consts::PI * 440.0 * t).sin()
            + 0.15 * (2.0 * std::f64::consts::PI * 1500.0 * t).sin())
            * i16::MAX as f64;
        let r = (0.3 * (2.0 * std::f64::consts::PI * 523.25 * t).sin()
            + 0.1 * (2.0 * std::f64::consts::PI * 2000.0 * t).sin())
            * i16::MAX as f64;
        out.push(l as i32);
        out.push(r as i32);
    }
    out
}

// ---------------------------------------------------------------------
// Candidate A: flacenc — header via Stream::write (0 frames), frames via
// the lower-level encode_fixed_size_frame() + FrameBuf::fill_interleaved.
// ---------------------------------------------------------------------
mod via_flacenc {
    use super::*;
    use flacenc::component::{BitRepr, Stream, StreamInfo};
    use flacenc::config;
    use flacenc::error::Verify;
    use flacenc::source::{Fill, FrameBuf};

    pub fn encode(pcm: &[i32]) -> Vec<u8> {
        let config = config::Encoder::default().into_verified().unwrap();
        let stream_info = StreamInfo::new(SAMPLE_RATE as usize, CHANNELS, BITS).unwrap();

        // Header: marker + STREAMINFO, zero frames (placeholder stats,
        // matching how a live/unbounded stream's header looks). Block size
        // IS known up front for a live stream (we choose it), so set it
        // explicitly — matches what encode_with_fixed_block_size does.
        let mut header_stream = Stream::new(SAMPLE_RATE as usize, CHANNELS, BITS).unwrap();
        header_stream
            .stream_info_mut()
            .set_block_sizes(BLOCK_FRAMES, BLOCK_FRAMES)
            .unwrap();
        let mut header_sink = flacenc::bitsink::MemSink::<u8>::new();
        header_stream.write(&mut header_sink).unwrap();
        let mut out = header_sink.into_inner();

        let mut frame_number: usize = 0;
        for chunk in pcm.chunks(BLOCK_FRAMES * CHANNELS) {
            let frames_here = chunk.len() / CHANNELS;
            let mut framebuf = FrameBuf::with_size(CHANNELS, frames_here).unwrap();
            framebuf.fill_interleaved(chunk).unwrap();

            let frame =
                flacenc::encode_fixed_size_frame(&config, &framebuf, frame_number, &stream_info)
                    .unwrap();

            let mut frame_sink = flacenc::bitsink::MemSink::<u8>::new();
            frame.write(&mut frame_sink).unwrap();
            out.extend(frame_sink.into_inner());
            frame_number += 1;
        }
        out
    }
}

// ---------------------------------------------------------------------
// Candidate B: oxideav-flac — header via output_params().extradata
// (read immediately after construction, before any frame), frames via
// send_frame()/receive_packet() push-pull.
// ---------------------------------------------------------------------
mod via_oxideav {
    use super::*;
    use oxideav_core::format::{MediaType, SampleFormat};
    use oxideav_core::frame::{AudioFrame, Frame};
    use oxideav_core::stream::{CodecId, CodecParameters};
    use oxideav_flac::metadata::FLAC_MAGIC;

    pub fn encode(pcm: &[i32]) -> Vec<u8> {
        let mut params = CodecParameters::audio(CodecId::new("flac"));
        params.media_type = MediaType::Audio;
        params.sample_rate = Some(SAMPLE_RATE);
        params.channels = Some(CHANNELS as u16);
        params.sample_format = Some(SampleFormat::S16);

        let mut encoder = oxideav_flac::encoder::make_encoder(&params).unwrap();

        // Header read immediately after construction — before any frame.
        let mut out = Vec::new();
        out.extend_from_slice(&FLAC_MAGIC);
        out.extend_from_slice(&encoder.output_params().extradata);

        for chunk in pcm.chunks(BLOCK_FRAMES * CHANNELS) {
            let frames_here = (chunk.len() / CHANNELS) as u32;
            let mut bytes = Vec::with_capacity(chunk.len() * 2);
            for &s in chunk {
                bytes.extend_from_slice(&(s as i16).to_le_bytes());
            }
            let af = AudioFrame {
                samples: frames_here,
                pts: None,
                data: vec![bytes],
            };
            encoder.send_frame(&Frame::Audio(af)).unwrap();
            while let Ok(pkt) = encoder.receive_packet() {
                out.extend_from_slice(&pkt.data);
            }
        }
        // Drain any final partial frame.
        encoder.flush().unwrap();
        while let Ok(pkt) = encoder.receive_packet() {
            out.extend_from_slice(&pkt.data);
        }
        out
    }
}

// ---------------------------------------------------------------------
// Verification: decode via symphonia (independent of both candidates,
// and literally what the real snapcast-client uses in production).
// ---------------------------------------------------------------------
fn decode_with_symphonia(flac_bytes: Vec<u8>) -> Result<Vec<i16>, String> {
    use symphonia::core::codecs::DecoderOptions;
    use symphonia::core::errors::Error as SymError;
    use symphonia::core::formats::FormatOptions;
    use symphonia::core::io::MediaSourceStream;
    use symphonia::core::meta::MetadataOptions;
    use symphonia::core::probe::Hint;
    let cursor = Cursor::new(flac_bytes);
    let mss = MediaSourceStream::new(Box::new(cursor), Default::default());
    let mut hint = Hint::new();
    hint.with_extension("flac");

    let probed = symphonia::default::get_probe()
        .format(
            &hint,
            mss,
            &FormatOptions::default(),
            &MetadataOptions::default(),
        )
        .map_err(|e| format!("probe failed: {e}"))?;
    let mut format = probed.format;
    let track = format
        .tracks()
        .iter()
        .find(|t| t.codec_params.codec != symphonia::core::codecs::CODEC_TYPE_NULL)
        .ok_or("no audio track")?
        .clone();
    let mut decoder = symphonia::default::get_codecs()
        .make(&track.codec_params, &DecoderOptions::default())
        .map_err(|e| format!("decoder init failed: {e}"))?;

    let mut out: Vec<i16> = Vec::new();
    loop {
        let packet = match format.next_packet() {
            Ok(p) => p,
            Err(SymError::IoError(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
            Err(e) => return Err(format!("packet read failed: {e}")),
        };
        match decoder.decode(&packet) {
            Ok(decoded) => {
                let spec = *decoded.spec();
                let mut sample_buf = symphonia::core::audio::SampleBuffer::<i16>::new(
                    decoded.capacity() as u64,
                    spec,
                );
                sample_buf.copy_interleaved_ref(decoded);
                out.extend_from_slice(sample_buf.samples());
            }
            Err(SymError::DecodeError(e)) => return Err(format!("DECODE ERROR: {e}")),
            Err(e) => return Err(format!("decode failed: {e}")),
        }
    }
    Ok(out)
}

fn run_external_flac_check(label: &str, bytes: &[u8]) {
    let path = format!("/tmp/flac-spike-{label}.flac");
    fs::write(&path, bytes).unwrap();
    println!("  -> wrote {path} ({} bytes)", bytes.len());

    let test = std::process::Command::new("flac")
        .args(["-t", "--totally-silent", &path])
        .status();
    match test {
        Ok(s) if s.success() => println!("  -> `flac -t` (reference C decoder): OK"),
        Ok(s) => println!("  -> `flac -t` (reference C decoder): FAILED (exit {s})"),
        Err(e) => println!("  -> `flac -t` could not run: {e} (install via `brew install flac` / `apt install flac`)"),
    }

    let meta = std::process::Command::new("metaflac")
        .args(["--show-md5sum", &path])
        .output();
    if let Ok(o) = meta {
        let md5 = String::from_utf8_lossy(&o.stdout);
        println!("  -> embedded STREAMINFO md5: {}", md5.trim());
    }
}

fn pcm_as_i16(pcm: &[i32]) -> Vec<i16> {
    pcm.iter().map(|&s| s as i16).collect()
}

fn main() {
    let pcm = generate_pcm();
    println!(
        "Test signal: {} frames ({:.3}s) @ {} Hz, {} ch, {}-bit, {} blocks of {} frames",
        pcm.len() / CHANNELS,
        DURATION_SECS,
        SAMPLE_RATE,
        CHANNELS,
        BITS,
        (pcm.len() / CHANNELS).div_ceil(BLOCK_FRAMES),
        BLOCK_FRAMES,
    );
    let expected = pcm_as_i16(&pcm);

    let candidates: Vec<(&str, fn(&[i32]) -> Vec<u8>)> = vec![
        ("flacenc", via_flacenc::encode),
        ("oxideav-flac", via_oxideav::encode),
    ];
    for (label, encode_fn) in candidates {
        println!("\n=== {label} ===");
        let flac_bytes = encode_fn(&pcm);
        println!(
            "  encoded: {} bytes (from {} bytes raw PCM)",
            flac_bytes.len(),
            pcm.len() * 4
        );

        run_external_flac_check(label, &flac_bytes);

        match decode_with_symphonia(flac_bytes) {
            Ok(decoded) => {
                if decoded.len() != expected.len() {
                    println!(
                        "  -> symphonia decode: SAMPLE COUNT MISMATCH (got {}, want {})",
                        decoded.len(),
                        expected.len()
                    );
                } else if decoded == expected {
                    println!(
                        "  -> symphonia decode: BIT-EXACT MATCH ({} samples)",
                        decoded.len()
                    );
                } else {
                    let mismatches = decoded
                        .iter()
                        .zip(expected.iter())
                        .filter(|(a, b)| a != b)
                        .count();
                    let first = decoded
                        .iter()
                        .zip(expected.iter())
                        .position(|(a, b)| a != b)
                        .unwrap();
                    println!(
                        "  -> symphonia decode: MISMATCH — {mismatches}/{} samples differ, first at index {first} (got {}, want {})",
                        decoded.len(),
                        decoded[first],
                        expected[first]
                    );
                }
            }
            Err(e) => println!("  -> symphonia decode: ERROR — {e}"),
        }
    }
}
