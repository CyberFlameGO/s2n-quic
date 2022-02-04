use super::Owner;
use crate::Result;
use bytes::{Buf, BufMut, Bytes, BytesMut};
use core::{
    mem::MaybeUninit,
    pin::Pin,
    task::{Context, Poll},
};
use futures::ready;
use s2n_quic_core::stream::testing::Data;
use std::{
    collections::{hash_map::Entry, HashMap, HashSet, VecDeque},
    io::{self, IoSlice},
};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};

const DEFUALT_STREAM_WINDOW: u64 = 256000;
const DEFAULT_MAX_STREAMS: u64 = 100;

#[derive(Clone, Debug)]
pub struct Config {
    pub stream_window: u64,
    pub max_streams: u64,
    pub max_stream_data_thresh: u64,
    pub max_stream_frame_len: u32,
    pub max_write_queue_len: usize,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            stream_window: DEFUALT_STREAM_WINDOW,
            max_streams: DEFAULT_MAX_STREAMS,
            max_stream_data_thresh: DEFUALT_STREAM_WINDOW / 2,
            max_stream_frame_len: (1 << 15),
            max_write_queue_len: 250,
        }
    }
}

pub struct Connection<T: AsyncRead + AsyncWrite> {
    inner: Pin<Box<T>>,
    rx_open: bool,
    tx_open: bool,
    frame: Option<frame::Frame>,
    read_buf: ReadBuffer,
    decoder: frame::Decoder,
    write_buf: WriteBuffer,
    stream_controllers: [StreamController; 2],
    streams: [HashMap<u64, Stream>; 2],
    max_stream_data: HashSet<(Owner, u64)>,
    pending_accept: VecDeque<u64>,
    peer_initial_max_stream_data: u64,
    config: Config,
}

impl<T: AsyncRead + AsyncWrite> Connection<T> {
    pub fn new(inner: Pin<Box<T>>, config: Config) -> Self {
        let mut write_buf = WriteBuffer::default();

        if config.stream_window != DEFUALT_STREAM_WINDOW {
            write_buf.push(frame::Frame::InitialMaxStreamData {
                up_to: config.stream_window,
            });
        }

        Self {
            inner,
            rx_open: true,
            tx_open: true,
            frame: None,
            read_buf: Default::default(),
            decoder: Default::default(),
            write_buf,
            stream_controllers: [
                StreamController::new(config.max_streams),
                StreamController::new(100),
            ],
            streams: [Default::default(), Default::default()],
            max_stream_data: Default::default(),
            pending_accept: Default::default(),
            peer_initial_max_stream_data: DEFUALT_STREAM_WINDOW,
            config,
        }
    }

    fn flush_write_buffer(&mut self, cx: &mut Context) -> Result<()> {
        if !self.tx_open {
            return if self.write_buf.is_empty() {
                Ok(())
            } else {
                Err("stream was closed with pending data".into())
            };
        }

        if self.write_buf.is_empty() {
            return Ok(());
        }

        if self.inner.as_ref().is_write_vectored() {
            let chunks = self.write_buf.chunks();

            return match self.inner.as_mut().poll_write_vectored(cx, &chunks) {
                Poll::Ready(result) => {
                    let len = result?;
                    self.write_buf.advance(len);
                    self.write_buf.notify(cx);
                    Ok(())
                }
                Poll::Pending => Ok(()),
            };
        }

        while let Some(mut chunk) = self.write_buf.pop_front() {
            match self.inner.as_mut().poll_write(cx, &chunk) {
                Poll::Ready(result) => {
                    let len = result?;
                    chunk.advance(len);

                    if !chunk.is_empty() {
                        self.write_buf.push_front(chunk);
                    }

                    if len == 0 {
                        return if self.write_buf.is_empty() {
                            self.tx_open = false;
                            Ok(())
                        } else {
                            Err("stream was closed with pending data".into())
                        };
                    }
                }
                Poll::Pending => {
                    self.write_buf.push_front(chunk);
                    break;
                }
            }
        }

        self.write_buf.notify(cx);

        Ok(())
    }

