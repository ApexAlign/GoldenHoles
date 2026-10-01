//! Minimal bit-level writer/reader plus varint/zigzag helpers. No allocations beyond the output Vec.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecodeError {
    /// Ran off the end of the datagram.
    Truncated,
    /// Value outside the legal range for the field.
    Malformed,
    /// Delta snapshot references a baseline the client no longer holds.
    BaselineMissing(u32),
}

pub type DecodeResult<T> = Result<T, DecodeError>;

#[derive(Default)]
pub struct BitWriter {
    buf: Vec<u8>,
    acc: u64,
    n: u32,
}

impl BitWriter {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn bit_len(&self) -> usize {
        self.buf.len() * 8 + self.n as usize
    }

    /// Writes the low `bits` (<= 32) bits of `v`.
    pub fn write_bits(&mut self, v: u32, bits: u32) {
        debug_assert!(bits <= 32);
        let mask = if bits == 32 { u32::MAX } else { (1u32 << bits) - 1 };
        self.acc |= ((v & mask) as u64) << self.n;
        self.n += bits;
        while self.n >= 8 {
            self.buf.push(self.acc as u8);
            self.acc >>= 8;
            self.n -= 8;
        }
    }

    pub fn write_bool(&mut self, b: bool) {
        self.write_bits(b as u32, 1);
    }

    /// LEB128, 7 bits per byte.
    pub fn write_varint(&mut self, mut v: u32) {
        while v >= 0x80 {
            self.write_bits((v & 0x7f) | 0x80, 8);
            v >>= 7;
        }
        self.write_bits(v, 8);
    }

    pub fn write_zigzag(&mut self, v: i32) {
        self.write_varint(((v << 1) ^ (v >> 31)) as u32);
    }

    pub fn finish(mut self) -> Vec<u8> {
        if self.n > 0 {
            self.buf.push(self.acc as u8);
        }
        self.buf
    }
}

pub struct BitReader<'a> {
    buf: &'a [u8],
    pos: usize, // bit position
}

impl<'a> BitReader<'a> {
    pub fn new(buf: &'a [u8]) -> Self {
        Self { buf, pos: 0 }
    }

    pub fn read_bits(&mut self, bits: u32) -> DecodeResult<u32> {
        debug_assert!(bits <= 32);
        if self.pos + bits as usize > self.buf.len() * 8 {
            return Err(DecodeError::Truncated);
        }
        let mut out = 0u64;
        let mut got = 0u32;
        while got < bits {
            let byte = self.buf[self.pos / 8] as u64;
            let off = (self.pos % 8) as u32;
            let take = (8 - off).min(bits - got);
            out |= ((byte >> off) & ((1 << take) - 1)) << got;
            got += take;
            self.pos += take as usize;
        }
        Ok(out as u32)
    }

    pub fn read_bool(&mut self) -> DecodeResult<bool> {
        Ok(self.read_bits(1)? != 0)
    }

    pub fn read_varint(&mut self) -> DecodeResult<u32> {
        let mut out = 0u32;
        for i in 0..5 {
            let b = self.read_bits(8)?;
            let chunk = b & 0x7f;
            if i == 4 && chunk > 0x0f {
                return Err(DecodeError::Malformed);
            }
            out |= chunk << (7 * i);
            if b & 0x80 == 0 {
                return Ok(out);
            }
        }
        Err(DecodeError::Malformed)
    }

    pub fn read_zigzag(&mut self) -> DecodeResult<i32> {
        let u = self.read_varint()?;
        Ok(((u >> 1) as i32) ^ -((u & 1) as i32))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_mixed() {
        let mut w = BitWriter::new();
        w.write_bits(0b101, 3);
        w.write_varint(300);
        w.write_zigzag(-77);
        w.write_bits(0xABCD, 16);
        w.write_bool(true);
        w.write_zigzag(i32::MIN);
        w.write_varint(u32::MAX);
        let bytes = w.finish();
        let mut r = BitReader::new(&bytes);
        assert_eq!(r.read_bits(3).unwrap(), 0b101);
        assert_eq!(r.read_varint().unwrap(), 300);
        assert_eq!(r.read_zigzag().unwrap(), -77);
        assert_eq!(r.read_bits(16).unwrap(), 0xABCD);
        assert!(r.read_bool().unwrap());
        assert_eq!(r.read_zigzag().unwrap(), i32::MIN);
        assert_eq!(r.read_varint().unwrap(), u32::MAX);
    }

    #[test]
    fn truncated_is_error() {
        let mut r = BitReader::new(&[0x80]);
        assert_eq!(r.read_varint(), Err(DecodeError::Truncated));
    }
}
