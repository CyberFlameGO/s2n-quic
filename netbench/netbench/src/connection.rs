use crate::{op::ConnectionOp, Result};
use core::{
    future::Future,
    pin::Pin,
    task::{Context, Poll},
    time::Duration,
};
use futures::ready;
use std::collections::{HashMap, HashSet};
use tokio::time::Sleep;

pub mod multiplexed;
#[cfg(feature = "s2n-quic")]
pub mod s2n_quic;

pub struct Driver<'a, C: Connection> {
    pub connection: C,
    local_thread: Thread<'a>,
    local_rates: Rates,
    peer_streams: Vec<(Poll<()>, Thread<'a>)>,
    peer_rates: Rates,
    can_accept: bool,
    is_finished: bool,
}

impl<'a, C: Connection> Driver<'a, C> {
    pub fn new(scenario: &'a crate::scenario::Connection, connection: C) -> Self {
        Self {
            connection,
            local_thread: Thread::new(&scenario.ops, Owner::Local),
            local_rates: Default::default(),
            peer_streams: scenario
                .peer_streams
                .iter()
                .map(|ops| (Poll::Pending, Thread::new(ops, Owner::Remote)))
                .collect(),
            peer_rates: Default::default(),
            can_accept: true,
            is_finished: false,
        }
    }

    pub async fn run<T: Trace, Ch: Checkpoints>(
        mut self,
        trace: &mut T,
        checkpoints: &mut Ch,
    ) -> Result<C> {
        futures::future::poll_fn(|cx| self.poll(trace, checkpoints, cx)).await?;
        Ok(self.connection)
    }

    pub fn poll<T: Trace, Ch: Checkpoints>(
        &mut self,
        trace: &mut T,
        checkpoints: &mut Ch,
        cx: &mut Context,
    ) -> Poll<Result<()>> {
        if self.is_finished {
            return self.connection.poll_finish(cx);
        }

        let mut poll_accept = false;
        let mut all_ready = true;

        trace.enter(0, 0);
        let result = self.local_thread.poll(
            &mut self.connection,
            trace,
            checkpoints,
            &mut self.local_rates,
            cx,
        );
        trace.exit();

        match result {
            Poll::Ready(Ok(_)) => {}
            Poll::Ready(Err(err)) => return Err(err).into(),
            Poll::Pending => all_ready = false,
        }

        for (idx, (accepted, thread)) in self.peer_streams.iter_mut().enumerate() {
            // if we're still waiting to accept this stream move on
            if accepted.is_pending() {
                all_ready = false;
                poll_accept = self.can_accept;
                continue;
            }

            trace.enter(1, idx);
            let result = thread.poll(
                &mut self.connection,
                trace,
                checkpoints,
                &mut self.peer_rates,
                cx,
            );
            trace.exit();

            match result {
                Poll::Ready(Ok(_)) => {}
                Poll::Ready(Err(err)) => return Err(err).into(),
                Poll::Pending => all_ready = false,
            }
        }

        if poll_accept {
            match self.connection.poll_accept_stream(cx) {
                Poll::Ready(Ok(Some(id))) => {
                    trace.accept(id);
                    if let Some((accepted, _)) = self.peer_streams.get_mut(id as usize) {
                        *accepted = Poll::Ready(());
                        cx.waker().wake_by_ref();
                    } else {
                        todo!("return a not found error")
                    }
                }
                Poll::Ready(Ok(None)) => self.can_accept = false,
                Poll::Ready(Err(err)) => return Err(err).into(),
                Poll::Pending => all_ready = false,
            }
        }

        if all_ready {
            self.is_finished = true;
            self.connection.poll_finish(cx)
        } else {
            ready!(self.connection.poll_progress(cx))?;
            Poll::Pending
        }
    }
}

#[derive(Debug, Default)]
struct Rates {
    send: HashMap<u64, (u64, Duration)>,
    receive: HashMap<u64, (u64, Duration)>,
}