    fn flush_read_buffer(&mut self, cx: &mut Context) -> Result<()> {
        loop {
            if self.frame.is_none() {
                if let Some(frame) = self.decoder.decode(&mut self.read_buf)? {
                    self.frame = Some(frame);
                }
            }

            match self.dispatch_frame(cx) {
                Poll::Ready(result) => result?,
                Poll::Pending => return Ok(()),
            }
        }
    }

    fn dispatch_frame(&mut self, cx: &mut Context) -> Poll<Result<()>> {
        use frame::Frame::*;

        match self.frame.take() {
            Some(StreamOpen { id, bidirectional }) => {
                // TODO make sure the peer hasn't opened too many
                let mut stream = Stream {
                    rx: Some(ReceiveStream::new(self.config.stream_window)),
                    tx: None,
                };
                if bidirectional {
                    stream.tx = Some(SendStream::new(self.peer_initial_max_stream_data));
                };
                self.streams[Owner::Remote].insert(id, stream);
                self.pending_accept.push_back(id);
                cx.waker().wake_by_ref();
            }
            Some(StreamData {
                id,
                owner,
                mut data,
            }) => {
                if let Some(rx) = self.streams[owner]
                    .get_mut(&id)
                    .and_then(|stream| stream.rx.as_mut())
                {
                    let len = data.len() as u64;
                    let len = rx.buffer(len, cx)?;
                    data.advance(len as _);

                    if !data.is_empty() {
                        self.frame = Some(StreamData { id, owner, data });
                        return Poll::Pending;
                    }
                }
            }
            Some(MaxStreams { up_to }) => {
                self.stream_controllers[Owner::Remote].max_streams(up_to, cx);
            }
            Some(MaxStreamData { id, owner, up_to }) => {
                if let Some(tx) = self.streams[owner]
                    .get_mut(&id)
                    .and_then(|stream| stream.tx.as_mut())
                {
                    tx.max_data(up_to, cx);
                }
            }
            Some(StreamFinish { id, owner }) => {
                if let Some(rx) = self.streams[owner]
                    .get_mut(&id)
                    .ok_or("invalid stream")?
                    .rx
                    .as_mut()
                {
                    rx.finish(cx);
                }
            }
            Some(InitialMaxStreamData { up_to }) => {
                self.peer_initial_max_stream_data = up_to;
            }
            None => return Poll::Pending,
        }

        Ok(()).into()
    }
}

impl<T: AsyncRead + AsyncWrite> super::Connection for Connection<T> {
    fn poll_open_bidirectional_stream(&mut self, id: u64, _cx: &mut Context) -> Poll<Result<()>> {
        ready!(self.stream_controllers[Owner::Local].open());

        self.write_buf.push(frame::Frame::StreamOpen {
            id,
            bidirectional: true,
        });

        let stream = Stream {
            rx: Some(ReceiveStream::new(self.config.stream_window)),
            tx: Some(SendStream::new(self.peer_initial_max_stream_data)),
        };
        self.streams[Owner::Local].insert(id, stream);

        Ok(()).into()
    }

    fn poll_open_send_stream(&mut self, id: u64, _cx: &mut Context) -> Poll<Result<()>> {
        ready!(self.stream_controllers[Owner::Local].open());

        self.write_buf.push(frame::Frame::StreamOpen {
            id,
            bidirectional: false,
        });

        let stream = Stream {
            rx: None,
            tx: Some(SendStream::new(self.peer_initial_max_stream_data)),
        };
        self.streams[Owner::Local].insert(id, stream);

        Ok(()).into()
    }

