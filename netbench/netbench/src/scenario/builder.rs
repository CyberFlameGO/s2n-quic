use crate::op::{ClientOp, ConnectionOp, RouterOp, ScenarioId};
use core::{fmt, time::Duration};
use std::{cell::RefCell, collections::HashMap, marker::PhantomData, rc::Rc};

/*
impl Scenario {
    pub fn create_router(&self) -> Router {
        let routers = &mut self.state.borrow_mut().routers;
        let id = routers.len() as u64;

        let router = Router {
            id,
            ops: Ops::new(),
        };

        routers.push(router.clone());

        router
    }
}

#[derive(Clone, Debug)]
pub struct Router {
    id: u64,
    ops: Ops<RouterOp>,
}

impl Router {
    pub fn sleep(&self, amount: Duration) -> &Self {
        self.ops.push(RouterOp::Sleep { amount });
        self
    }

    pub fn set_server_buffer_count(&self, packet_count: u32) -> &Self {
        self.ops.push(RouterOp::ServerBufferCount { packet_count });
        self
    }

    pub fn set_server_drop_rate(&self, packet_count: u32) -> &Self {
        self.ops.push(RouterOp::ServerDropRate { packet_count });
        self
    }

    pub fn set_server_reorder_rate(&self, packet_count: u32) -> &Self {
        self.ops.push(RouterOp::ServerReorderRate { packet_count });
        self
    }

    pub fn set_server_corrput_rate(&self, packet_count: u32) -> &Self {
        self.ops.push(RouterOp::ServerCorruptRate { packet_count });
        self
    }

    pub fn set_server_delay(&self, amount: Duration) -> &Self {
        self.ops.push(RouterOp::ServerDelay { amount });
        self
    }

    pub fn set_server_jitter(&self, amount: Duration) -> &Self {
        self.ops.push(RouterOp::ServerJitter { amount });
        self
    }

    pub fn set_server_mtu(&self, mtu: u16) -> &Self {
        self.ops.push(RouterOp::ServerMtu { mtu });
        self
    }

    pub fn set_client_buffer_count(&self, packet_count: u32) -> &Self {
        self.ops.push(RouterOp::ClientBufferCount { packet_count });
        self
    }

    pub fn set_client_drop_rate(&self, packet_count: u32) -> &Self {
        self.ops.push(RouterOp::ClientDropRate { packet_count });
        self
    }

    pub fn set_client_reorder_rate(&self, packet_count: u32) -> &Self {
        self.ops.push(RouterOp::ClientReorderRate { packet_count });
        self
    }

    pub fn set_client_corrput_rate(&self, packet_count: u32) -> &Self {
        self.ops.push(RouterOp::ClientCorruptRate { packet_count });
        self
    }

    pub fn set_client_delay(&self, amount: Duration) -> &Self {
        self.ops.push(RouterOp::ClientDelay { amount });
        self
    }

    pub fn set_client_jitter(&self, amount: Duration) -> &Self {
        self.ops.push(RouterOp::ClientJitter { amount });
        self
    }

    pub fn set_client_mtu(&self, mtu: u16) -> &Self {
        self.ops.push(RouterOp::ClientMtu { mtu });
        self
    }

    pub fn set_client_rebind_port_rate(&self, packet_count: u32) -> &Self {
        self.ops
            .push(RouterOp::ClientRebindPortRate { packet_count });
        self
    }

    pub fn set_client_rebind_addr_rate(&self, packet_count: u32) -> &Self {
        self.ops
            .push(RouterOp::ClientRebindAddressRate { packet_count });
        self
    }

    pub fn rebind_ports(&self) -> &Self {
        self.ops.push(RouterOp::RebindAll {
            ports: true,
            addresses: false,
        });
        self
    }

    pub fn rebind_addresses(&self) -> &Self {
        self.ops.push(RouterOp::RebindAll {
            ports: false,
            addresses: true,
        });
        self
    }

    pub fn trace(&self, trace_id: u64) -> &Self {
        self.ops.push(RouterOp::Trace { trace_id });
        self
    }
}
*/

