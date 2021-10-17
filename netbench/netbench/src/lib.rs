pub type Result<T, E = Error> = core::result::Result<T, E>;
pub type Error = Box<dyn std::error::Error>;

mod try_buf;

pub mod connection;
pub mod ext;
pub mod op;
pub mod overlay;
pub mod scenario;
