//! Shared types used across the protocol.

use std::io::{self, Read, Write};

use byteorder::{LittleEndian, ReadBytesExt, WriteBytesExt};

/// Timestamp with second and microsecond components.
///
/// Matches the C++ `tv` struct used throughout the Snapcast protocol.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Timeval {
    /// Seconds component.
    pub sec: i32,
    /// Microseconds component.
    pub usec: i32,
}

impl Timeval {
    /// Create from microseconds since epoch.
    pub fn from_usec(usec: i64) -> Self {
        Self {
            sec: (usec / 1_000_000) as i32,
            usec: (usec % 1_000_000) as i32,
        }
    }

    /// Convert to microseconds since epoch.
    pub fn to_usec(self) -> i64 {
        self.sec as i64 * 1_000_000 + self.usec as i64
    }

    /// Read a Timeval (8 bytes, little-endian) from a reader.
    pub fn read_from<R: Read>(r: &mut R) -> io::Result<Self> {
        Ok(Self {
            sec: r.read_i32::<LittleEndian>()?,
            usec: r.read_i32::<LittleEndian>()?,
        })
    }

    /// Write a Timeval (8 bytes, little-endian) to a writer.
    pub fn write_to<W: Write>(&self, w: &mut W) -> io::Result<()> {
        w.write_i32::<LittleEndian>(self.sec)?;
        w.write_i32::<LittleEndian>(self.usec)?;
        Ok(())
    }
}

impl std::ops::Add for Timeval {
    type Output = Self;

    fn add(self, rhs: Self) -> Self {
        let mut sec = self.sec + rhs.sec;
        let mut usec = self.usec + rhs.usec;
        if usec >= 1_000_000 {
            sec += usec / 1_000_000;
            usec %= 1_000_000;
        }
        Self { sec, usec }
    }
}

impl std::ops::Sub for Timeval {
    type Output = Self;

    fn sub(self, rhs: Self) -> Self {
        let mut sec = self.sec - rhs.sec;
        let mut usec = self.usec - rhs.usec;
        while usec < 0 {
            sec -= 1;
            usec += 1_000_000;
        }
        Self { sec, usec }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timeval_add() {
        let a = Timeval {
            sec: 1,
            usec: 900_000,
        };
        let b = Timeval {
            sec: 0,
            usec: 200_000,
        };
        let result = a + b;
        assert_eq!(
            result,
            Timeval {
                sec: 2,
                usec: 100_000
            }
        );
    }

    #[test]
    fn timeval_sub() {
        let a = Timeval {
            sec: 2,
            usec: 100_000,
        };
        let b = Timeval {
            sec: 1,
            usec: 900_000,
        };
        let result = a - b;
        assert_eq!(
            result,
            Timeval {
                sec: 0,
                usec: 200_000
            }
        );
    }

    #[test]
    fn timeval_round_trip() {
        let tv = Timeval {
            sec: 1000,
            usec: 500_000,
        };
        let mut buf = Vec::new();
        tv.write_to(&mut buf).unwrap();
        assert_eq!(buf.len(), 8);
        let mut cursor = io::Cursor::new(&buf);
        let decoded = Timeval::read_from(&mut cursor).unwrap();
        assert_eq!(tv, decoded);
    }

    #[test]
    fn timeval_known_bytes() {
        // sec=1000 (0x000003E8), usec=500000 (0x0007A120), little-endian
        let expected: [u8; 8] = [0xE8, 0x03, 0x00, 0x00, 0x20, 0xA1, 0x07, 0x00];
        let tv = Timeval {
            sec: 1000,
            usec: 500_000,
        };
        let mut buf = Vec::new();
        tv.write_to(&mut buf).unwrap();
        assert_eq!(buf.as_slice(), &expected);
    }
}