    fn poll_accept_stream(&mut self, _cx: &mut Context) -> Poll<Result<Option<u64>>> {
        if !self.rx_open {
            return Ok(None).into();
        }

        if let Some(id) = self.pending_accept.pop_front() {
            Ok(Some(id)).into()
        } else {
            Poll::Pending
        }
    }

    fn poll_send(
        &mut self,
        owner: Owner,
        id: u64,
        bytes: u64,
        _cx: &mut Context,
    ) -> Poll<Result<u64>> {
        if !self.write_buf.request_push(self.config.max_write_queue_len) {
            return Poll::Pending;
        }

        let stream = self.streams[owner]
            .get_mut(&id)
            .ok_or("missing stream")?
            .tx
            .as_mut()
            .ok_or("missing tx stream")?;

        let allowed_bytes = bytes.min(self.config.max_stream_frame_len as _);

        if let Some(data) = stream.send(allowed_bytes) {
            let len = data.len() as u64;
            self.write_buf.push(frame::Frame::StreamData {
                id,
                owner: !owner,
                data,
            });
            Ok(len).into()
        } else {
            Poll::Pending
        }
    }

    fn poll_receive(
        &mut self,
        owner: Owner,
        id: u64,
        bytes: u64,
        _cx: &mut Context,
    ) -> Poll<Result<u64>> {
        let stream = self.streams[owner]
            .get_mut(&id)
            .ok_or("missing stream")?
            .rx
            .as_mut()
            .ok_or("missing rx stream")?;

        let len = ready!(stream.receive(bytes))?;

        if stream.receive_window() < self.config.stream_window / 2 {
            self.max_stream_data.insert((owner, id));
        }

        Ok(len).into()
    }

    fn poll_send_finish(&mut self, owner: Owner, id: u64, _cx: &mut Context) -> Poll<Result<()>> {
        if !self.write_buf.request_push(self.config.max_write_queue_len) {
            return Poll::Pending;
        }

        if let Entry::Occupied(mut entry) = self.streams[owner].entry(id) {
            let stream = entry.get_mut();

            if stream.tx.take().is_some() {
                self.write_buf
                    .push(frame::Frame::StreamFinish { id, owner: !owner });
            }

            if stream.rx.is_none() {
                entry.remove();
            }
        }

        Ok(()).into()
    }

    fn poll_receive_finish(
        &mut self,
        owner: Owner,
        id: u64,
        _cx: &mut Context,
    ) -> Poll<Result<()>> {
        if let Entry::Occupied(mut entry) = self.streams[owner].entry(id) {
            let stream = entry.get_mut();
            stream.rx = None;
            if stream.tx.is_none() {
                entry.remove();
            }
        }

        Ok(()).into()
    }

    fn poll_progress(&mut self, cx: &mut Context) -> Poll<Result<()>> {
        loop {
            for (owner, id) in self.max_stream_data.drain() {
                let stream = self.streams[owner]
                    .get_mut(&id)
                    .unwrap()
                    .rx
                    .as_mut()
                    .unwrap();
                let up_to = stream.credit(self.config.stream_window);
                self.write_buf.push_priority(frame::Frame::MaxStreamData {
                    id,
                    owner: !owner,
                    up_to,
                });
            }

            self.flush_write_buffer(cx)?;

            self.flush_read_buffer(cx)?;

            // only read from the socket if it's open and we don't have a pending frame
            if !(self.rx_open && self.frame.is_none()) {
                return Ok(()).into();
            }

            let rx_open = &mut self.rx_open;
            let inner = self.inner.as_mut();

            ready!(self.read_buf.read(|buf| {
                ready!(inner.poll_read(cx, buf))?;

                // the socket returned a 0 write
                if buf.filled().is_empty() {
                    *rx_open = false;
                }

                Ok(()).into()
            }))?;
        }
    }

    fn poll_finish(&mut self, cx: &mut Context) -> Poll<Result<()>> {
        self.flush_write_buffer(cx)?;

        if !self.write_buf.is_empty() {
            return Poll::Pending;
        }

        ready!(self.inner.as_mut().poll_flush(cx))?;
        Ok(()).into()
    }
}

