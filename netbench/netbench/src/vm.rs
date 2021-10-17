use bytes::{Buf, Bytes};
use core::time::Duration;
use std::collections::VecDeque;

pub mod duplex;
pub mod s2n_quic;

#[derive(Debug)]
pub struct ByteVec {
    chunks: VecDeque<Bytes>,
    len: usize,
}

impl Default for ByteVec {
    fn default() -> Self {
        Self {
            chunks: Default::default(),
            len: 0,
        }
    }
}

impl ByteVec {
    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub fn push(&mut self, chunk: Bytes) {
        if !chunk.is_empty() {
            self.len += chunk.len();
            self.chunks.push_back(chunk);
        }
    }
}

impl Buf for ByteVec {
    fn remaining(&self) -> usize {
        self.len
    }

    fn chunk(&self) -> &[u8] {
        if let Some(front) = self.chunks.front() {
            &front[..]
        } else {
            &[][..]
        }
    }

    fn advance(&mut self, mut cnt: usize) {
        debug_assert!(self.len >= cnt);
        self.len -= cnt;

        while cnt > 0 {
            let mut chunk = self.chunks.pop_front().unwrap();

            if let Some(remaining) = cnt.checked_sub(chunk.len()) {
                cnt = remaining;
                continue;
            }

            chunk.advance(cnt);
            self.chunks.push_front(chunk);
            break;
        }
    }
}

pub mod server {
    use super::*;
    use core::mem::size_of;
    use enum_primitive_derive::Primitive;
    use num_traits::{AsPrimitive, FromPrimitive};

    #[derive(Debug, Primitive)]
    enum Tag {
        Wait = 0,
        BidirectionalStream = 1,
        UnidirectionalStream = 2,
        Send = 3,
        Reset = 4,
        Finish = 5,
        Await = 6,
        Trace = 7,
    }

    #[derive(Clone, Copy, Debug)]
    pub enum Op {
        /// Pause for the specified duration before processing the next op
        Wait { timeout: Duration },
        /// Open a bidirectional stream with an identifier
        BidirectionalStream { id: u64 },
        /// Open a unidirectional stream with an identifier
        UnidirectionalStream { id: u64 },
        /// Send a specific amount of data over the stream id
        Send { id: u64, len: usize },
        /// Send a reset on a stream id
        Reset { id: u64, err: u64 },
        /// Finish the stream
        Finish { id: u64 },
        /// Await a stream operation before moving forward
        Await { id: u64 },
        /// Emit a trace event
        Trace { id: u64 },
    }

    const OP_SIZE: usize = size_of::<u8>() + size_of::<u64>() + size_of::<u64>();

    impl Op {
        pub fn decode<B: Buf>(buf: &mut B) -> Option<Self> {
            if buf.remaining() < OP_SIZE {
                return None;
            }

            let tag = buf.get_u8();
            let a = buf.get_u64();
            let b = buf.get_u64();

            Some(match Tag::from_u8(tag)? {
                Tag::Wait => Op::Wait {
                    timeout: Duration::new(a, b as _),
                },
                Tag::BidirectionalStream => Op::BidirectionalStream { id: a },
                Tag::UnidirectionalStream => Op::UnidirectionalStream { id: a },
                Tag::Send => Op::Send { id: a, len: b as _ },
                Tag::Reset => Op::Reset { id: a, err: b },
                Tag::Finish => Op::Finish { id: a },
                Tag::Await => Op::Await { id: a },
                Tag::Trace => Op::Trace { id: a },
            })
        }
    }
}
