use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, net::SocketAddr};

#[derive(Clone, Debug, Default, Deserialize, Serialize, Hash)]
pub struct Overlay {
    pub clients: Vec<SocketAddr>,
    pub servers: Vec<SocketAddr>,
    pub routers: Vec<BTreeMap<usize, SocketAddr>>,
}