#[derive(Debug)]
struct Stream {
    rx: Option<ReceiveStream>,
    tx: Option<SendStream>,
}

#[derive(Debug)]
struct ReceiveStream {
    received_offset: u64,
    buffered_offset: u64,
    window_offset: u64,
    is_finished: bool,
    blocked: Blocked,
}

impl ReceiveStream {
    pub fn new(window_offset: u64) -> Self {
        Self {
            received_offset: 0,
            buffered_offset: 0,
            window_offset,
            is_finished: false,
            blocked: Default::default(),
        }
    }

    pub fn buffer(&mut self, len: u64, cx: &mut Context) -> Result<u64> {
        if self.is_finished {
            return Err("stream is already finished".into());
        }

        let len = (self.window_offset - self.buffered_offset).min(len);

        if len == 0 {
            return Ok(0);
        }

        self.buffered_offset += len;

        self.blocked.unblock(cx);

        Ok(len)
    }

    pub fn receive(&mut self, len: u64) -> Poll<Result<u64>> {
        let len = (self.buffered_offset - self.received_offset).min(len);

        if len == 0 {
            return if self.is_finished {
                Ok(0).into()
            } else {
                self.blocked.block();
                Poll::Pending
            };
        }

        self.received_offset += len;

        Ok(len).into()
    }

    pub fn finish(&mut self, cx: &mut Context) {
        self.is_finished = true;
        self.blocked.unblock(cx);
    }

    pub fn receive_window(&self) -> u64 {
        self.window_offset - self.received_offset
    }

    pub fn credit(&mut self, credits: u64) -> u64 {
        self.window_offset += credits;
        self.window_offset
    }
}

#[derive(Debug, Default)]
struct SendStream {
    max_data: u64,
    data: Data,
    blocked: Blocked,
}

impl SendStream {
    pub fn new(max_data: u64) -> Self {
        Self {
            max_data,
            data: Data::new(u64::MAX),
            blocked: Default::default(),
        }
    }

    pub fn max_data(&mut self, max_data: u64, cx: &mut Context) {
        self.max_data = max_data.max(self.max_data);
        self.blocked.unblock(cx)
    }

    pub fn send(&mut self, len: u64) -> Option<Bytes> {
        let window = self.max_data - self.data.offset();
        let len = len.min(window) as usize;

        if len == 0 {
            self.blocked.block();
            return None;
        }

        self.data.send_one(len)
    }
}

#[derive(Debug, Default)]
struct ReadBuffer {
    len: usize,
    tail: BytesMut,
    head: VecDeque<BytesMut>,
}

impl ReadBuffer {
    pub fn read<F: FnOnce(&mut ReadBuf) -> Poll<Result<()>>>(
        &mut self,
        read: F,
    ) -> Poll<Result<()>> {
        let capacity = self.tail.capacity();

        // const CAPACITY: usize = 5 * 1_000_000_000;
        const CAPACITY: usize = 2 << 12;

        if capacity == 0 {
            self.tail.reserve(CAPACITY);
        } else if capacity < 32 {
            let tail = core::mem::replace(&mut self.tail, BytesMut::with_capacity(CAPACITY));
            self.head.push_back(tail);
        }

        let buf = self.tail.chunk_mut();
        let buf = unsafe { &mut *(buf as *mut _ as *mut [MaybeUninit<u8>]) };
        let mut buf = ReadBuf::uninit(buf);
        ready!(read(&mut buf))?;
        let len = buf.filled().len();

        if len == 0 {
            return Ok(()).into();
        }

        unsafe {
            self.tail.advance_mut(len);
            self.len += len;
        }

        Ok(()).into()
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    fn check_consistency(&self) {
        if cfg!(debug_assertions) {
            let mut actual_len = self.tail.len();
            for chunk in self.head.iter() {
                actual_len += chunk.len();
            }
            assert_eq!(actual_len, self.remaining(), "{:?}", self);
        }
    }
}

impl Buf for ReadBuffer {
    fn remaining(&self) -> usize {
        self.len
    }

