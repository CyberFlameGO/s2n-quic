use bytes::{Buf, BufMut, Bytes, BytesMut};

#[derive(Clone, Copy, Debug)]
pub struct Eof;

pub trait TryBuf {
    fn remaining(&self) -> usize;

    fn read_slice(&mut self, dst: &mut [u8]) -> Result<(), Eof>;

    #[inline]
    fn read_u8(&mut self) -> Result<u8, Eof> {
        let mut v = [0; 1];
        self.read_slice(&mut v)?;
        Ok(v[0])
    }

    #[inline]
    fn read_u16(&mut self) -> Result<u16, Eof> {
        let mut v = [0; 2];
        self.read_slice(&mut v)?;
        Ok(u16::from_be_bytes(v))
    }

    #[inline]
    fn read_u32(&mut self) -> Result<u32, Eof> {
        let mut v = [0; 4];
        self.read_slice(&mut v)?;
        Ok(u32::from_be_bytes(v))
    }

    #[inline]
    fn read_u64(&mut self) -> Result<u64, Eof> {
        let mut v = [0; 8];
        self.read_slice(&mut v)?;
        Ok(u64::from_be_bytes(v))
    }
}

pub trait TryBufMut {
    fn remaining_mut(&self) -> usize;

    fn write_slice(&mut self, src: &[u8]) -> Result<(), Eof>;

    #[inline]
    fn write_u8(&mut self, v: u8) -> Result<(), Eof> {
        self.write_slice(&[v])
    }

    #[inline]
    fn write_u16(&mut self, v: u16) -> Result<(), Eof> {
        self.write_slice(&v.to_be_bytes())
    }

    #[inline]
    fn write_u32(&mut self, v: u32) -> Result<(), Eof> {
        self.write_slice(&v.to_be_bytes())
    }

    #[inline]
    fn write_u64(&mut self, v: u64) -> Result<(), Eof> {
        self.write_slice(&v.to_be_bytes())
    }
}

impl TryBuf for &[u8] {
    #[inline]
    fn remaining(&self) -> usize {
        self.len()
    }

    #[inline]
    fn read_slice(&mut self, dst: &mut [u8]) -> Result<(), Eof> {
        if dst.len() > self.len() {
            return Err(Eof);
        }
        let (source, remaining) = self.split_at(dst.len());
        dst.copy_from_slice(source);
        *self = remaining;
        Ok(())
    }
}

impl TryBuf for Bytes {
    #[inline]
    fn remaining(&self) -> usize {
        Buf::remaining(self)
    }

    #[inline]
    fn read_slice(&mut self, dst: &mut [u8]) -> Result<(), Eof> {
        if dst.len() > self.remaining() {
            return Err(Eof);
        }
        self.copy_to_slice(dst);
        Ok(())
    }
}

impl TryBuf for BytesMut {
    #[inline]
    fn remaining(&self) -> usize {
        Buf::remaining(self)
    }

    #[inline]
    fn read_slice(&mut self, dst: &mut [u8]) -> Result<(), Eof> {
        if dst.len() > self.remaining() {
            return Err(Eof);
        }
        self.copy_to_slice(dst);
        Ok(())
    }
}

impl TryBufMut for BytesMut {
    #[inline]
    fn remaining_mut(&self) -> usize {
        BufMut::remaining_mut(self)
    }

    #[inline]
    fn write_slice(&mut self, src: &[u8]) -> Result<(), Eof> {
        if src.len() > self.remaining_mut() {
            return Err(Eof);
        }
        self.put_slice(src);
        Ok(())
    }
}
