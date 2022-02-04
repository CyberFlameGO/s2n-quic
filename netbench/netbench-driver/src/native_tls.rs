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
use tokio_native_tls::native_tls::{Certificate, Identity, TlsAcceptor, TlsConnector};

#[derive(Debug, StructOpt)]
pub enum NativeTls {
    Client(Client),
    Server(Server),
}

impl NativeTls {
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

        let ident = self.identity()?;
        let acceptor = TlsAcceptor::builder(ident).build()?;
        let acceptor: tokio_native_tls::TlsAcceptor = acceptor.into();
        let acceptor = Arc::new(acceptor);

        let mut conn_id = 0;
        loop {
            let (connection, _addr) = server.accept().await?;
            // spawn a task per connection
            let scenario = scenario.clone();
            let id = conn_id;
            conn_id += 1;
            let acceptor = acceptor.clone();
            spawn(async move {
                if let Err(err) = handle_connection(acceptor, connection, id, scenario).await {
                    eprintln!("{}", err);
                }
            });
        }

        async fn handle_connection(
            acceptor: Arc<tokio_native_tls::TlsAcceptor>,
            connection: TcpStream,
            conn_id: u64,
            scenario: Arc<[scenario::Connection]>,
        ) -> Result<()> {
            let connection = acceptor.accept(connection).await?;

            // TODO parse the hostname
            let id = 0;
            let scenario = scenario.get(id).ok_or("invalid connection id")?;

            let config = Default::default();
            let connection = Box::pin(connection);

            let conn = netbench::connection::Driver::new(
                scenario,
                netbench::connection::multiplexed::Connection::new(connection, config),
            );

            let mut trace = netbench::connection::trace::Disabled::default();
            let mut trace = netbench::connection::trace::Logger::new(conn_id, &[][..]);

            let mut trace = netbench::connection::trace::Throughput::default();
            let reporter = trace.reporter(core::time::Duration::from_secs(1));

            let mut checkpoints = HashSet::new();

            conn.run(&mut trace, &mut checkpoints).await?;

            drop(reporter);

            Ok(())
        }
    }

    async fn server(&self) -> Result<TcpListener> {
        let server = TcpListener::bind((self.ip, self.port)).await?;

        eprintln!("Server listening on port {}", self.port);

        Ok(server)
    }

    fn identity(&self) -> Result<Identity> {
        let ca = if let Some(path) = self.certificate.as_ref() {
            let pem = std::fs::read_to_string(path)?;
            openssl::x509::X509::from_pem(pem.as_bytes())?
        } else {
            openssl::x509::X509::from_pem(
                s2n_quic_core::crypto::tls::testing::certificates::CERT_PEM.as_bytes(),
            )?
        };

        let key = if let Some(path) = self.private_key.as_ref() {
            let pem = std::fs::read_to_string(path)?;
            openssl::pkey::PKey::private_key_from_pem(pem.as_bytes())?
        } else {
            openssl::pkey::PKey::private_key_from_pem(
                s2n_quic_core::crypto::tls::testing::certificates::KEY_PEM.as_bytes(),
            )?
        };

        let cert = openssl::pkcs12::Pkcs12::builder().build("", "", &key, &ca)?;
        let cert = cert.to_der()?;

        let ident = Identity::from_pkcs12(&cert, "")?;

        Ok(ident)
    }
}

#[derive(Debug, StructOpt)]
pub struct Client {
    #[structopt(long)]
    ca: Option<PathBuf>,

    #[structopt(long, default_value = "netbench")]
    alpn_protocols: Vec<String>,

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

        let connector = TlsConnector::builder()
            .add_root_certificate(self.ca()?)
            .build()?;
        let connector: tokio_native_tls::TlsConnector = connector.into();
        let connector = Arc::new(connector);

        // TODO execute client ops instead
        let mut conn_id = 0;
        for scenario in scenario.iter() {
            // TODO read server address from instance file
            let addr: SocketAddr = "192.168.86.76:4433".parse()?;
            let connection = TcpStream::connect(addr).await?;
            let id = conn_id;
            conn_id += 1;
            handle_connection(connector.clone(), connection, id, scenario.clone()).await?;
        }

        async fn handle_connection(
            connector: Arc<tokio_native_tls::TlsConnector>,
            connection: TcpStream,
            conn_id: u64,
            scenario: Arc<scenario::Connection>,
        ) -> Result<()> {
            // TODO write the server's connection id
            let connection = connector.connect("localhost", connection).await?;

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

        return Ok(());
    }

    fn ca(&self) -> Result<Certificate> {
        Ok(if let Some(path) = self.ca.as_ref() {
            let key = std::fs::read(path)?;
            Certificate::from_pem(&key)?
        } else {
            Certificate::from_pem(
                s2n_quic_core::crypto::tls::testing::certificates::CERT_PEM.as_bytes(),
            )?
        })
    }
}