macro_rules! sync {
    ($endpoint:ty, $location:ty) => {
        pub fn park(&mut self, checkpoint: Checkpoint<$endpoint, $location, Park>) -> &mut Self {
            self.ops.push(ConnectionOp::Park {
                checkpoint: checkpoint.id,
            });
            self
        }

        pub fn unpark(
            &mut self,
            checkpoint: Checkpoint<$endpoint, $location, Unpark>,
        ) -> &mut Self {
            self.ops.push(ConnectionOp::Unpark {
                checkpoint: checkpoint.id,
            });
            self
        }
    };
}

#[derive(Debug)]
pub struct Park;

#[derive(Debug)]
pub struct Unpark;

macro_rules! sleep {
    () => {
        pub fn sleep(&mut self, amount: Duration) -> &mut Self {
            self.ops.push(ConnectionOp::Sleep { amount });
            self
        }
    };
}

macro_rules! trace {
    () => {
        pub fn trace(&mut self, name: &str) -> &mut Self {
            let trace_id = self.state.trace(name);
            self.ops.push(ConnectionOp::Trace { trace_id });
            self
        }
    };
}

#[derive(Debug)]
pub struct ScenarioBuilder {
    state: ScenarioState,
}

impl ScenarioBuilder {
    pub(super) fn new() -> Self {
        Self {
            state: Default::default(),
        }
    }

    pub fn create_server(&mut self) -> Server {
        Server::new(self.state.clone())
    }

    pub fn create_client<F: FnOnce(&mut ClientBuilder)>(&mut self, f: F) {
        let id = self.state.clients.push(super::Client {
            name: String::new(),
            scenario: vec![],
            connections: vec![],
            configuration: Default::default(),
        }) as u64;

        let mut builder = ClientBuilder::new(id, self.state.clone());
        f(&mut builder);

        self.state.clients.0.borrow_mut()[id as usize].scenario = builder.ops;
    }

    pub(super) fn finish(self) -> super::Scenario {
        let clients = self.state.clients.take();
        let servers = self.state.servers.take();
        let mut traces = self.state.trace.take().into_iter().collect::<Vec<_>>();
        traces.sort_by(|(_, a), (_, b)| a.cmp(b));
        let traces = traces.into_iter().map(|(value, _)| value).collect();

        let mut scenario = super::Scenario {
            id: Default::default(),
            clients,
            servers,
            routers: vec![],
            traces,
        };

        let mut hash = ScenarioId::hasher();
        core::hash::Hash::hash(&scenario, &mut hash);
        scenario.id = hash.finish();

        scenario
    }
}

#[derive(Debug)]
pub struct Server {
    id: u64,
    state: ScenarioState,
    connections: RefMap<u64, ConnectionInfo>,
}

impl Server {
    fn new(state: ScenarioState) -> Self {
        let id = state.servers.push(super::Server::default()) as u64;

        Self {
            id,
            state,
            connections: Default::default(),
        }
    }

    pub fn with<F: FnOnce(&mut ConnectionBuilder<Server>)>(&self, f: F) -> Connection<Server> {
        let mut builder = ConnectionBuilder::new(self.state.connection());
        f(&mut builder);

        Connection {
            endpoint_id: self.id,
            state: self.state.clone(),
            template: builder.finish(),
            endpoint: PhantomData,
        }
    }
}

#[derive(Debug)]
pub struct ConnectionInfo {
    peer_streams: Vec<Vec<ConnectionOp>>,
    ops: Vec<ConnectionOp>,
}

#[derive(Debug)]
pub struct ClientBuilder {
    id: u64,
    state: ScenarioState,
    ops: Vec<ClientOp>,
}

impl ClientBuilder {
    fn new(id: u64, state: ScenarioState) -> Self {
        Self {
            id,
            state,
            ops: vec![],
        }
    }

    pub fn connect_to<F: FnOnce(&mut ConnectionBuilder<Client>), To: Connect<Client>>(
        &mut self,
        to: To,
        f: F,
    ) -> Connection<Client> {
        let mut builder = ConnectionBuilder::new(self.state.connection());
        f(&mut builder);

        let template = builder.finish();
        let connection = Connection {
            endpoint_id: self.id,
            state: self.state.clone(),
            template,
            endpoint: PhantomData,
        };

        let op = to.connect_to(&connection);
        self.ops.push(op);

        connection
    }

