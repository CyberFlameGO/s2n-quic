use netbench::Result;
use structopt::StructOpt;

#[cfg(feature = "native-tls")]
mod native_tls;
#[cfg(feature = "s2n-quic")]
mod s2n_quic;
#[cfg(feature = "tcp")]
mod tcp;

#[derive(Debug, StructOpt)]
enum Arguments {
    #[cfg(feature = "native-tls")]
    NativeTls(native_tls::NativeTls),
    #[cfg(feature = "s2n-quic")]
    S2nQuic(s2n_quic::S2nQuic),
    #[cfg(feature = "tcp")]
    Tcp(tcp::Tcp),
}

impl Arguments {
    pub async fn run(&self) -> Result<()> {
        match self {
            #[cfg(feature = "native-tls")]
            Self::NativeTls(subject) => subject.run().await,
            #[cfg(feature = "s2n-quic")]
            Self::S2nQuic(subject) => subject.run().await,
            #[cfg(feature = "tcp")]
            Self::Tcp(subject) => subject.run().await,
        }
    }
}

//#[tokio::main(flavor = "current_thread")]
#[tokio::main]
async fn main() {
    let format = tracing_subscriber::fmt::format()
        .with_level(false) // don't include levels in formatted output
        .with_timer(tracing_subscriber::fmt::time::uptime())
        .with_ansi(false)
        .compact(); // Use a less verbose output format.

    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .event_format(format)
        .init();

    match Arguments::from_args_safe() {
        Ok(args) => {
            if let Err(error) = args.run().await {
                eprintln!("Error: {:?}", error);
            }
        }
        Err(error) => {
            if error.use_stderr() {
                eprintln!("{}", error);

                // https://github.com/marten-seemann/quic-interop-runner/blob/cd223804bf3f102c3567758ea100577febe486ff/interop.py#L102
                // The interop runner wants us to exit with code 127 when an invalid argument is passed
                std::process::exit(127);
            } else {
                println!("{}", error);
            }
        }
    };
}
