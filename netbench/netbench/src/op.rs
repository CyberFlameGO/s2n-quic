use core::time::Duration;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, Hash)]
#[serde(rename_all = "snake_case")]
pub enum ConnectionOp {
    /// Pause for the specified duration before processing the next op
    Sleep {
        #[serde(with = "duration_format", rename = "amount_ms")]
        amount: Duration,
    },
    /// Open a bidirectional stream with an identifier
    BidirectionalStream { stream_id: u64 },
    /// Open a unidirectional stream with an identifier
    SendStream { stream_id: u64 },
    /// Send a specific amount of data over the stream id
    Send { stream_id: u64, bytes: u64 },
    /// Finish sending data on the stream
    SendFinish { stream_id: u64 },
    /// Sets the send rate for a stream
    SendRate {
        stream_id: u64,
        bytes: u64,
        #[serde(with = "duration_format", rename = "period_ms")]
        period: Duration,
    },
    /// Send a specific amount of data over the stream id
    Receive { stream_id: u64, bytes: u64 },
    /// Receives all of the data on the stream until it is finished
    ReceiveAll { stream_id: u64 },
    /// Finish receiving data on the stream
    ReceiveFinish { stream_id: u64 },
    /// Sets the receive rate for a stream
    ReceiveRate {
        stream_id: u64,
        bytes: u64,
        #[serde(with = "duration_format", rename = "period_ms")]
        period: Duration,
    },
    /// Parks the current thread and waits for the checkpoint to be unparked
    Park { checkpoint: u64 },
    /// Notifies the parked checkpoint that it can continue
    Unpark { checkpoint: u64 },
    /// Emit a trace event
    Trace { trace_id: u64 },
    /// Perform operations concurrently
    Scope { threads: Vec<Vec<ConnectionOp>> },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, Hash)]
#[serde(rename_all = "snake_case")]
pub enum ClientOp {
    /// Pause for the specified duration before processing the next op
    Sleep {
        #[serde(with = "duration_format", rename = "timeout_ms")]
        timeout: Duration,
    },
    /// Open a connection with an identifier
    Connect {
        server_id: u64,
        #[serde(skip_serializing_if = "Option::is_none", default)]
        router_id: Option<u64>,
        server_connection_id: u64,
        client_connection_id: u64,
    },
    /// Synchronizes two checkpoints across different threads
    Sync { checkpoint: u64 },
    /// Emit a trace event
    Trace { trace_id: u64 },
    /// Perform operations concurrently
    Scope { threads: Vec<Vec<ClientOp>> },
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize, Hash)]
#[serde(rename_all = "snake_case")]
pub enum RouterOp {
    /// Pause for the specified duration before processing the next op
    Sleep {
        #[serde(with = "duration_format", rename = "amount_ms")]
        amount: Duration,
    },

    /// Set the number of packets that can be buffered server->client
    ServerBufferCount { packet_count: u32 },
    /// Set the chance of a server->client packet being dropped
    ServerDropRate { packet_count: u32 },
    /// Set the chance of a server->client packet being reordered
    ServerReorderRate { packet_count: u32 },
    /// Set the chance of a server->client packet being corrupted
    ServerCorruptRate { packet_count: u32 },
    /// Set the amount of delay for server->client packets
    ServerDelay {
        #[serde(with = "duration_format", rename = "amount_ms")]
        amount: Duration,
    },
    /// Set the amount of jitter for server->client packets
    ServerJitter {
        #[serde(with = "duration_format", rename = "amount_ms")]
        amount: Duration,
    },
    /// Set the server->client MTU
    ServerMtu { mtu: u16 },

    /// Set the number of packets that can be buffered server->client
    ClientBufferCount { packet_count: u32 },
    /// Set the chance of a client->server packet being dropped
    ClientDropRate { packet_count: u32 },
    /// Set the chance of a client->server packet being reordered
    ClientReorderRate { packet_count: u32 },
    /// Set the chance of a client->server packet being corrupted
    ClientCorruptRate { packet_count: u32 },
    /// Set the amount of delay for client->server packets
    ClientDelay {
        #[serde(with = "duration_format", rename = "amount_ms")]
        amount: Duration,
    },
    /// Set the amount of jitter for client->server packets
    ClientJitter {
        #[serde(with = "duration_format", rename = "amount_ms")]
        amount: Duration,
    },
    /// Set the client->server MTU
    ClientMtu { mtu: u16 },

    /// Set the chance of a port being rebound
    ClientRebindPortRate { packet_count: u32 },
    /// Set the chance of an IP being rebound
    ClientRebindAddressRate { packet_count: u32 },

    /// Rebinds all of the ports and/or addresses currently being used
    RebindAll { ports: bool, addresses: bool },

    /// Emit a trace event
    Trace { trace_id: u64 },
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, Hash)]
pub struct ScenarioId(String);

impl ScenarioId {
    pub fn hasher() -> ScenarioIdHasher {
        ScenarioIdHasher::default()
    }
}

#[derive(Debug, Default)]
pub struct ScenarioIdHasher {
    hash: sha2::Sha256,
}

impl ScenarioIdHasher {
    pub fn finish(self) -> ScenarioId {
        use sha2::Digest;
        let hash = self.hash.finalize();
        let mut out = String::new();
        for byte in hash.iter() {
            use core::fmt::Write;
            write!(out, "{:02x}", byte).unwrap();
        }
        ScenarioId(out)
    }
}

impl core::hash::Hasher for ScenarioIdHasher {
    fn write(&mut self, bytes: &[u8]) {
        use sha2::Digest;
        self.hash.update(bytes)
    }

    fn finish(&self) -> u64 {
        unimplemented!()
    }
}

mod duration_format {
    use core::time::Duration;
    use serde::{self, Deserialize, Deserializer, Serializer};

    pub fn serialize<S>(duration: &Duration, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_u64(duration.as_millis() as u64)
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<Duration, D::Error>
    where
        D: Deserializer<'de>,
    {
        let millis = u64::deserialize(deserializer)?;
        Ok(Duration::from_millis(millis))
    }
}
