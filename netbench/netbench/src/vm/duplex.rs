use super::{server::Op, ByteVec};
use bytes::{BufMut, Bytes, BytesMut};
use std::{
    collections::{HashMap, VecDeque},
    io,
};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

type Result<T, E = io::Error> = core::result::Result<T, E>;

const CAPACITY: usize = 4016;
const BUFFER_MIN: usize = 128;
const MIN_SEND_BUFFER: usize = u16::MAX as usize * 2;

pub async fn handle_server_connection<W: AsyncWrite, R: AsyncRead>(
    w: W,
    r: R,
    id: u64,
) -> Result<()> {
    tokio::pin!(w);
    tokio::pin!(r);

    let mut read_buf = BytesMut::with_capacity(CAPACITY);
    let mut read_vec = ByteVec::default();
    let mut write_buf = BytesMut::with_capacity(CAPACITY);
    let mut write_vec = ByteVec::default();
    let mut ops = Ops::default();
    let mut decoder = frame::Decoder::default();

    // send the connection id
    write_buf.put_u64(id);

    loop {
        let read_fut = r.read_buf(&mut read_buf);
        let write_fut = async {
            if write_vec.is_empty() {
                futures::future::pending().await
            } else {
                w.write_buf(&mut write_vec).await
            }
        };

        tokio::select! {
            res = read_fut => {
                let len = res?;
                if len == 0 {
                    return Ok(());
                }

                read_vec.push(read_buf.split_to(len).freeze());

                if read_buf.remaining_mut() < BUFFER_MIN {
                    read_buf = BytesMut::with_capacity(CAPACITY);
                }
            }
            res = write_fut => {
                let len = res?;
                if len == 0 {
                    return Ok(());
                }
            }
        }

        while let Some(frame) = decoder.decode(&mut read_vec)? {
            match frame {
                frame::Frame::Control { op } => ops.push(op)?,
                frame::Frame::Stream { .. } => {
                    // ignore for now
                }
            }
        }

        // TODO add timeout
        ops.apply()?;

        // flush all of the writes
        ops.flush(&mut write_vec);
    }
}

#[derive(Debug, Default)]
struct Ops {
    awaiting: Option<u64>,
    streams: HashMap<u64, Stream>,
    ops: VecDeque<Op>,
    pending_streams: VecDeque<u64>,
}

impl Ops {
    pub fn is_paused(&self) -> bool {
        if let Some(id) = self.awaiting.as_ref() {
            !self.streams.get(id).unwrap().is_active()
        } else {
            true
        }
    }

    pub fn push(&mut self, op: Op) -> Result<()> {
        if !self.is_paused() {
            self.apply_one(op)?;
        } else {
            self.ops.push_back(op);
        }

        Ok(())
    }

    pub fn flush(&mut self, b: &mut ByteVec) {
        let mut pending = vec![];

        loop {
            if b.len() >= MIN_SEND_BUFFER {
                break;
            }

            if let Some(id) = self.pending_streams.pop_front() {
                if self.streams.get_mut(&id).unwrap().write(b) {
                    pending.push(id);
                } else if self.awaiting == Some(id) {
                    self.awaiting = None;
                }
            } else {
                break;
            }
        }

        self.pending_streams.extend(pending);
    }

    pub fn apply(&mut self) -> Result<()> {
        loop {
            if self.is_paused() {
                return Ok(());
            }

            if let Some(op) = self.ops.pop_front() {
                self.apply_one(op)?;
            } else {
                return Ok(());
            }
        }
    }

    fn apply_one(&mut self, op: Op) -> Result<()> {
        match op {
            Op::Wait { timeout } => todo!(),
            Op::BidirectionalStream { id } | Op::UnidirectionalStream { id } => {
                self.streams.insert(id, Stream::new(id));
            }
            Op::Send { id, len } => self.stream_mut(id)?.send(len),
            Op::Reset { id, err } => self.stream_mut(id)?.reset(err),
            Op::Finish { id } => self.stream_mut(id)?.finish(),
            Op::Await { id } => {
                let stream = self.stream(id)?;
                if stream.is_active() {
                    self.awaiting = Some(id);
                }
            }
            Op::Trace { id } => todo!(),
        }
        Ok(())
    }

    fn stream(&self, id: u64) -> Result<&Stream> {
        let stream = self
            .streams
            .get(&id)
            .ok_or_else(|| io::Error::new(io::ErrorKind::Other, "invalid stream id"))?;

        Ok(stream)
    }

    fn stream_mut(&mut self, id: u64) -> Result<&mut Stream> {
        let stream = self
            .streams
            .get_mut(&id)
            .ok_or_else(|| io::Error::new(io::ErrorKind::Other, "invalid stream id"))?;

        if !stream.is_active() {
            self.pending_streams.push_back(id);
        }

        Ok(stream)
    }
}

#[derive(Debug)]
enum Stream {
    Sending {
        id: Bytes,
        data: s2n_quic_core::stream::testing::Data,
        pending: usize,
        finished: bool,
    },
    Reset {
        id: Bytes,
        err: u64,
    },
    Finished,
}