    pub fn checkpoint(
        &mut self,
    ) -> (
        Checkpoint<Client, Local, Park>,
        Checkpoint<Client, Local, Unpark>,
    ) {
        self.state.checkpoint()
    }
}

#[derive(Clone, Debug, Default)]
struct ScenarioState {
    checkpoint: IdPool,
    servers: RefVec<super::Server>,
    clients: RefVec<super::Client>,
    trace: RefMap<String, u64>,
}

impl ScenarioState {
    fn checkpoint<Endpoint, Location>(
        &self,
    ) -> (
        Checkpoint<Endpoint, Location, Park>,
        Checkpoint<Endpoint, Location, Unpark>,
    ) {
        let id = self.checkpoint.next_id();
        (Checkpoint::new(id), Checkpoint::new(id))
    }

    fn connection(&self) -> ConnectionState {
        ConnectionState::new(self.clone())
    }

    fn trace(&self, name: &str) -> u64 {
        if let Some(v) = self.trace.0.borrow().get(name) {
            return *v;
        }

        let mut map = self.trace.0.borrow_mut();

        let id = map.len() as u64;
        map.insert(name.to_string(), id);
        id
    }
}

#[derive(Clone, Debug, Default)]
struct ConnectionState {
    peer_streams: RefVec<Vec<ConnectionOp>>,
    stream: IdPool,
    scenario: ScenarioState,
}

impl ConnectionState {
    fn new(scenario: ScenarioState) -> Self {
        Self {
            scenario,
            ..Default::default()
        }
    }
}

impl core::ops::Deref for ConnectionState {
    type Target = ScenarioState;

    fn deref(&self) -> &Self::Target {
        &self.scenario
    }
}

#[derive(Clone, Default)]
struct IdPool(Rc<RefCell<u64>>);

impl fmt::Debug for IdPool {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "IdPool({})", *self.0.borrow())
    }
}

impl IdPool {
    fn next_id(&self) -> u64 {
        let mut next_id = self.0.borrow_mut();
        let id = *next_id;
        *next_id += 1;
        id
    }
}

#[derive(Clone, Debug)]
struct RefMap<Key, Value>(Rc<RefCell<HashMap<Key, Value>>>);

impl<Key, Value> Default for RefMap<Key, Value> {
    fn default() -> Self {
        Self(Default::default())
    }
}

impl<Key: core::hash::Hash + Eq, Value> RefMap<Key, Value> {
    #[allow(dead_code)]
    fn insert(&self, key: Key, value: Value) {
        self.0.borrow_mut().insert(key, value);
    }

    fn take(&self) -> HashMap<Key, Value> {
        core::mem::take(&mut self.0.borrow_mut())
    }
}

#[derive(Clone, Debug)]
struct RefVec<Value>(Rc<RefCell<Vec<Value>>>);

impl<Value> Default for RefVec<Value> {
    fn default() -> Self {
        Self(Default::default())
    }
}

impl<Value> RefVec<Value> {
    fn len(&self) -> usize {
        self.0.borrow().len()
    }

    fn push(&self, value: Value) -> usize {
        let mut v = self.0.borrow_mut();
        let len = v.len();
        v.push(value);
        len
    }

    fn take(&self) -> Vec<Value> {
        core::mem::take(&mut self.0.borrow_mut())
    }
}

#[derive(Debug)]
pub struct Client {}

#[derive(Debug)]
pub struct ConnectionBuilder<Endpoint> {
    ops: Vec<ConnectionOp>,
    state: ConnectionState,
    endpoint: PhantomData<Endpoint>,
}

impl<E: Endpoint> ConnectionBuilder<E> {
    fn new(state: ConnectionState) -> Self {
        Self {
            ops: vec![],
            state,
            endpoint: PhantomData,
        }
    }

    pub fn checkpoint<Location>(
        &self,
    ) -> (
        Checkpoint<E, Location, Park>,
        Checkpoint<E, Location, Unpark>,
    ) {
        self.state.checkpoint()
    }