#[derive(Debug)]
pub struct Thread<'a> {
    ops: &'a [ConnectionOp],
    index: usize,
    op: Option<Op<'a>>,
    timer: Timer,
    owner: Owner,
}

impl<'a> Thread<'a> {
    pub fn new(ops: &'a [ConnectionOp], owner: Owner) -> Self {
        Self {
            ops,
            index: 0,
            op: None,
            timer: Timer::default(),
            owner,
        }
    }

    fn poll<C: Connection, T: Trace, Ch: Checkpoints>(
        &mut self,
        conn: &mut C,
        trace: &mut T,
        checkpoints: &mut Ch,
        rates: &mut Rates,
        cx: &mut Context,
    ) -> Poll<Result<()>> {
        loop {
            while self.op.is_none() {
                if let Some(op) = self.ops.get(self.index) {
                    self.index += 1;
                    self.on_op(op, trace, checkpoints, rates, cx);
                } else {
                    // we are all done processing the operations
                    return Poll::Ready(Ok(()));
                }
            }

            ready!(self.poll_op(conn, trace, checkpoints, rates, cx))?;
            self.op = None;
        }
    }

    fn on_op<T: Trace, Ch: Checkpoints>(
        &mut self,
        op: &'a ConnectionOp,
        trace: &mut T,
        checkpoints: &mut Ch,
        rates: &mut Rates,
        cx: &mut Context,
    ) {
        trace.exec(op);
        match op {
            ConnectionOp::Sleep { amount } => {
                self.timer.sleep(*amount);
                self.op = Some(Op::Sleep);
            }
            ConnectionOp::BidirectionalStream { stream_id } => {
                self.op = Some(Op::OpenBidirectionalStream { id: *stream_id });
            }
            ConnectionOp::SendStream { stream_id } => {
                self.op = Some(Op::OpenSendStream { id: *stream_id });
            }
            ConnectionOp::Send { stream_id, bytes } => {
                self.op = Some(Op::Send {
                    id: *stream_id,
                    remaining: *bytes,
                    rate: rates.send.get(stream_id).cloned(),
                });
            }
            ConnectionOp::SendFinish { stream_id } => {
                self.op = Some(Op::SendFinish { id: *stream_id });
            }
            ConnectionOp::SendRate {
                stream_id,
                bytes,
                period,
            } => {
                rates.send.insert(*stream_id, (*bytes, *period));
            }
            ConnectionOp::Receive { stream_id, bytes } => {
                self.op = Some(Op::Receive {
                    id: *stream_id,
                    remaining: *bytes,
                    rate: rates.receive.get(stream_id).cloned(),
                });
            }
            ConnectionOp::ReceiveAll { stream_id } => {
                self.op = Some(Op::Receive {
                    id: *stream_id,
                    remaining: u64::MAX,
                    rate: rates.receive.get(stream_id).cloned(),
                });
            }
            ConnectionOp::ReceiveFinish { stream_id } => {
                self.op = Some(Op::ReceiveFinish { id: *stream_id });
            }
            ConnectionOp::ReceiveRate {
                stream_id,
                bytes,
                period,
            } => {
                rates.receive.insert(*stream_id, (*bytes, *period));
            }
            ConnectionOp::Trace { trace_id } => {
                trace.trace(*trace_id);
            }
            ConnectionOp::Park { checkpoint } => {
                self.op = Some(Op::Wait {
                    checkpoint: *checkpoint,
                });
            }
            ConnectionOp::Unpark { checkpoint } => {
                // notify the checkpoint that it can make progress
                checkpoints.unpark(*checkpoint);
                // re-poll the operations since we may be unblocking another task
                cx.waker().wake_by_ref();
            }
            ConnectionOp::Scope { threads } => {
                if !threads.is_empty() {
                    let threads = threads
                        .iter()
                        .map(|thread| Thread::new(thread, self.owner))
                        .collect();
                    self.op = Some(Op::Scope { threads });
                }
            }
        }
    }

