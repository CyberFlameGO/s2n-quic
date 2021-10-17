use netbench::{
    scenario::{self, Scenario},
    Result,
};
use s2n_quic::{
    provider::{
        event, io,
        tls::default::certificate::{Certificate, IntoCertificate, IntoPrivateKey, PrivateKey},
    },
    Connection,
};
use std::{collections::HashSet, net::SocketAddr, path::PathBuf, sync::Arc};
use structopt::StructOpt;
use tokio::spawn;

#[derive(Debug, StructOpt)]
pub enum S2nQuic {
    Client(Client),
    Server(Server),
}

impl S2nQuic {
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

    #[structopt(long)]
    certificate: Option<PathBuf>,

    #[structopt(long)]
    private_key: Option<PathBuf>,

    #[structopt(long, default_value = "netbench")]
    alpn_protocols: Vec<String>,

    #[structopt(long)]
    disable_gso: bool,

    #[structopt(long, default_value = "0")]
    server_id: usize,

    scenario: PathBuf,
}

impl Server {
    pub async fn run(&self) -> Result<()> {
        let mut scenario = Scenario::open(&self.scenario)?;
        let scenario = scenario.servers.remove(self.server_id);
        let scenario: Arc<[_]> = scenario.connections.clone().into();

        let mut server = self.server()?;

        while let Some(connection) = server.accept().await {
            // spawn a task per connection
            let scenario = scenario.clone();
            spawn(async move {
                let _ = dbg!(handle_connection(connection, scenario).await);
            });
        }

        return Err("".into());

        async fn handle_connection(
            connection: Connection,
            scenario: Arc<[scenario::Connection]>,
        ) -> Result<()> {
            // let host = connection.sni()?.ok_or("missing hostname")?;
            // let id = host.split(".").next().ok_or("invalid hostname")?;
            // let id: usize = id.parse()?;
            let id = 0;
            let scenario = scenario.get(id).ok_or("invalid connection id")?;

            let conn_id = connection.id();

            let conn = netbench::connection::Driver::new(
                scenario,
                netbench::connection::s2n_quic::Connection::new(connection),
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

    fn server(&self) -> Result<s2n_quic::Server> {
        let private_key = self.private_key()?;
        let certificate = self.certificate()?;

        let tls = s2n_quic::provider::tls::default::Server::builder()
            .with_certificate(certificate, private_key)?
            .with_alpn_protocols(self.alpn_protocols.iter().map(String::as_bytes))?
            .with_key_logging()?
            .build()?;

        let mut io_builder =
            io::Default::builder().with_receive_address((self.ip, self.port).into())?;

        if self.disable_gso {
            io_builder = io_builder.with_gso_disabled()?;
        }

        let io = io_builder.build()?;

        let server = s2n_quic::Server::builder()
            .with_io(io)?
            .with_tls(tls)?
            .with_event(event::tracing::Provider::default())?
            .start()
            .unwrap();

        eprintln!("Server listening on port {}", self.port);

        Ok(server)
    }

    fn certificate(&self) -> Result<Certificate> {
        Ok(if let Some(pathbuf) = self.certificate.as_ref() {
            pathbuf.into_certificate()?
        } else {
            s2n_quic_core::crypto::tls::testing::certificates::CERT_PEM.into_certificate()?
        })
    }

    fn private_key(&self) -> Result<PrivateKey> {
        Ok(if let Some(pathbuf) = self.private_key.as_ref() {
            pathbuf.into_private_key()?
        } else {
            s2n_quic_core::crypto::tls::testing::certificates::KEY_PEM.into_private_key()?
        })
    }
}

#[derive(Debug, StructOpt)]
pub struct Client {
    #[structopt(long)]
    ca: Option<PathBuf>,

    #[structopt(long, default_value = "netbench")]
    alpn_protocols: Vec<String>,

    #[structopt(long)]
    disable_gso: bool,

    #[structopt(short, long, default_value = "::")]
    local_ip: std::net::IpAddr,

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

        let mut client = self.client()?;

        // TODO execute client ops instead
        for scenario in scenario.iter() {
            // TODO read server address from instance file
            let addr: SocketAddr = "[::1]:4433".parse()?;
            // TODO format the server's connection id as part of the hostname
            let hostname = format!("localhost");
            let connect = s2n_quic::client::Connect::new(addr).with_hostname(hostname);
            let connection = client.connect(connect).await?;
            handle_connection(connection, scenario.clone()).await?;
        }

        async fn handle_connection(
            connection: Connection,
            scenario: Arc<scenario::Connection>,
        ) -> Result<()> {
            let conn_id = connection.id();
            let conn = netbench::connection::Driver::new(
                &scenario,
                netbench::connection::s2n_quic::Connection::new(connection),
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

        client.wait_idle().await?;

        return Ok(());
    }

    fn client(&self) -> Result<s2n_quic::Client> {
        let ca = self.ca()?;

        let tls = s2n_quic::provider::tls::default::Client::builder()
            .with_certificate(ca)?
            // the "amplificationlimit" tests generates a very large chain so bump the limit
            .with_max_cert_chain_depth(10)?
            .with_alpn_protocols(self.alpn_protocols.iter().map(String::as_bytes))?
            .with_key_logging()?
            .build()?;

        let mut io_builder =
            io::Default::builder().with_receive_address((self.local_ip, 0u16).into())?;

        if self.disable_gso {
            io_builder = io_builder.with_gso_disabled()?;
        }

        let io = io_builder.build()?;

        let client = s2n_quic::Client::builder()
            .with_io(io)?
            .with_tls(tls)?
            .with_event(event::tracing::Provider::default())?
            .start()
            .unwrap();

        Ok(client)
    }

    fn ca(&self) -> Result<Certificate> {
        Ok(if let Some(pathbuf) = self.ca.as_ref() {
            pathbuf.into_certificate()?
        } else {
            s2n_quic_core::crypto::tls::testing::certificates::CERT_PEM.into_certificate()?
        })
    }
}