    sync!(E, Local);
    sleep!();
    trace!();

    pub fn scope<F: FnOnce(&mut Scope<E>)>(&mut self, f: F) -> &mut Self {
        let mut scope = Scope::new(self.state.clone());
        f(&mut scope);

        let threads = scope.threads;

        if threads.is_empty() {
            // no-op
        } else if threads.len() == 1 {
            // only a single thread was spawned, which is the same as not spawning it
            self.ops.extend(threads.into_iter().flatten());
        } else {
            self.ops.push(ConnectionOp::Scope { threads });
        }

        self
    }

    pub fn concurrently<
        A: FnOnce(&mut ConnectionBuilder<E>),
        B: FnOnce(&mut ConnectionBuilder<E>),
    >(
        &mut self,
        a: A,
        b: B,
    ) -> &mut Self {
        self.scope(|scope| {
            scope.spawn(a);
            scope.spawn(b);
        })
    }

    pub fn open_bidirectional_stream<
        L: FnOnce(&mut Stream<E, Local>),
        R: FnOnce(&mut Stream<E::Peer, Remote>),
    >(
        &mut self,
        local: L,
        remote: R,
    ) -> &mut Self {
        let id = self.state.stream.next_id();
        let mut local_stream = Stream::new(id, self.state.clone());
        let mut remote_stream = Stream::new(id, self.state.clone());

        local(&mut local_stream);
        remote(&mut remote_stream);

        self.ops
            .push(ConnectionOp::BidirectionalStream { stream_id: id });
        self.ops.extend(local_stream.ops);

        debug_assert_eq!(id, self.state.peer_streams.len() as u64);
        self.state.peer_streams.push(remote_stream.ops);

        self
    }

    pub fn open_send_stream<
        L: FnOnce(&mut SendStream<E, Local>),
        R: FnOnce(&mut ReceiveStream<E::Peer, Remote>),
    >(
        &mut self,
        local: L,
        remote: R,
    ) -> &mut Self {
        let id = self.state.stream.next_id();
        let mut local_stream = SendStream::new(id, self.state.clone());
        let mut remote_stream = ReceiveStream::new(id, self.state.clone());

        local(&mut local_stream);
        remote(&mut remote_stream);

        self.ops.push(ConnectionOp::SendStream { stream_id: id });
        self.ops.extend(local_stream.ops);

        debug_assert_eq!(id, self.state.peer_streams.len() as u64);
        self.state.peer_streams.push(remote_stream.ops);

        self
    }

    fn finish(self) -> ConnectionInfo {
        let peer_streams = self.state.peer_streams.take();
        let ops = self.ops;
        ConnectionInfo { peer_streams, ops }
    }
}

#[derive(Debug)]
pub struct Checkpoint<Endpoint, Location, Op> {
    id: u64,
    endpoint: PhantomData<Endpoint>,
    location: PhantomData<Location>,
    op: PhantomData<Op>,
}

impl<Endpoint, Location, Op> Checkpoint<Endpoint, Location, Op> {
    fn new(id: u64) -> Self {
        Self {
            id,
            endpoint: PhantomData,
            location: PhantomData,
            op: PhantomData,
        }
    }
}

#[derive(Debug)]
pub struct Local;

#[derive(Debug)]
pub struct Remote;

#[derive(Debug)]
pub struct Connection<Endpoint> {
    state: ScenarioState,
    endpoint_id: u64,
    template: ConnectionInfo,
    endpoint: PhantomData<Endpoint>,
}

pub trait Connect<Endpoint> {
    fn connect_to(&self, handle: &Connection<Endpoint>) -> ClientOp;
}

impl Connect<Client> for Connection<Server> {
    fn connect_to(&self, handle: &Connection<Client>) -> ClientOp {
        let server_id = self.endpoint_id;
        let server = &mut self.state.servers.0.borrow_mut()[server_id as usize];
        let server_connection_id = server.connections.len() as u64;
        server.connections.push(super::Connection {
            ops: self.template.ops.clone(),
            peer_streams: handle.template.peer_streams.clone(),
        });

        let client = &mut self.state.clients.0.borrow_mut()[handle.endpoint_id as usize];
        let client_connection_id = client.connections.len() as u64;
        client.connections.push(super::Connection {
            ops: handle.template.ops.clone(),
            peer_streams: self.template.peer_streams.clone(),
        });

        ClientOp::Connect {
            server_id,
            router_id: None,
            server_connection_id,
            client_connection_id,
        }
    }
}