    fn chunk(&self) -> &[u8] {
        for chunk in self.head.iter() {
            if !chunk.is_empty() {
                return chunk.chunk();
            }
        }

        self.tail.chunk()
    }

    fn advance(&mut self, mut cnt: usize) {
        while cnt > 0 {
            let len = if let Some(front) = self.head.front_mut() {
                let len = front.len().min(cnt);
                front.advance(len);
                if front.is_empty() {
                    let _ = self.head.pop_front();
                }
                len
            } else {
                self.tail.advance(cnt);
                cnt
            };

            cnt -= len;
            self.len -= len;
        }

        self.check_consistency();
    }

    fn copy_to_bytes(&mut self, len: usize) -> Bytes {
        while let Some(mut front) = self.head.pop_front() {
            if front.is_empty() {
                continue;
            }

            self.len -= len;

            if front.len() == len {
                self.check_consistency();
                return front.freeze();
            }

            let out = front.split_to(len);
            self.head.push_front(front);

            self.check_consistency();
            return out.freeze();
        }

        let out = self.tail.split_to(len).freeze();

        self.len -= len;
        self.check_consistency();

        out
    }
}

#[derive(Debug, Default)]
struct WriteBuffer {
    unfilled: BytesMut,
    queue: VecDeque<Bytes>,
    push_interest: bool,
}

impl WriteBuffer {
    pub fn request_push(&mut self, max_queue_len: usize) -> bool {
        let can_push = max_queue_len > self.queue.len();
        self.push_interest |= !can_push;
        can_push
    }

    pub fn push(&mut self, frame: frame::Frame) {
        let capacity = self.unfilled.capacity();

        const CAPACITY: usize = 4096;

        if capacity == 0 {
            self.unfilled.reserve(CAPACITY);
        } else if capacity < 32 {
            self.unfilled = BytesMut::with_capacity(CAPACITY);
        }

        frame.write_header(&mut self.unfilled);
        self.queue.push_back(self.unfilled.split().freeze());

        if let Some(data) = frame.body() {
            self.queue.push_back(data);
        }
    }

    pub fn push_priority(&mut self, frame: frame::Frame) {
        let capacity = self.unfilled.capacity();

        const CAPACITY: usize = 4096;

        if capacity == 0 {
            self.unfilled.reserve(CAPACITY);
        } else if capacity < 32 {
            self.unfilled = BytesMut::with_capacity(CAPACITY);
        }

        frame.write_header(&mut self.unfilled);
        let header = self.unfilled.split().freeze();

        // push the body first
        if let Some(data) = frame.body() {
            self.queue.push_front(data);
        }

        self.queue.push_front(header);
    }

    pub fn is_empty(&self) -> bool {
        self.queue.is_empty()
    }

    pub fn pop_front(&mut self) -> Option<Bytes> {
        self.queue.pop_front()
    }

    pub fn push_front(&mut self, chunk: Bytes) {
        self.queue.push_front(chunk);
    }

    pub fn notify(&mut self, cx: &mut Context) {
        if self.push_interest {
            self.push_interest = false;
            cx.waker().wake_by_ref();
        }
    }

    pub fn advance(&mut self, mut len: usize) {
        while let Some(mut chunk) = self.queue.pop_front() {
            let next_len = len.saturating_sub(chunk.len());

            if next_len > 0 {
                len = next_len;
                continue;
            }

            if chunk.len() > len {
                chunk.advance(len);
                self.queue.push_front(chunk);
            }

            return;
        }
    }

