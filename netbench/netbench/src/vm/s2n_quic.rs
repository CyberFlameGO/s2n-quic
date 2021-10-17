use super::{server::Op, ByteVec};
use bytes::Bytes;
use core::{
    convert::TryInto,
    future::Future,
    pin::Pin,
    task::{Context, Poll},
    time::Duration,
};
use s2n_quic::stream::{BidirectionalStream, PeerStream};
use std::collections::HashMap;
use tokio::sync::mpsc;

const CHUNK_LEN: usize = 64;

type Result<T, E = s2n_quic::stream::Error> = core::result::Result<T, E>;

pub async fn handle_server(mut server: s2n_quic::Server) -> Result<()> {
    while let Some(connection) = server.accept().await {
        tokio::spawn(handle_server_connection(connection));
    }
    Ok(())
}

pub async fn handle_server_connection(mut connection: s2n_quic::Connection) -> Result<()> {
    let mut control = if let Some(stream) = connection.accept_bidirectional_stream().await? {
        stream
    } else {
        return Ok(());
    };

    // tell the client our connection id
    let id = connection.id();
    let id = id.to_be_bytes();
    let id = Bytes::copy_from_slice(&id);
    control.send(id).await?;

    let (handle, mut acceptor) = connection.split();
    let mut streams = StreamSet::new(handle);
    let mut control = Control::new(control);
    let mut op_fut = OpResult::None;
    let sleep = tokio::time::sleep(Duration::from_secs(60));
    tokio::pin!(sleep);

    loop {
        let control_task = control.recv();
        let acceptor_task = acceptor.accept();
        let op_task =
            futures::future::poll_fn(|cx| op_fut.poll_apply(cx, &mut streams, &mut sleep));

        tokio::select! {
            _ = op_task => {
                // nothing to do
            }
            _ = control_task => {
                // nothing to do
            }
            stream = acceptor_task => {
                if let Some(stream) = stream? {
                    match stream {
                        PeerStream::Receive(receive) => {
                            tokio::spawn(handle_receive_stream(receive));
                        }
                        PeerStream::Bidirectional(stream) => {
                            let (mut receive, send) = stream.split();

                            if let Ok(id) = read_u64(&mut receive).await {
                                tokio::spawn(handle_receive_stream(receive));
                                streams.insert_send_stream(id, send);
                            }
                        }
                    }
                }
            }
        }

        while let Some(op) = control.op() {
            match streams.apply(op, &mut sleep) {
                Ok(OpResult::None) => {}
                Ok(other) => {
                    op_fut = other;
                    break;
                }
                Err(_) => {
                    // unidentified stream id
                    control.restore(op);
                    break;
                }
            }
        }
    }
}

struct Control {
    stream: BidirectionalStream,
    restored: Option<Op>,
    recv: Vec<Bytes>,
    buffer: ByteVec,
}

impl Control {
    pub fn new(stream: BidirectionalStream) -> Self {
        Self {
            stream,
            restored: None,
            recv: vec![Bytes::new(); CHUNK_LEN],
            buffer: ByteVec::default(),
        }
    }

    pub async fn recv(&mut self) -> Result<()> {
        let (count, _is_open) = self.stream.receive_vectored(&mut self.recv).await?;

        for chunk in &mut self.recv[..count] {
            self.buffer.push(core::mem::take(chunk));
        }

        Ok(())
    }

    pub fn op(&mut self) -> Option<Op> {
        if let Some(op) = self.restored.take() {
            return Some(op);
        }

        Op::decode(&mut self.buffer)
    }

    pub fn restore(&mut self, op: Op) {
        debug_assert!(self.restored.is_none());
        self.restored = Some(op);
    }
}

struct StreamSet {
    handle: s2n_quic::connection::Handle,
    tasks: HashMap<u64, StreamTask>,
    sync: (mpsc::UnboundedSender<()>, mpsc::UnboundedReceiver<()>),
}

enum OpResult {
    None,
    Sleep,
    Await,
}

impl OpResult {
    pub fn poll_apply(
        &mut self,
        cx: &mut Context,
        streams: &mut StreamSet,
        sleep: &mut Pin<&mut tokio::time::Sleep>,
    ) -> Poll<()> {
        let res = match self {
            Self::None => Poll::Pending,
            Self::Sleep => sleep.as_mut().poll(cx),
            Self::Await => streams.sync.1.poll_recv(cx).map(|_| ()),
        };

        if res.is_ready() {
            *self = Self::None;
        }

        res
    }
}

struct StreamTask {
    messages: mpsc::UnboundedSender<Message>,
}