impl Connect<Client> for Server {
    fn connect_to(&self, handle: &Connection<Client>) -> ClientOp {
        self.with(|_| {
            // empty instructions
        })
        .connect_to(handle)
    }
}

pub struct Scope<Endpoint> {
    state: ConnectionState,
    threads: Vec<Vec<ConnectionOp>>,
    endpoint: PhantomData<Endpoint>,
}

impl<E: Endpoint> Scope<E> {
    fn new(state: ConnectionState) -> Self {
        Self {
            state,
            threads: vec![],
            endpoint: PhantomData,
        }
    }

    pub fn spawn<F: FnOnce(&mut ConnectionBuilder<E>)>(&mut self, f: F) -> &mut Self {
        let mut builder = ConnectionBuilder::new(self.state.clone());
        f(&mut builder);
        self.threads.push(builder.ops);
        self
    }
}

pub trait Endpoint {
    type Peer: Endpoint;
}

impl Endpoint for Client {
    type Peer = Server;
}

impl Endpoint for Server {
    type Peer = Client;
}

macro_rules! send_stream {
    () => {
        pub fn send(&mut self, bytes: u64) -> &mut Self {
            self.ops.push(ConnectionOp::Send {
                stream_id: self.id,
                bytes,
            });
            self
        }

        pub fn set_send_rate(&mut self, bytes: u64, period: Duration) -> &mut Self {
            self.ops.push(ConnectionOp::SendRate {
                stream_id: self.id,
                bytes,
                period,
            });
            self
        }
    };
}

macro_rules! receive_stream {
    () => {
        pub fn receive(&mut self, bytes: u64) -> &mut Self {
            self.ops.push(ConnectionOp::Receive {
                stream_id: self.id,
                bytes,
            });
            self
        }

        pub fn set_receive_rate(&mut self, bytes: u64, period: Duration) -> &mut Self {
            self.ops.push(ConnectionOp::ReceiveRate {
                stream_id: self.id,
                bytes,
                period,
            });
            self
        }

        pub fn receive_all(&mut self) -> &mut Self {
            self.ops
                .push(ConnectionOp::ReceiveAll { stream_id: self.id });
            self
        }
    };
}

pub struct Stream<Endpoint, Location> {
    id: u64,
    ops: Vec<ConnectionOp>,
    state: ConnectionState,
    endpoint: PhantomData<Endpoint>,
    location: PhantomData<Location>,
}

impl<Endpoint, Location> Stream<Endpoint, Location> {
    send_stream!();
    receive_stream!();
    sync!(Endpoint, Location);
    sleep!();
    trace!();

    fn new(id: u64, state: ConnectionState) -> Self {
        Self {
            id,
            ops: vec![],
            state,
            endpoint: PhantomData,
            location: PhantomData,
        }
    }

    pub fn concurrently<
        S: FnOnce(&mut SendStream<Endpoint, Location>),
        R: FnOnce(&mut ReceiveStream<Endpoint, Location>),
    >(
        &mut self,
        send: S,
        receive: R,
    ) -> &mut Self {
        let mut send_stream = SendStream::new(self.id, self.state.clone());
        let mut receive_stream = ReceiveStream::new(self.id, self.state.clone());
        send(&mut send_stream);
        receive(&mut receive_stream);
        let threads = vec![send_stream.ops, receive_stream.ops];
        self.ops.push(ConnectionOp::Scope { threads });
        self
    }
}

pub struct SendStream<Endpoint, Location> {
    id: u64,
    ops: Vec<ConnectionOp>,
    state: ConnectionState,
    endpoint: PhantomData<Endpoint>,
    location: PhantomData<Location>,
}

impl<Endpoint, Location> SendStream<Endpoint, Location> {
    send_stream!();
    sync!(Endpoint, Location);
    sleep!();
    trace!();