    pub fn chunks(&self) -> Vec<IoSlice> {
        self.queue.iter().map(|v| IoSlice::new(v)).collect()
    }
}

#[derive(Debug, Default)]
struct StreamController {
    open_offset: u64,
    window_offset: u64,
    blocked: Blocked,
}

impl StreamController {
    pub fn new(window_offset: u64) -> Self {
        Self {
            open_offset: 0,
            window_offset,
            blocked: Default::default(),
        }
    }

    pub fn open(&mut self) -> Poll<()> {
        if self.capacity() == 0 {
            self.blocked.block();
            return Poll::Pending;
        }

        self.open_offset += 1;

        Poll::Ready(())
    }

    pub fn max_streams(&mut self, up_to: u64, cx: &mut Context) {
        self.window_offset = up_to;
        self.blocked.unblock(cx)
    }

    pub fn capacity(&self) -> u64 {
        self.window_offset - self.open_offset
    }
}

#[derive(Debug, Default)]
struct Blocked(bool);

impl Blocked {
    pub fn block(&mut self) {
        self.0 = true;
    }

    pub fn unblock(&mut self, cx: &mut Context) {
        if self.0 {
            self.0 = false;
            cx.waker().wake_by_ref();
        }
    }
}

pub mod frame {
    use super::*;
    use bytes::{Buf, BufMut, Bytes};
    use core::mem::size_of;
    use enum_primitive_derive::Primitive;
    use num_traits::FromPrimitive;

    #[derive(Clone, Copy, Debug, Primitive)]
    #[repr(u8)]
    enum Tag {
        StreamOpen = 0,
        StreamData = 1,
        StreamFinish = 2,
        MaxStreams = 3,
        MaxStreamData = 4,
        InitialMaxStreamData = 5,
    }

    #[derive(Clone, Debug)]
    pub enum Frame {
        StreamOpen { id: u64, bidirectional: bool },
        StreamData { id: u64, owner: Owner, data: Bytes },
        StreamFinish { id: u64, owner: Owner },
        MaxStreams { up_to: u64 },
        MaxStreamData { id: u64, owner: Owner, up_to: u64 },
        InitialMaxStreamData { up_to: u64 },
    }

    impl Frame {
        pub fn write_header<B: BufMut>(&self, buf: &mut B) {
            match self {
                Self::StreamOpen { id, bidirectional } => {
                    buf.put_u8(Tag::StreamOpen as _);
                    buf.put_u64(*id);
                    buf.put_u8(*bidirectional as u8);
                }
                Self::StreamData { id, owner, data } => {
                    buf.put_u8(Tag::StreamData as _);
                    buf.put_u64(*id);
                    buf.put_u8(*owner as _);
                    buf.put_u32(data.len() as _);
                }
                Self::StreamFinish { id, owner } => {
                    buf.put_u8(Tag::StreamFinish as _);
                    buf.put_u64(*id);
                    buf.put_u8(*owner as _);
                }
                Self::MaxStreams { up_to } => {
                    buf.put_u8(Tag::MaxStreams as _);
                    buf.put_u64(*up_to);
                }
                Self::MaxStreamData { id, owner, up_to } => {
                    buf.put_u8(Tag::MaxStreamData as _);
                    buf.put_u64(*id);
                    buf.put_u8(*owner as _);
                    buf.put_u64(*up_to);
                }
                Self::InitialMaxStreamData { up_to } => {
                    buf.put_u8(Tag::InitialMaxStreamData as _);
                    buf.put_u64(*up_to);
                }
            }
        }

        pub fn body(self) -> Option<Bytes> {
            if let Self::StreamData { data, .. } = self {
                Some(data)
            } else {
                None
            }
        }
    }

    #[derive(Debug, Default)]
    pub struct Decoder {
        tag: Option<Tag>,
        stream: Option<(u64, Owner, usize)>,
    }

