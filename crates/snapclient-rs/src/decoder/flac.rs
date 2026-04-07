//! FLAC decoder using symphonia.
//!
//! The snapserver sends a FLAC stream header (fLaC + STREAMINFO) as the CodecHeader,
//! then individual FLAC frames as WireChunk payloads.

use anyhow::{Result, bail};
use snapcast_proto::SampleFormat;
use snapcast_proto::message::codec_header::CodecHeader;
use symphonia::core::audio::SampleBuffer;
use symphonia::core::codecs::{CODEC_TYPE_FLAC, CodecParameters, DecoderOptions};
use symphonia::core::formats::Packet;

use crate::decoder::Decoder;

/// Parse FLAC STREAMINFO from a codec header payload.
///
/// Layout: "fLaC" (4) + block_header (4) + STREAMINFO (34)
/// STREAMINFO bytes 10-13 contain: sample_rate (20 bits) | channels-1 (3 bits) | bps-1 (5 bits)
fn parse_streaminfo(payload: &[u8]) -> Result<(SampleFormat, CodecParameters)> {
    if payload.len() < 42 {
        bail!(
            "FLAC header too small ({} bytes, need >= 42)",
            payload.len()
        );
    }
    if &payload[0..4] != b"fLaC" {
        bail!("not a FLAC header (missing fLaC magic)");
    }

    // STREAMINFO starts at offset 8 (after 4-byte magic + 4-byte block header)
    let si = &payload[8..];

    // Bytes 10-13 of STREAMINFO (offsets 10..14 from si start):
    // 20 bits sample rate | 3 bits (channels-1) | 5 bits (bps-1) | 36 bits total samples
    let b10 = si[10] as u32;
    let b11 = si[11] as u32;
    let b12 = si[12] as u32;

    let sample_rate = (b10 << 12) | (b11 << 4) | (b12 >> 4);
    let channels = ((b12 >> 1) & 0x07) + 1;
    let bits_per_sample = (((b12 & 0x01) << 4) | (si[13] as u32 >> 4)) + 1;

    let sf = SampleFormat::new(sample_rate, bits_per_sample as u16, channels as u16);

    let mut params = CodecParameters::new();
    params
        .for_codec(CODEC_TYPE_FLAC)
        .with_sample_rate(sample_rate)
        .with_bits_per_sample(bits_per_sample)
        .with_channels(
            symphonia::core::audio::Channels::from_bits(((1u64 << channels) - 1) as u32)
                .unwrap_or(symphonia::core::audio::Channels::FRONT_LEFT),
        )
        .with_extra_data(payload[8..42].to_vec().into_boxed_slice());

    Ok((sf, params))
}

pub struct FlacDecoder {
    decoder: Box<dyn symphonia::core::codecs::Decoder>,
    sample_format: SampleFormat,
    packet_id: u64,
}

impl FlacDecoder {
    fn new_from_params(sf: SampleFormat, params: &CodecParameters) -> Result<Self> {
        let decoder = symphonia::default::get_codecs()
            .make(params, &DecoderOptions::default())
            .map_err(|e| anyhow::anyhow!("failed to create FLAC decoder: {e}"))?;
        Ok(Self {
            decoder,
            sample_format: sf,
            packet_id: 0,
        })
    }
}

impl Decoder for FlacDecoder {
    fn set_header(&mut self, header: &CodecHeader) -> Result<SampleFormat> {
        let (sf, params) = parse_streaminfo(&header.payload)?;
        *self = Self::new_from_params(sf, &params)?;
        Ok(self.sample_format)
    }

    fn decode(&mut self, data: &mut Vec<u8>) -> Result<bool> {
        if data.is_empty() {
            return Ok(false);
        }

        let packet = Packet::new_from_slice(0, self.packet_id, 0, data);
        self.packet_id += 1;

        let decoded = match self.decoder.decode(&packet) {
            Ok(buf) => buf,
            Err(e) => {
                tracing::error!("FLAC decode error: {e}");
                return Ok(false);
            }
        };

        let spec = *decoded.spec();
        let duration = decoded.capacity() as u64;

        let mut sample_buf = SampleBuffer::<i32>::new(duration, spec);
        sample_buf.copy_interleaved_ref(decoded);
        let samples = sample_buf.samples();

        // Convert interleaved i32 samples to the target bit depth
        let bytes_per_sample = self.sample_format.sample_size() as usize;
        let mut out = Vec::with_capacity(samples.len() * bytes_per_sample);

        for &s in samples {
            match bytes_per_sample {
                2 => out.extend_from_slice(&(s as i16).to_le_bytes()),
                4 => out.extend_from_slice(&s.to_le_bytes()),
                _ => out.extend_from_slice(&(s as i16).to_le_bytes()),
            }
        }

        *data = out;
        Ok(true)
    }
}

