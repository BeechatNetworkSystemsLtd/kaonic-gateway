//! Snapshot types exported to hosts/UI. Plain serde data — no Reticulum types.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct LocalNodeDto {
    pub identity_hash: String,
    pub destination_hash: String,
    pub codename: String,
    pub gateway_version: String,
    pub announce_secs: u32,
    pub accepts_pairing: bool,
}

/// Where a node stands in the pairing lifecycle, from this node's point of view.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PairingState {
    #[default]
    None,
    /// We sent a request and are waiting for the remote operator.
    Requested,
    /// The remote asked us; awaiting local approval.
    Incoming,
    Paired,
    Rejected,
    Failed,
}

impl PairingState {
    pub fn as_str(&self) -> &'static str {
        match self {
            PairingState::None => "none",
            PairingState::Requested => "requested",
            PairingState::Incoming => "incoming",
            PairingState::Paired => "paired",
            PairingState::Rejected => "rejected",
            PairingState::Failed => "failed",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "none" => PairingState::None,
            "requested" => PairingState::Requested,
            "incoming" => PairingState::Incoming,
            "paired" => PairingState::Paired,
            "rejected" => PairingState::Rejected,
            "failed" => PairingState::Failed,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum LinkState {
    #[default]
    None,
    Pending,
    Active,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct NodeDto {
    pub identity_hash: String,
    pub destination_hash: String,
    pub codename: String,
    pub gateway_version: String,
    pub protocol: u8,
    pub hops: Option<u8>,
    pub rssi: Option<i8>,
    pub last_seen_ts: u64,
    pub online: bool,
    pub paired: bool,
    pub pairing: PairingState,
    pub pairing_detail: String,
    pub link: LinkState,
    pub sas: String,
    pub permissions: u32,
    pub accepts_pairing: bool,
    /// Peer decodes per-frame FEC selection (faster codes may be used).
    pub fec_capable: bool,
    /// Operator's own label for this node, kept in the local database.
    #[serde(default)]
    pub tag: String,
    /// VPN tunnel IP of the same identity, when the VPN knows it.
    #[serde(default)]
    pub vpn_tunnel_ip: String,
    /// Subnets this node exports over the VPN (the 192.168.x.0/24 aliases).
    #[serde(default)]
    pub vpn_routes: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct PairingRequestDto {
    pub identity_hash: String,
    pub codename: String,
    pub received_ts: u64,
    pub sas: String,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum JobState {
    #[default]
    Starting,
    Transferring,
    Applying,
    Done,
    Failed,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct BlobJobDto {
    pub id: u32,
    pub node: String,
    pub codename: String,
    pub name: String,
    pub purpose: u8,
    pub size: u32,
    pub sent: u32,
    pub state: JobState,
    pub detail: String,
    pub started_ts: u64,
    pub updated_ts: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct MediaStreamDto {
    pub node: String,
    pub codename: String,
    pub stream: u8,
    /// "out" (we send) or "in" (we receive).
    pub direction: String,
    pub k: u8,
    pub m: u8,
    pub packets_sent: u64,
    pub parity_sent: u64,
    pub packets_received: u64,
    pub packets_recovered: u64,
    pub packets_lost: u64,
    pub shards_late: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct RemoteSnapshot {
    pub local: LocalNodeDto,
    pub nodes: Vec<NodeDto>,
    pub incoming_requests: Vec<PairingRequestDto>,
    pub jobs: Vec<BlobJobDto>,
    pub media: Vec<MediaStreamDto>,
    /// Recent human-readable activity, newest first.
    pub events: Vec<RemoteEventDto>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct RemoteEventDto {
    pub ts: u64,
    pub kind: String,
    pub node: String,
    pub codename: String,
    pub details: String,
}