    fn new(id: u64, state: ConnectionState) -> Self {
        Self {
            id,
            ops: vec![],
            state,
            endpoint: PhantomData,
            location: PhantomData,
        }
    }
}

pub struct ReceiveStream<Endpoint, Location> {
    id: u64,
    ops: Vec<ConnectionOp>,
    state: ConnectionState,
    endpoint: PhantomData<Endpoint>,
    location: PhantomData<Location>,
}

impl<Endpoint, Location> ReceiveStream<Endpoint, Location> {
    receive_stream!();
    sync!(Endpoint, Location);
    sleep!();
    trace!();

    fn new(id: u64, state: ConnectionState) -> Self {
        Self {
            id,
            ops: vec![],
            state,
            endpoint: PhantomData,
            location: PhantomData,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::Scenario;
    use crate::ext::*;
    use insta::assert_json_snapshot;

    #[test]
    fn simple() {
        let scenario = Scenario::build(|scenario| {
            let server = scenario.create_server();

            scenario.create_client(|client| {
                client.connect_to(server, |conn| {
                    conn.open_send_stream(
                        |local| {
                            local.set_send_rate(1024.bytes(), 50.millis());
                            local.send(1.megabytes());
                        },
                        |peer| {
                            peer.set_receive_rate(1024.bytes(), 50.millis());
                            peer.receive(1.megabytes());
                        },
                    );
                });
            });
        });

        assert_json_snapshot!(scenario);
    }

    #[test]
    fn conn_checkpoints() {
        let scenario = Scenario::build(|scenario| {
            let server = scenario.create_server();

            scenario.create_client(|client| {
                client.connect_to(server, |conn| {
                    let (cp1_rx, cp1_tx) = conn.checkpoint();

                    conn.concurrently(
                        |conn| {
                            conn.open_send_stream(
                                |local| {
                                    local.set_send_rate(10.kilobytes(), 50.millis());
                                    local.send(1.megabytes() / 2);
                                    local.unpark(cp1_tx);
                                    local.send(1.megabytes() / 2);
                                },
                                |peer| {
                                    peer.set_receive_rate(10.kilobytes(), 50.millis());
                                    peer.receive(1.megabytes());
                                },
                            );
                        },
                        |conn| {
                            conn.open_send_stream(
                                |local| {
                                    local.park(cp1_rx);
                                    local.set_send_rate(1024.bytes(), 50.millis());
                                    local.send(1.megabytes());
                                },
                                |peer| {
                                    peer.set_receive_rate(1024.bytes(), 50.millis());
                                    peer.receive(1.megabytes());
                                },
                            );
                        },
                    );
                });
            });
        });

        assert_json_snapshot!(scenario);
    }

    #[test]
    fn linked_streams() {
        let scenario = Scenario::build(|scenario| {
            let server = scenario.create_server();

            scenario.create_client(|client| {
                client.connect_to(server, |conn| {
                    let (b_park, b_unpark) = conn.checkpoint();

                    conn.concurrently(
                        |conn| {
                            conn.open_bidirectional_stream(
                                |local| {
                                    local.concurrently(
                                        |sender| {
                                            sender.set_send_rate(1024.bytes(), 50.millis());
                                            sender.send(1.megabytes());
                                        },
                                        |receiver| {
                                            receiver.receive_all();
                                        },
                                    );

                                    local.unpark(b_unpark);
                                },
                                |peer| {
                                    peer.sleep(100.millis());

                                    peer.set_receive_rate(1024.bytes(), 50.millis());
                                    peer.receive(100.kilobytes());

                                    peer.set_receive_rate(10.bytes(), 50.millis());
                                    peer.receive_all();

                                    peer.send(2.megabytes());
                                },
                            );
                        },
                        |conn| {
                            conn.open_send_stream(
                                |local| {
                                    local.park(b_park);
                                    local.send(1.megabytes());
                                },
                                |peer| {
                                    peer.receive(1.megabytes());
                                },
                            );
                        },
                    );
                });
            });
        });

        assert_json_snapshot!(scenario);
    }
}
