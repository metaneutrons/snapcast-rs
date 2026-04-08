//! Opus decoder using opus-decoder (pure Rust, no C dependencies).
//!
//! The snapserver sends a custom "pseudo header":
//!   4 bytes: ID (0x4F505553 as LE u32)
//!   4 bytes: sample rate (LE u32)
//!   2 bytes: bits per sample (LE u16)
//!   2 bytes: channels (LE u16)

use anyhow::{Result, bail};
use opus_decoder::OpusDecoder as OpusDec;
use snapcast_proto::SampleFormat;
use snapcast_proto::message::codec_header::CodecHeader;

use crate::decoder::Decoder;

const OPUS_ID: u32 = 0x4F50_5553;
const MAX_FRAME_SIZE: usize = 2880;

/// Parse the Opus pseudo header.
fn parse_opus_header(payload: &[u8]) -> Result<SampleFormat> {
    if payload.len() < 12 {
        bail!(
            "Opus header too small ({} bytes, need >= 12)",
            payload.len()
        );
    }
    let id = u32::from_le_bytes(payload[0..4].try_into().unwrap());
    if id != OPUS_ID {
        bail!("not an Opus header (expected 0x{OPUS_ID:08X}, got 0x{id:08X})");
    }
    let rate = u32::from_le_bytes(payload[4..8].try_into().unwrap());
    let bits = u16::from_le_bytes(payload[8..10].try_into().unwrap());
    let channels = u16::from_le_bytes(payload[10..12].try_into().unwrap());
    Ok(SampleFormat::new(rate, bits, channels))
}

/// Opus audio decoder (pure Rust).
pub struct OpusDecoder {
    decoder: OpusDec,
    sample_format: SampleFormat,
    pcm_buf: Vec<i16>,
}

impl Decoder for OpusDecoder {
    fn set_header(&mut self, header: &CodecHeader) -> Result<SampleFormat> {
        let sf = parse_opus_header(&header.payload)?;
        self.decoder = OpusDec::new(sf.rate(), sf.channels() as usize)
            .map_err(|e| anyhow::anyhow!("failed to create Opus decoder: {e}"))?;
        self.sample_format = sf;
        self.pcm_buf
            .resize(MAX_FRAME_SIZE * sf.channels() as usize, 0);
        Ok(sf)
    }

    fn decode(&mut self, data: &mut Vec<u8>) -> Result<bool> {
        if data.is_empty() {
            return Ok(false);
        }
        let decoded_samples = match self
            .decoder
            .decode(data.as_slice(), &mut self.pcm_buf, false)
        {
            Ok(n) => n,
            Err(e) => {
                tracing::error!("Opus decode error: {e}");
                return Ok(false);
            }
        };
        let total_samples = decoded_samples * self.sample_format.channels() as usize;
        let mut out = Vec::with_capacity(total_samples * 2);
        for &s in &self.pcm_buf[..total_samples] {
            out.extend_from_slice(&s.to_le_bytes());
        }
        *data = out;
        Ok(true)
    }
}

/// Create an OpusDecoder from a CodecHeader.
pub fn create(header: &CodecHeader) -> Result<OpusDecoder> {
    let sf = parse_opus_header(&header.payload)?;
    let dec = OpusDec::new(sf.rate(), sf.channels() as usize)
        .map_err(|e| anyhow::anyhow!("failed to create Opus decoder: {e}"))?;
    Ok(OpusDecoder {
        decoder: dec,
        sample_format: sf,
        pcm_buf: vec![0i16; MAX_FRAME_SIZE * sf.channels() as usize],
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn opus_header(rate: u32, bits: u16, channels: u16) -> Vec<u8> {
        let mut h = Vec::new();
        h.extend_from_slice(&OPUS_ID.to_le_bytes());
        h.extend_from_slice(&rate.to_le_bytes());
        h.extend_from_slice(&bits.to_le_bytes());
        h.extend_from_slice(&channels.to_le_bytes());
        h
    }

    #[test]
    fn parse_header_48000_16_2() {
        let sf = parse_opus_header(&opus_header(48000, 16, 2)).unwrap();
        assert_eq!(sf.rate(), 48000);
        assert_eq!(sf.bits(), 16);
        assert_eq!(sf.channels(), 2);
    }

    #[test]
    fn parse_header_too_small() {
        assert!(parse_opus_header(&[0; 8]).is_err());
    }

    #[test]
    fn parse_header_bad_magic() {
        let mut h = opus_header(48000, 16, 2);
        h[0] = 0xFF;
        assert!(parse_opus_header(&h).is_err());
    }

    #[test]
    fn create_decoder_48000_16_2() {
        let header = CodecHeader {
            codec: "opus".into(),
            payload: opus_header(48000, 16, 2),
        };
        let dec = create(&header);
        assert!(dec.is_ok());
    }
}