/// Create a FlacDecoder from a CodecHeader (convenience for factory use).
pub fn create(header: &CodecHeader) -> Result<FlacDecoder> {
    let (sf, params) = parse_streaminfo(&header.payload)?;
    FlacDecoder::new_from_params(sf, &params)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a minimal valid FLAC header: fLaC + STREAMINFO block
    /// for 44100 Hz, 16-bit, 2 channels
    fn flac_header_44100_16_2() -> Vec<u8> {
        let mut h = Vec::new();
        h.extend_from_slice(b"fLaC");
        // Block header: type=0 (STREAMINFO), last=1 → 0x80, length=34
        h.push(0x80);
        h.extend_from_slice(&[0x00, 0x00, 0x22]); // length = 34

        // STREAMINFO (34 bytes), all multi-byte fields are BIG-endian:
        h.extend_from_slice(&[0x00, 0x10]); // min block size = 16
        h.extend_from_slice(&[0x10, 0x00]); // max block size = 4096
        h.extend_from_slice(&[0x00, 0x00, 0x00]); // min frame size
        h.extend_from_slice(&[0x00, 0x00, 0x00]); // max frame size

        // Bytes 10-13: sample_rate(20) | channels-1(3) | bps-1(5) | total_samples high 4 bits
        // 44100 = 0xAC44
        // byte10 = 0xAC44 >> 12 = 0x0A
        // byte11 = (0xAC44 >> 4) & 0xFF = 0xC4
        // byte12 = ((0xAC44 & 0x0F) << 4) | ((2-1) << 1) | ((16-1) >> 4)
        //        = (0x04 << 4) | (1 << 1) | (15 >> 4) = 0x40 | 0x02 | 0x00 = 0x42
        // byte13 = ((16-1) & 0x0F) << 4 = 0xF0
        h.push(0x0A);
        h.push(0xC4);
        h.push(0x42);
        h.push(0xF0);

        // Remaining 4 bytes of total_samples + 16 bytes MD5
        h.extend_from_slice(&[0; 20]);

        assert_eq!(h.len(), 42);
        h
    }

    #[test]
    fn parse_streaminfo_44100_16_2() {
        let payload = flac_header_44100_16_2();
        let (sf, _params) = parse_streaminfo(&payload).unwrap();
        assert_eq!(sf.rate(), 44100);
        assert_eq!(sf.bits(), 16);
        assert_eq!(sf.channels(), 2);
    }

    #[test]
    fn parse_streaminfo_48000_24_2() {
        let mut h = Vec::new();
        h.extend_from_slice(b"fLaC");
        h.push(0x80);
        h.extend_from_slice(&[0x00, 0x00, 0x22]);
        h.extend_from_slice(&[0x10, 0x00]); // min block
        h.extend_from_slice(&[0x10, 0x00]); // max block
        h.extend_from_slice(&[0; 6]); // frame sizes

        // 48000 = 0xBB80
        // byte10 = 0x0B, byte11 = 0xB8, byte12 = 0x02 | (1<<1) | (23>>4) = 0x02|0x02|0x01 = 0x03
        // Wait: channels-1=1, bps-1=23
        // byte12 = (0x00 << 4) | (1 << 1) | (23 >> 4) = 0x00 | 0x02 | 0x01 = 0x03
        // byte13 = (23 & 0x0F) << 4 = 0x70
        h.push(0x0B);
        h.push(0xB8);
        h.push(0x03);
        h.push(0x70);
        h.extend_from_slice(&[0; 20]);

        let (sf, _) = parse_streaminfo(&h).unwrap();
        assert_eq!(sf.rate(), 48000);
        assert_eq!(sf.bits(), 24);
        assert_eq!(sf.channels(), 2);
    }

    #[test]
    fn parse_streaminfo_too_small() {
        assert!(parse_streaminfo(&[0; 10]).is_err());
    }

    #[test]
    fn parse_streaminfo_bad_magic() {
        let mut h = vec![0u8; 42];
        h[0..4].copy_from_slice(b"NOPE");
        assert!(parse_streaminfo(&h).is_err());
    }

    #[test]
    fn create_decoder_from_header() {
        let header = CodecHeader {
            codec: "flac".into(),
            payload: flac_header_44100_16_2(),
        };
        let dec = create(&header);
        if let Err(e) = &dec {
            eprintln!("Error: {e:?}");
        }
        let dec = dec.unwrap();
        assert_eq!(dec.sample_format.rate(), 44100);
    }
}