impl StreamSet {
    pub fn new(handle: s2n_quic::connection::Handle) -> Self {
        Self {
            handle,
            tasks: Default::default(),
            sync: mpsc::unbounded_channel(),
        }
    }

    pub fn insert_send_stream(&mut self, id: u64, send: s2n_quic::stream::SendStream) {
        let sync = self.sync.0.clone();
        let (messages, queue) = mpsc::unbounded_channel();
        tokio::spawn(handle_send_stream(send, queue, sync));
        self.tasks.insert(id, StreamTask { messages });
    }

    pub fn apply(
        &mut self,
        operation: Op,
        sleep: &mut Pin<&mut tokio::time::Sleep>,
    ) -> Result<OpResult, ()> {
        match operation {
            Op::Wait { timeout } => {
                let deadline = tokio::time::Instant::now() + timeout;
                sleep.as_mut().reset(deadline);
                return Ok(OpResult::Sleep);
            }
            Op::BidirectionalStream { id } => {
                let mut conn = self.handle.clone();
                let sync = self.sync.0.clone();
                let (messages, queue) = mpsc::unbounded_channel();
                tokio::spawn(async move {
                    let stream = conn.open_bidirectional_stream().await?;
                    let (receive, send) = stream.split();

                    tokio::spawn(handle_receive_stream(receive));

                    handle_send_stream(send, queue, sync).await?;

                    <Result<()>>::Ok(())
                });
                self.tasks.insert(id, StreamTask { messages });
            }
            Op::UnidirectionalStream { id } => {
                let mut conn = self.handle.clone();
                let sync = self.sync.0.clone();
                let (messages, queue) = mpsc::unbounded_channel();
                tokio::spawn(async move {
                    let send = conn.open_send_stream().await?;

                    handle_send_stream(send, queue, sync).await?;

                    <Result<()>>::Ok(())
                });
                self.tasks.insert(id, StreamTask { messages });
            }
            Op::Send { id, len } => {
                let _ = self
                    .tasks
                    .get(&id)
                    .ok_or(())?
                    .messages
                    .send(Message::Send(len));
            }
            Op::Reset { id, err } => {
                let _ = self
                    .tasks
                    .get(&id)
                    .ok_or(())?
                    .messages
                    .send(Message::Reset(err));
            }
            Op::Finish { id } => {
                let _ = self
                    .tasks
                    .get(&id)
                    .ok_or(())?
                    .messages
                    .send(Message::Finish);
            }
            Op::Await { id } => {
                let _ = self.tasks.get(&id).ok_or(())?.messages.send(Message::Await);
                return Ok(OpResult::Await);
            }
            Op::Trace { id } => todo!(),
        }

        Ok(OpResult::None)
    }
}

enum Message {
    Send(usize),
    Reset(u64),
    Finish,
    Await,
}

/// Executes messages on a send stream
async fn handle_send_stream(
    mut stream: s2n_quic::stream::SendStream,
    mut queue: mpsc::UnboundedReceiver<Message>,
    sync: mpsc::UnboundedSender<()>,
) -> Result<()> {
    let mut chunks = vec![Bytes::new(); CHUNK_LEN];

    let mut data = s2n_quic_core::stream::testing::Data::new(u64::MAX);

    while let Some(message) = queue.recv().await {
        match message {
            Message::Send(len) => {
                let count = data.send(len, &mut chunks).unwrap();
                stream.send_vectored(&mut chunks[..count]).await?;
            }
            Message::Reset(code) => {
                stream.reset(code.try_into().unwrap())?;
            }
            Message::Finish => {
                stream.finish()?;
            }
            Message::Await => {
                let _ = sync.send(());
            }
        }
    }

    Ok(())
}

/// Drains a receive stream
async fn handle_receive_stream(mut stream: s2n_quic::stream::ReceiveStream) -> Result<()> {
    let mut chunks = vec![Bytes::new(); CHUNK_LEN];

    loop {
        let (len, is_open) = stream.receive_vectored(&mut chunks).await?;

        if !is_open {
            break;
        }

        for chunk in chunks[..len].iter_mut() {
            // discard chunks
            *chunk = Bytes::new();
        }
    }

    Ok(())
}

pub async fn read_u64(stream: &mut s2n_quic::stream::ReceiveStream) -> Result<u64> {
    let mut offset = 0;
    let mut id = [0u8; core::mem::size_of::<u64>()];

    while offset < id.len() {
        let chunk = stream
            .receive()
            .await?
            .expect("every stream should be prefixed with the scenario ID");

        let needed_len = id.len() - offset;
        let len = chunk.len().min(needed_len);

        id[offset..offset + len].copy_from_slice(&chunk[..len]);
        offset += len;
    }

    let id = u64::from_be_bytes(id);

    Ok(id)
}
