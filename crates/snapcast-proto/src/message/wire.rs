//! Wire format helpers for length-prefixed strings and byte arrays.

use std::io::{Read, Write};

use byteorder::{LittleEndian, ReadBytesExt, WriteBytesExt};

use crate::message::base::ProtoError;

/// Read a length-prefixed string (u32 LE length + UTF-8 bytes).
pub fn read_string<R: Read>(r: &mut R) -> Result<String, ProtoError> {
    let len = r.read_u32::<LittleEndian>()? as usize;
    let mut buf = vec![0u8; len];
    r.read_exact(&mut buf)?;
    Ok(String::from_utf8_lossy(&buf).into_owned())
}

/// Write a length-prefixed string (u32 LE length + UTF-8 bytes).
pub fn write_string<W: Write>(w: &mut W, s: &str) -> Result<(), ProtoError> {
    w.write_u32::<LittleEndian>(s.len() as u32)?;
    w.write_all(s.as_bytes())?;
    Ok(())
}

/// Read a length-prefixed byte array (u32 LE length + bytes).
pub fn read_bytes<R: Read>(r: &mut R) -> Result<Vec<u8>, ProtoError> {
    let len = r.read_u32::<LittleEndian>()? as usize;
    let mut buf = vec![0u8; len];
    r.read_exact(&mut buf)?;
    Ok(buf)
}

/// Write a length-prefixed byte array (u32 LE length + bytes).
pub fn write_bytes<W: Write>(w: &mut W, data: &[u8]) -> Result<(), ProtoError> {
    w.write_u32::<LittleEndian>(data.len() as u32)?;
    w.write_all(data)?;
    Ok(())
}

/// Size of a length-prefixed string on the wire.
pub fn string_wire_size(s: &str) -> u32 {
    4 + s.len() as u32
}

/// Size of a length-prefixed byte array on the wire.
pub fn bytes_wire_size(data: &[u8]) -> u32 {
    4 + data.len() as u32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_string() {
        let mut buf = Vec::new();
        write_string(&mut buf, "flac").unwrap();
        // 4 bytes length + 4 bytes "flac"
        assert_eq!(buf, [0x04, 0x00, 0x00, 0x00, b'f', b'l', b'a', b'c']);
        let mut cursor = std::io::Cursor::new(&buf);
        let s = read_string(&mut cursor).unwrap();
        assert_eq!(s, "flac");
    }

    #[test]
    fn round_trip_bytes() {
        let data = vec![0xDE, 0xAD, 0xBE, 0xEF];
        let mut buf = Vec::new();
        write_bytes(&mut buf, &data).unwrap();
        let mut cursor = std::io::Cursor::new(&buf);
        let decoded = read_bytes(&mut cursor).unwrap();
        assert_eq!(decoded, data);
    }

    #[test]
    fn empty_string() {
        let mut buf = Vec::new();
        write_string(&mut buf, "").unwrap();
        assert_eq!(buf, [0x00, 0x00, 0x00, 0x00]);
        let mut cursor = std::io::Cursor::new(&buf);
        let s = read_string(&mut cursor).unwrap();
        assert_eq!(s, "");
    }
}