    fn poll_op<C: Connection, T: Trace, Ch: Checkpoints>(
        &mut self,
        conn: &mut C,
        trace: &mut T,
        checkpoints: &mut Ch,
        rates: &mut Rates,
        cx: &mut Context,
    ) -> Poll<Result<()>> {
        let owner = self.owner;
        match self.op.as_mut().unwrap() {
            Op::Sleep => {
                ready!(self.timer.poll(cx));
                Poll::Ready(Ok(()))
            }
            Op::OpenBidirectionalStream { id } => conn.poll_open_bidirectional_stream(*id, cx),
            Op::OpenSendStream { id } => conn.poll_open_send_stream(*id, cx),
            Op::Send {
                id,
                remaining,
                rate,
            } => self.timer.transfer(remaining, rate, cx, |bytes, cx| {
                let amount = ready!(conn.poll_send(owner, *id, bytes, cx))?;
                trace.send(*id, amount);
                Ok(amount).into()
            }),
            Op::SendFinish { id } => conn.poll_send_finish(owner, *id, cx),
            Op::Receive {
                id,
                remaining,
                rate,
            } => self.timer.transfer(remaining, rate, cx, |bytes, cx| {
                let amount = ready!(conn.poll_receive(owner, *id, bytes, cx))?;
                trace.receive(*id, amount);
                Ok(amount).into()
            }),
            Op::ReceiveFinish { id } => conn.poll_receive_finish(owner, *id, cx),
            Op::Wait { checkpoint } => {
                ready!(checkpoints.park(*checkpoint));
                Ok(()).into()
            }
            Op::Scope { threads } => {
                let mut all_ready = true;
                let op_idx = self.index;
                for (idx, thread) in threads.iter_mut().enumerate() {
                    trace.enter(op_idx, idx);
                    let result = thread.poll(conn, trace, checkpoints, rates, cx);
                    trace.exit();
                    match result {
                        Poll::Ready(Ok(_)) => {}
                        Poll::Ready(Err(err)) => return Err(err).into(),
                        Poll::Pending => all_ready = false,
                    }
                }
                if all_ready {
                    Poll::Ready(Ok(()))
                } else {
                    Poll::Pending
                }
            }
        }
    }
}

#[derive(Debug)]
enum Op<'a> {
    Sleep,
    OpenBidirectionalStream {
        id: u64,
    },
    OpenSendStream {
        id: u64,
    },
    Send {
        id: u64,
        remaining: u64,
        rate: Option<(u64, Duration)>,
    },
    SendFinish {
        id: u64,
    },
    Receive {
        id: u64,
        remaining: u64,
        rate: Option<(u64, Duration)>,
    },
    ReceiveFinish {
        id: u64,
    },
    Wait {
        checkpoint: u64,
    },
    Scope {
        threads: Vec<Thread<'a>>,
    },
}

#[derive(Debug, Default)]
struct Timer {
    sleep: Option<Pin<Box<Sleep>>>,
    is_armed: bool,
    window: u64,
}

impl Timer {
    fn poll(&mut self, cx: &mut Context) -> Poll<()> {
        if !self.is_armed {
            return Poll::Ready(());
        }

        if let Some(timer) = self.sleep.as_mut() {
            ready!(timer.as_mut().poll(cx));
            self.is_armed = false;
        }

        Poll::Ready(())
    }

    fn sleep(&mut self, duration: Duration) {
        let deadline = tokio::time::Instant::now() + duration;
        if let Some(timer) = self.sleep.as_mut() {
            Sleep::reset(timer.as_mut(), deadline);
        } else {
            self.sleep = Some(Box::pin(tokio::time::sleep_until(deadline)));
        }
        self.is_armed = true;
    }