    impl Decoder {
        pub fn decode<B: Buf>(&mut self, buf: &mut B) -> Result<Option<Frame>> {
            if let Some((id, owner, len)) = self.stream.take() {
                return self.decode_stream(buf, id, owner, len);
            }

            if let Some(tag) = self.tag.take() {
                return self.decode_frame(buf, tag);
            }

            if buf.remaining() == 0 {
                return Ok(None);
            }

            let tag = Tag::from_u8(buf.get_u8())
                .ok_or_else(|| io::Error::new(io::ErrorKind::Other, "invalid frame tag"))?;

            self.decode_frame(buf, tag)
        }

        fn decode_frame<B: Buf>(&mut self, buf: &mut B, tag: Tag) -> Result<Option<Frame>> {
            match tag {
                Tag::StreamOpen => {
                    if buf.remaining() < (size_of::<u64>() + size_of::<u8>()) {
                        self.tag = Some(tag);
                        return Ok(None);
                    }

                    let id = buf.get_u64();
                    let bidirectional = buf.get_u8() != 0;

                    Ok(Some(Frame::StreamOpen { id, bidirectional }))
                }
                Tag::StreamData => {
                    if buf.remaining() < (size_of::<u64>() + size_of::<u8>() + size_of::<u32>()) {
                        self.tag = Some(tag);
                        return Ok(None);
                    }

                    let id = buf.get_u64();
                    let owner = Self::decode_owner(buf)?;
                    let len = buf.get_u32();
                    self.decode_stream(buf, id, owner, len as _)
                }
                Tag::StreamFinish => {
                    if buf.remaining() < (size_of::<u64>() + size_of::<u8>()) {
                        self.tag = Some(tag);
                        return Ok(None);
                    }

                    let id = buf.get_u64();
                    let owner = Self::decode_owner(buf)?;

                    Ok(Some(Frame::StreamFinish { id, owner }))
                }
                Tag::MaxStreams => {
                    if buf.remaining() < size_of::<u64>() {
                        self.tag = Some(tag);
                        return Ok(None);
                    }

                    let up_to = buf.get_u64();

                    Ok(Some(Frame::MaxStreams { up_to }))
                }
                Tag::MaxStreamData => {
                    if buf.remaining() < (size_of::<u64>() + size_of::<u8>() + size_of::<u64>()) {
                        self.tag = Some(tag);
                        return Ok(None);
                    }

                    let id = buf.get_u64();
                    let owner = Self::decode_owner(buf)?;
                    let up_to = buf.get_u64();

                    Ok(Some(Frame::MaxStreamData { id, owner, up_to }))
                }
                Tag::InitialMaxStreamData => {
                    if buf.remaining() < (size_of::<u64>()) {
                        self.tag = Some(tag);
                        return Ok(None);
                    }

                    let up_to = buf.get_u64();

                    Ok(Some(Frame::InitialMaxStreamData { up_to }))
                }
            }
        }

        fn decode_owner<B: Buf>(buf: &mut B) -> Result<Owner> {
            let owner = buf.get_u8();
            Ok(match owner {
                0 => Owner::Local,
                1 => Owner::Remote,
                _ => {
                    return Err(
                        io::Error::new(io::ErrorKind::InvalidData, "invalid owner id").into(),
                    )
                }
            })
        }

        fn decode_stream<B: Buf>(
            &mut self,
            buf: &mut B,
            id: u64,
            owner: Owner,
            len: usize,
        ) -> Result<Option<Frame>> {
            if len == 0 {
                return self.decode(buf);
            }

            let chunk_len = buf.chunk().len();

            if chunk_len == 0 {
                self.stream = Some((id, owner, len));
                return Ok(None);
            }

            Ok(if chunk_len >= len {
                let data = buf.copy_to_bytes(len);
                Some(Frame::StreamData { id, owner, data })
            } else {
                let data = buf.copy_to_bytes(chunk_len);
                self.stream = Some((id, owner, len - chunk_len));
                Some(Frame::StreamData { id, owner, data })
            })
        }
    }
}
