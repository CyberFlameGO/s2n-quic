use netbench::{
    scenario::{self, Scenario},
    Result,
};
use std::{collections::HashSet, net::SocketAddr, path::PathBuf, sync::Arc};
use structopt::StructOpt;
use tokio::{
    net::{TcpListener, TcpStream},
    spawn,
};

#[derive(Debug, StructOpt)]
pub enum Tcp {
    Client(Client),
    Server(Server),
}

impl Tcp {
    pub async fn run(&self) -> Result<()> {
        match self {
            Self::Client(client) => client.run().await,
            Self::Server(server) => server.run().await,
        }
    }
}

#[derive(Debug, StructOpt)]
pub struct Server {
    #[structopt(short, long, default_value = "::")]
    ip: std::net::IpAddr,

    #[structopt(short, long, default_value = "4433")]
    port: u16,

    #[structopt(long, default_value = "0")]
    server_id: usize,

    scenario: PathBuf,
}

impl Server {
    pub async fn run(&self) -> Result<()> {
        let mut scenario = Scenario::open(&self.scenario)?;
        let scenario = scenario.servers.remove(self.server_id);
        let scenario: Arc<[_]> = scenario.connections.clone().into();

        let server = self.server().await?;

        let mut conn_id = 0;
        loop {
            let (connection, _addr) = server.accept().await?;
            // spawn a task per connection
            let scenario = scenario.clone();
            let id = conn_id;
            conn_id += 1;
            spawn(async move {
                let _ = dbg!(handle_connection(connection, id, scenario).await);
            });
        }

        async fn handle_connection(
            connection: TcpStream,
            conn_id: u64,
            scenario: Arc<[scenario::Connection]>,
        ) -> Result<()> {
            // TODO parse the first few bytes for the server id
            let id = 0;
            let scenario = scenario.get(id).ok_or("invalid connection id")?;

            let config = Default::default();
            let connection = Box::pin(connection);

            let conn = netbench::connection::Driver::new(
                scenario,
                netbench::connection::multiplexed::Connection::new(connection, config),
            );

            // let mut trace = netbench::connection::trace::Disabled::default();
            // let mut trace = netbench::connection::trace::Logger::new(conn_id, &[][..]);

            let mut trace = netbench::connection::trace::Throughput::default();
            let reporter = trace.reporter(core::time::Duration::from_secs(1));

            let mut checkpoints = HashSet::new();

            conn.run(&mut trace, &mut checkpoints).await?;

            drop(trace);

            Ok(())
        }
    }

    async fn server(&self) -> Result<TcpListener> {
        let server = TcpListener::bind((self.ip, self.port)).await?;

        eprintln!("Server listening on port {}", self.port);

        Ok(server)
    }
}

#[derive(Debug, StructOpt)]
pub struct Client {
    #[structopt(long, default_value = "0")]
    client_id: usize,

    scenario: PathBuf,
}

impl Client {
    pub async fn run(&self) -> Result<()> {
        let mut scenario = Scenario::open(&self.scenario)?;
        let mut scenario = scenario.clients.remove(self.client_id);
        let scenario: Arc<[_]> = scenario
            .connections
            .drain(..)
            .map(|scenario| Arc::new(scenario))
            .collect::<Vec<_>>()
            .into();

        // TODO execute client ops instead
        let mut conn_id = 0;
        for scenario in scenario.iter() {
            // TODO read server address from instance file
            let addr: SocketAddr = "192.168.86.76:4433".parse()?;
            let connection = TcpStream::connect(addr).await?;
            let id = conn_id;
            conn_id += 1;
            handle_connection(connection, id, scenario.clone()).await?;
        }

        return Ok(());

        async fn handle_connection(
            connection: TcpStream,
            conn_id: u64,
            scenario: Arc<scenario::Connection>,
        ) -> Result<()> {
            // TODO write the server's connection id

            let config = Default::default();
            let connection = Box::pin(connection);

            let conn = netbench::connection::Driver::new(
                &scenario,
                netbench::connection::multiplexed::Connection::new(connection, config),
            );

            // let mut trace = netbench::connection::trace::Disabled::default();
            // let mut trace = netbench::connection::trace::Logger::new(conn_id, &[][..]);
            let mut trace = netbench::connection::trace::Throughput::default();
            let reporter = trace.reporter(core::time::Duration::from_secs(1));
            let mut checkpoints = HashSet::new();

            conn.run(&mut trace, &mut checkpoints).await?;

            drop(reporter);

            Ok(())
        }
    }
}