    fn transfer<F: FnMut(u64, &mut Context) -> Poll<Result<u64>>>(
        &mut self,
        remaining: &mut u64,
        rate: &Option<(u64, Duration)>,
        cx: &mut Context,
        mut f: F,
    ) -> Poll<Result<()>> {
        loop {
            if let Some((bytes, period)) = rate.as_ref() {
                if self.poll(cx).is_ready() {
                    self.window = *bytes.min(remaining);
                    self.sleep(*period);
                }

                if self.window == 0 {
                    return Poll::Pending;
                }

                let amount = ready!(f(self.window, cx))?;

                if amount == 0 {
                    return Poll::Pending;
                }

                *remaining -= amount;
                self.window -= amount;
            } else {
                *remaining -= ready!(f(*remaining, cx))?;
            }

            if *remaining == 0 {
                self.window = 0;
                return Poll::Ready(Ok(()));
            }
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Owner {
    Local,
    Remote,
}

impl<T> core::ops::Index<Owner> for [T; 2] {
    type Output = T;

    fn index(&self, index: Owner) -> &Self::Output {
        match index {
            Owner::Local => &self[0],
            Owner::Remote => &self[1],
        }
    }
}

impl<T> core::ops::IndexMut<Owner> for [T; 2] {
    fn index_mut(&mut self, index: Owner) -> &mut Self::Output {
        match index {
            Owner::Local => &mut self[0],
            Owner::Remote => &mut self[1],
        }
    }
}

impl core::ops::Not for Owner {
    type Output = Self;

    fn not(self) -> Self::Output {
        match self {
            Self::Local => Self::Remote,
            Self::Remote => Self::Local,
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct IdPrefixReader {
    bytes: [u8; core::mem::size_of::<u64>()],
    cursor: u8,
}

impl IdPrefixReader {
    pub fn remaining(&mut self) -> &mut [u8] {
        &mut self.bytes[self.cursor as usize..]
    }

    pub fn on_read(&mut self, len: usize) -> Poll<u64> {
        let cursor = self.cursor as usize + len;
        if cursor >= self.bytes.len() {
            Poll::Ready(u64::from_be_bytes(self.bytes))
        } else {
            self.cursor = cursor as u8;
            Poll::Pending
        }
    }
}

pub trait Connection {
    fn poll_open_bidirectional_stream(&mut self, id: u64, cx: &mut Context) -> Poll<Result<()>>;
    fn poll_open_send_stream(&mut self, id: u64, cx: &mut Context) -> Poll<Result<()>>;
    fn poll_accept_stream(&mut self, cx: &mut Context) -> Poll<Result<Option<u64>>>;
    fn poll_send(
        &mut self,
        owner: Owner,
        id: u64,
        bytes: u64,
        cx: &mut Context,
    ) -> Poll<Result<u64>>;
    fn poll_receive(
        &mut self,
        owner: Owner,
        id: u64,
        bytes: u64,
        cx: &mut Context,
    ) -> Poll<Result<u64>>;
    fn poll_send_finish(&mut self, owner: Owner, id: u64, cx: &mut Context) -> Poll<Result<()>>;
    fn poll_receive_finish(&mut self, owner: Owner, id: u64, cx: &mut Context) -> Poll<Result<()>>;
    fn poll_progress(&mut self, cx: &mut Context) -> Poll<Result<()>> {
        let _ = cx;
        Ok(()).into()
    }
    fn poll_finish(&mut self, cx: &mut Context) -> Poll<Result<()>> {
        let _ = cx;
        Ok(()).into()
    }
}

pub trait Checkpoints {
    fn park(&mut self, id: u64) -> Poll<()>;
    fn unpark(&mut self, id: u64);
}

impl Checkpoints for HashSet<u64> {
    fn park(&mut self, id: u64) -> Poll<()> {
        if self.remove(&id) {
            Poll::Ready(())
        } else {
            Poll::Pending
        }
    }

    fn unpark(&mut self, id: u64) {
        self.insert(id);
    }
}

pub trait Trace {
    #[inline(always)]
    fn exec(&mut self, op: &ConnectionOp) {
        let _ = op;
    }

    #[inline(always)]
    fn enter(&mut self, scope: usize, thread: usize) {
        let _ = scope;
        let _ = thread;
    }

    #[inline(always)]
    fn exit(&mut self) {}

    #[inline(always)]
    fn send(&mut self, stream_id: u64, len: u64) {
        let _ = stream_id;
        let _ = len;
    }

    #[inline(always)]
    fn receive(&mut self, stream_id: u64, len: u64) {
        let _ = stream_id;
        let _ = len;
    }

    #[inline(always)]
    fn accept(&mut self, stream_id: u64) {
        let _ = stream_id;
    }

    #[inline(always)]
    fn trace(&mut self, id: u64) {
        let _ = id;
    }
}

pub mod trace {
    use super::*;
    use core::fmt;
    use std::sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc,
    };

    #[derive(Clone, Debug, Default)]
    pub struct Disabled(());

    impl Trace for Disabled {}

    #[derive(Clone, Debug)]
    pub struct Logger<'a> {
        id: u64,
        traces: &'a [String],
        scope: Vec<(usize, usize)>,
    }

    impl<'a> Logger<'a> {
        pub fn new(id: u64, traces: &'a [String]) -> Self {
            Self {
                id,
                traces,
                scope: vec![],
            }
        }

        fn log(&self, v: impl fmt::Display) {
            use std::io::Write;

            let out = std::io::stdout();
            let mut out = out.lock();
            let _ = write!(out, "{}:", self.id);
            for (scope, thread) in self.scope.iter() {
                let _ = write!(out, "{}.{}:", scope, thread);
            }
            let _ = writeln!(out, "{}", v);
        }
    }

    impl Trace for Logger<'_> {
        #[inline(always)]
        fn exec(&mut self, op: &ConnectionOp) {
            self.log(format_args!("exec: {:?}", op));
        }

        #[inline(always)]
        fn enter(&mut self, scope: usize, thread: usize) {
            self.scope.push((scope, thread));
        }

        #[inline(always)]
        fn exit(&mut self) {
            self.scope.pop();
        }

        #[inline(always)]
        fn send(&mut self, stream_id: u64, len: u64) {
            self.log(format_args!("send: stream={}, len={}", stream_id, len));
        }

        #[inline(always)]
        fn receive(&mut self, stream_id: u64, len: u64) {
            self.log(format_args!("recv: stream={}, len={}", stream_id, len));
        }

        #[inline(always)]
        fn accept(&mut self, stream_id: u64) {
            self.log(format_args!("accept: stream={}", stream_id));
        }

        #[inline(always)]
        fn trace(&mut self, id: u64) {
            if let Some(msg) = self.traces.get(id as usize) {
                self.log(format_args!("trace: {}", msg));
            } else {
                self.log(format_args!("trace: id={}", id));
            }
        }
    }

    #[derive(Clone, Debug, Default)]
    pub struct Throughput<Counter> {
        rx: Counter,
        tx: Counter,
    }

    impl Throughput<Arc<AtomicU64>> {
        pub fn take(&self) -> Throughput<u64> {
            Throughput {
                rx: self.rx.swap(0, Ordering::Relaxed),
                tx: self.tx.swap(0, Ordering::Relaxed),
            }
        }

        pub fn reporter(&self, freq: Duration) -> ReporterHandle {
            let handle = ReporterHandle::default();
            let values = self.clone();
            let r_handle = handle.clone();
            tokio::spawn(async move {
                while !r_handle.0.fetch_or(false, Ordering::Relaxed) {
                    tokio::time::sleep(freq).await;
                    let v = values.take();
                    eprintln!("{:?}", v);
                }
            });

            handle
        }
    }

    impl Trace for Throughput<Arc<AtomicU64>> {
        fn send(&mut self, _stream_id: u64, len: u64) {
            self.tx.fetch_add(len, Ordering::Relaxed);
        }

        fn receive(&mut self, _stream_id: u64, len: u64) {
            self.rx.fetch_add(len, Ordering::Relaxed);
        }
    }

    #[derive(Clone, Debug, Default)]
    pub struct ReporterHandle(Arc<AtomicBool>);

    impl Drop for ReporterHandle {
        fn drop(&mut self) {
            self.0.store(true, Ordering::Relaxed);
        }
    }
}