impl Stream {
    pub fn new(id: u64) -> Self {
        let id = Bytes::copy_from_slice(&id.to_be_bytes());
        Self::Sending {
            id,
            data: s2n_quic_core::stream::testing::Data::new(u64::MAX),
            pending: 0,
            finished: false,
        }
    }

    pub fn send(&mut self, len: usize) {
        if let Self::Sending { pending, .. } = self {
            *pending += len;
        }
    }

    pub fn reset(&mut self, err: u64) {
        if let Self::Sending { id, .. } = self {
            *self = Self::Reset {
                id: core::mem::take(id),
                err,
            };
        }
    }

    pub fn finish(&mut self) {
        if let Self::Sending { finished, .. } = self {
            *finished = true;
        }
    }

    pub fn is_active(&self) -> bool {
        match self {
            Self::Sending {
                pending, finished, ..
            } => *pending != 0 || *finished,
            Self::Reset { .. } => true,
            Self::Finished => false,
        }
    }

    pub fn write(&mut self, b: &mut ByteVec) -> bool {
        debug_assert!(self.is_active());

        match self {
            Self::Sending {
                id,
                pending,
                data,
                finished,
            } => {
                let mut has_work = false;

                if *pending > 0 {
                    let len = (*pending).min(u16::MAX as _);
                    let mut chunks = [Bytes::new(), Bytes::new(), Bytes::new(), Bytes::new()];
                    let count = data.send(len, &mut chunks).unwrap();
                    b.push(frame::STREAM.clone());
                    b.push(id.clone());
                    for chunk in &mut chunks[..count] {
                        b.push(core::mem::take(chunk));
                    }
                    *pending -= len;
                    has_work = *pending > 0;
                }

                if *finished {
                    b.push(frame::FINISH.clone());
                    b.push(id.clone());
                    *self = Self::Finished;
                    has_work = false;
                }

                has_work
            }
            Self::Reset { id, err } => {
                b.push(id.clone());
                b.push(Bytes::copy_from_slice(&err.to_be_bytes()));
                *self = Self::Finished;
                false
            }
            Self::Finished => unreachable!(),
        }
    }
}

pub mod frame {
    use super::*;
    use bytes::{Buf, Bytes};
    use core::mem::size_of;
    use enum_primitive_derive::Primitive;
    use num_traits::{AsPrimitive, FromPrimitive};

    pub static STREAM: Bytes = Bytes::from_static(&[1]);
    pub static FINISH: Bytes = Bytes::from_static(&[2]);
    pub static RESET: Bytes = Bytes::from_static(&[3]);

    #[derive(Debug, Primitive)]
    enum Tag {
        Control = 0,
        Stream = 1,
    }

    #[derive(Debug)]
    pub enum Frame {
        Control { op: Op },
        Stream { id: u64, data: Bytes },
    }

    #[derive(Debug, Default)]
    pub struct Decoder {
        tag: Option<Tag>,
        stream: Option<(u64, usize)>,
    }

    impl Decoder {
        pub fn decode<B: Buf>(&mut self, buf: &mut B) -> Result<Option<Frame>> {
            if let Some((id, len)) = self.stream.take() {
                return self.decode_stream(buf, id, len);
            }

            if let Some(tag) = self.tag.take() {
                return self.decode_frame(buf, tag);
            }

            if buf.remaining() < size_of::<u8>() {
                return Ok(None);
            }

            let tag = Tag::from_u8(buf.get_u8())
                .ok_or_else(|| io::Error::new(io::ErrorKind::Other, "invalid frame tag"))?;

            self.decode_frame(buf, tag)
        }

        fn decode_frame<B: Buf>(&mut self, buf: &mut B, tag: Tag) -> Result<Option<Frame>> {
            match tag {
                Tag::Control => {
                    if let Some(op) = Op::decode(buf) {
                        Ok(Some(Frame::Control { op }))
                    } else {
                        self.tag = Some(tag);
                        Ok(None)
                    }
                }
                Tag::Stream => {
                    if buf.remaining() < (size_of::<u64>() + size_of::<u16>()) {
                        self.tag = Some(tag);
                        return Ok(None);
                    }

                    let id = buf.get_u64();
                    let len = buf.get_u16();
                    self.decode_stream(buf, id, len as _)
                }
            }
        }

        fn decode_stream<B: Buf>(
            &mut self,
            buf: &mut B,
            id: u64,
            len: usize,
        ) -> Result<Option<Frame>> {
            if len == 0 {
                return self.decode(buf);
            }

            let chunk_len = buf.chunk().len();

            if chunk_len == 0 {
                self.stream = Some((id, len));
                return Ok(None);
            }

            Ok(if chunk_len >= len {
                let data = buf.copy_to_bytes(len);
                Some(Frame::Stream { id, data })
            } else {
                let data = buf.copy_to_bytes(chunk_len);
                self.stream = Some((id, len - chunk_len));
                Some(Frame::Stream { id, data })
            })
        }
    }
}
