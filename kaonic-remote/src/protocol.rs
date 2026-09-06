//! Wire protocol for Kaonic Remote.
//!
//! Everything here is designed around the radio path: a Reticulum link data
//! packet that fits in a single LDPC-coded radio frame carries at most
//! [`MAX_LINK_PAYLOAD`] bytes of plaintext, so control messages use a 4-byte
//! envelope plus positional (array-encoded) MessagePack bodies, blob chunks
//! use a 6-byte raw header, and announces are a fixed 16-byte record.
//!
//! Layout
//!
//! ```text
//! Request  : [0x01][req_id u16 BE][op u8][body...]
//! Response : [0x02][req_id u16 BE][status u8][body...]
//! Chunk    : [0x03][transfer u8][index u32 BE][data...]
//! Parity   : [0x04][transfer u8][block u32 BE][shard u8][data...]
//! Media    : [0x05][stream u8][block u16 BE][shard u8][k u8][m u8][len u16 BE][data...]
//! ```

use serde::{Deserialize, Serialize};

/// Protocol version carried in announces; bump on incompatible changes.
pub const PROTOCOL_VERSION: u8 = 2;
/// Reticulum destination `kaonic.remote`.
pub const APP_NAME: &str = "kaonic";
pub const ASPECT: &str = "remote";

/// Announce app-data magic so foreign `kaonic.*` announces are ignored cheaply.
pub const ANNOUNCE_MAGIC: [u8; 2] = *b"KR";
pub const CODENAME_LEN: usize = 8;
/// Fixed announce record length.
pub const ANNOUNCE_LEN: usize = 16;

/// Largest link payload that still fits one radio frame after Reticulum
/// framing (19 B header) and link encryption (16 B IV + PKCS7 + 32 B HMAC)
/// on an 896-byte LDPC frame: 19 + 16 + 816 + 32 = 883 ≤ 896.
pub const MAX_LINK_PAYLOAD: usize = 815;
/// Blob chunk data size — chunk frame is 6 B header + data ≤ MAX_LINK_PAYLOAD.
pub const CHUNK_SIZE: usize = 800;
/// Upper bound the receiver accepts for a single blob (plugin packages).
pub const MAX_BLOB_SIZE: u32 = 96 * 1024 * 1024;

// ── Announce flags ────────────────────────────────────────────────────────────

/// Node accepts incoming pairing requests.
pub const FLAG_ACCEPTS_PAIRING: u8 = 0b0000_0001;
/// Node decodes per-frame FEC code selection (radio frames may use codes
/// other than the legacy TM2048 towards it).
pub const FLAG_FEC_SELECT: u8 = 0b0000_0010;

// ── Frame kinds ───────────────────────────────────────────────────────────────

pub const KIND_REQUEST: u8 = 0x01;
pub const KIND_RESPONSE: u8 = 0x02;
pub const KIND_CHUNK: u8 = 0x03;
/// Reed–Solomon parity shard for a block of chunks.
pub const KIND_PARITY: u8 = 0x04;
/// Media stream shard (see [`crate::media`]).
pub const KIND_MEDIA: u8 = 0x05;

/// Bulk transfer erasure block: `BULK_BLOCK_K` data chunks + `BULK_BLOCK_M`
/// parity shards (default; tunable at runtime). 2/16 = 12.5 % overhead,
/// survives two lost frames per
/// block without a round trip.
pub const BULK_BLOCK_K: u8 = 16;
pub const BULK_BLOCK_M: u8 = 2;

// ── Operations ────────────────────────────────────────────────────────────────

pub mod op {
    pub const PING: u8 = 0x01;
    pub const INFO: u8 = 0x02;

    pub const PAIR_REQUEST: u8 = 0x10;
    pub const PAIR_RESULT: u8 = 0x11;
    pub const UNPAIR: u8 = 0x12;

    pub const RADIO_GET: u8 = 0x20;
    pub const RADIO_SET: u8 = 0x21;

    pub const PLUGIN_LIST: u8 = 0x30;
    pub const PLUGIN_ACTION: u8 = 0x31;

    pub const BLOB_BEGIN: u8 = 0x40;
    pub const BLOB_STATUS: u8 = 0x41;
    pub const BLOB_END: u8 = 0x42;
    pub const BLOB_ABORT: u8 = 0x43;

    pub const SYSTEM_REBOOT: u8 = 0x50;
    pub const SERVICE_RESTART: u8 = 0x51;

    /// Run a shell command; the reply carries the exit code and the first
    /// slice of output. Remaining output is paged with [`SHELL_FETCH`].
    pub const SHELL_EXEC: u8 = 0x60;
    pub const SHELL_FETCH: u8 = 0x61;

    pub fn name(op: u8) -> &'static str {
        match op {
            PING => "ping",
            INFO => "info",
            PAIR_REQUEST => "pair-request",
            PAIR_RESULT => "pair-result",
            UNPAIR => "unpair",
            RADIO_GET => "radio-get",
            RADIO_SET => "radio-set",
            PLUGIN_LIST => "plugin-list",
            PLUGIN_ACTION => "plugin-action",
            BLOB_BEGIN => "blob-begin",
            BLOB_STATUS => "blob-status",
            BLOB_END => "blob-end",
            BLOB_ABORT => "blob-abort",
            SYSTEM_REBOOT => "system-reboot",
            SERVICE_RESTART => "service-restart",
            SHELL_EXEC => "shell-exec",
            SHELL_FETCH => "shell-fetch",
            _ => "unknown",
        }
    }
}

// ── Response status codes ─────────────────────────────────────────────────────

pub mod status {
    pub const OK: u8 = 0x00;
    pub const ERROR: u8 = 0x01;
    pub const UNAUTHORIZED: u8 = 0x02;
    pub const UNSUPPORTED: u8 = 0x03;
    pub const BAD_REQUEST: u8 = 0x04;
    /// Pairing request stored; waiting for operator approval on the target.
    pub const PENDING: u8 = 0x05;
    pub const REJECTED: u8 = 0x06;
    pub const BUSY: u8 = 0x07;
    pub const NOT_FOUND: u8 = 0x08;

    pub fn name(status: u8) -> &'static str {
        match status {
            OK => "ok",
            ERROR => "error",
            UNAUTHORIZED => "unauthorized",
            UNSUPPORTED => "unsupported",
            BAD_REQUEST => "bad-request",
            PENDING => "pending",
            REJECTED => "rejected",
            BUSY => "busy",
            NOT_FOUND => "not-found",
            _ => "unknown",
        }
    }
}

// ── Permissions (bitmask stored per paired node) ──────────────────────────────

pub mod perm {
    pub const INFO: u32 = 1 << 0;
    pub const RADIO: u32 = 1 << 1;
    pub const PLUGINS: u32 = 1 << 2;
    pub const SYSTEM: u32 = 1 << 3;
    /// Real-time media streams (PTT, video).
    pub const MEDIA: u32 = 1 << 4;
    /// Remote shell. Separate bit so it can be withheld from a paired node.
    pub const SHELL: u32 = 1 << 5;
    pub const ALL: u32 = u32::MAX;

    /// Permission bit an operation requires. Pairing ops are gated separately.
    pub fn required(op: u8) -> u32 {
        use super::op;
        match op {
            op::PING | op::INFO => INFO,
            op::RADIO_GET | op::RADIO_SET => RADIO,
            op::PLUGIN_LIST | op::PLUGIN_ACTION => PLUGINS,
            op::BLOB_BEGIN | op::BLOB_STATUS | op::BLOB_END | op::BLOB_ABORT => PLUGINS,
            op::SYSTEM_REBOOT | op::SERVICE_RESTART => SYSTEM,
            op::SHELL_EXEC | op::SHELL_FETCH => SHELL,
            op::UNPAIR => INFO,
            _ => ALL,
        }
    }
}

// ── Blob purposes ─────────────────────────────────────────────────────────────

pub mod blob {
    /// Plugin package (zip) — installed/updated through the local installer.
    pub const PLUGIN_PACKAGE: u8 = 0x01;
}

// ── Plugin actions ────────────────────────────────────────────────────────────

pub mod plugin_action {
    pub const START: u8 = 0;
    pub const STOP: u8 = 1;
    pub const RESTART: u8 = 2;
    pub const DELETE: u8 = 3;
}

// ── Announce ──────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnnounceInfo {
    pub protocol: u8,
    pub flags: u8,
    pub codename: String,
    pub gateway_version: (u8, u8, u8),
    /// Announce period the node runs on, so peers can derive "online".
    pub announce_secs: u8,
}

pub fn encode_announce(info: &AnnounceInfo) -> [u8; ANNOUNCE_LEN] {
    let mut out = [0u8; ANNOUNCE_LEN];
    out[0..2].copy_from_slice(&ANNOUNCE_MAGIC);
    out[2] = info.protocol;
    out[3] = info.flags;
    let mut name = [b' '; CODENAME_LEN];
    for (dst, src) in name.iter_mut().zip(info.codename.bytes()) {
        *dst = src;
    }
    out[4..12].copy_from_slice(&name);
    out[12] = info.gateway_version.0;
    out[13] = info.gateway_version.1;
    out[14] = info.gateway_version.2;
    out[15] = info.announce_secs;
    out
}

pub fn is_remote_announce(app_data: &[u8]) -> bool {
    app_data.len() >= ANNOUNCE_LEN && app_data[0..2] == ANNOUNCE_MAGIC
}

pub fn decode_announce(app_data: &[u8]) -> Option<AnnounceInfo> {
    if !is_remote_announce(app_data) {
        return None;
    }
    let codename = String::from_utf8_lossy(&app_data[4..12]).trim().to_string();
    Some(AnnounceInfo {
        protocol: app_data[2],
        flags: app_data[3],
        codename,
        gateway_version: (app_data[12], app_data[13], app_data[14]),
        announce_secs: app_data[15],
    })
}

/// Parse "MAJOR.MINOR.PATCH" (extra suffixes ignored) into the announce triple.
pub fn parse_version(version: &str) -> (u8, u8, u8) {
    let mut parts = version
        .split(|c: char| !c.is_ascii_digit())
        .filter(|s| !s.is_empty())
        .map(|s| s.parse::<u16>().unwrap_or(0).min(255) as u8);
    (
        parts.next().unwrap_or(0),
        parts.next().unwrap_or(0),
        parts.next().unwrap_or(0),
    )
}

pub fn format_version(v: (u8, u8, u8)) -> String {
    format!("{}.{}.{}", v.0, v.1, v.2)
}

// ── Frames ────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Frame {
    Request {
        id: u16,
        op: u8,
        body: Vec<u8>,
    },
    Response {
        id: u16,
        status: u8,
        body: Vec<u8>,
    },
    Chunk {
        transfer: u8,
        index: u32,
        data: Vec<u8>,
    },
    Parity {
        transfer: u8,
        block: u32,
        shard: u8,
        data: Vec<u8>,
    },
    Media(MediaShard),
}

/// One radio frame of a media stream: shard `shard` of block `block`, coded
/// RS(k + m, k). Data shards carry `len` real bytes (padded to the block's
/// shard length); parity shards carry `len` = 0.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MediaShard {
    pub stream: u8,
    pub block: u16,
    pub shard: u8,
    pub k: u8,
    pub m: u8,
    pub len: u16,
    pub data: Vec<u8>,
}

pub const MEDIA_HEADER_LEN: usize = 9;

impl Frame {
    pub fn encode(&self) -> Vec<u8> {
        match self {
            Frame::Request { id, op, body } => {
                let mut out = Vec::with_capacity(4 + body.len());
                out.push(KIND_REQUEST);
                out.extend_from_slice(&id.to_be_bytes());
                out.push(*op);
                out.extend_from_slice(body);
                out
            }
            Frame::Response { id, status, body } => {
                let mut out = Vec::with_capacity(4 + body.len());
                out.push(KIND_RESPONSE);
                out.extend_from_slice(&id.to_be_bytes());
                out.push(*status);
                out.extend_from_slice(body);
                out
            }
            Frame::Chunk {
                transfer,
                index,
                data,
            } => {
                let mut out = Vec::with_capacity(6 + data.len());
                out.push(KIND_CHUNK);
                out.push(*transfer);
                out.extend_from_slice(&index.to_be_bytes());
                out.extend_from_slice(data);
                out
            }
            Frame::Parity {
                transfer,
                block,
                shard,
                data,
            } => {
                let mut out = Vec::with_capacity(7 + data.len());
                out.push(KIND_PARITY);
                out.push(*transfer);
                out.extend_from_slice(&block.to_be_bytes());
                out.push(*shard);
                out.extend_from_slice(data);
                out
            }
            Frame::Media(shard) => {
                let mut out = Vec::with_capacity(MEDIA_HEADER_LEN + shard.data.len());
                out.push(KIND_MEDIA);
                out.push(shard.stream);
                out.extend_from_slice(&shard.block.to_be_bytes());
                out.push(shard.shard);
                out.push(shard.k);
                out.push(shard.m);
                out.extend_from_slice(&shard.len.to_be_bytes());
                out.extend_from_slice(&shard.data);
                out
            }
        }
    }

    pub fn decode(bytes: &[u8]) -> Option<Frame> {
        let kind = *bytes.first()?;
        match kind {
            KIND_REQUEST | KIND_RESPONSE => {
                if bytes.len() < 4 {
                    return None;
                }
                let id = u16::from_be_bytes([bytes[1], bytes[2]]);
                let body = bytes[4..].to_vec();
                Some(if kind == KIND_REQUEST {
                    Frame::Request {
                        id,
                        op: bytes[3],
                        body,
                    }
                } else {
                    Frame::Response {
                        id,
                        status: bytes[3],
                        body,
                    }
                })
            }
            KIND_CHUNK => {
                if bytes.len() < 6 {
                    return None;
                }
                let index = u32::from_be_bytes([bytes[2], bytes[3], bytes[4], bytes[5]]);
                Some(Frame::Chunk {
                    transfer: bytes[1],
                    index,
                    data: bytes[6..].to_vec(),
                })
            }
            KIND_PARITY => {
                if bytes.len() < 7 {
                    return None;
                }
                let block = u32::from_be_bytes([bytes[2], bytes[3], bytes[4], bytes[5]]);
                Some(Frame::Parity {
                    transfer: bytes[1],
                    block,
                    shard: bytes[6],
                    data: bytes[7..].to_vec(),
                })
            }
            KIND_MEDIA => {
                if bytes.len() < MEDIA_HEADER_LEN {
                    return None;
                }
                Some(Frame::Media(MediaShard {
                    stream: bytes[1],
                    block: u16::from_be_bytes([bytes[2], bytes[3]]),
                    shard: bytes[4],
                    k: bytes[5],
                    m: bytes[6],
                    len: u16::from_be_bytes([bytes[7], bytes[8]]),
                    data: bytes[MEDIA_HEADER_LEN..].to_vec(),
                }))
            }
            _ => None,
        }
    }
}

// ── Bodies (positional MessagePack) ───────────────────────────────────────────

pub fn encode_body<T: Serialize>(value: &T) -> Vec<u8> {
    rmp_serde::to_vec(value).unwrap_or_default()
}

pub fn decode_body<'a, T: Deserialize<'a>>(bytes: &'a [u8]) -> Result<T, String> {
    rmp_serde::from_slice(bytes).map_err(|err| format!("decode: {err}"))
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct PairRequestBody {
    pub codename: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct PairResultBody {
    pub accepted: bool,
    pub codename: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct InfoBody {
    pub codename: String,
    pub serial: String,
    pub gateway_version: String,
    pub uptime_secs: u64,
    pub protocol: u8,
    pub radio_modules: u8,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct DetailBody {
    pub detail: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct RadioGetBody {
    pub module: u8,
}

/// Modulation kinds carried in [`RadioConfigWire::mod_kind`].
pub mod mod_kind {
    pub const OFF: u8 = 0;
    pub const OFDM: u8 = 1;
    pub const QPSK: u8 = 2;
    pub const FSK: u8 = 3;
}

/// Radio module configuration in a transport-neutral compact form. The
/// gateway maps this onto its hardware types; `mod_a`/`mod_b` are the
/// modulation-specific parameters (OFDM: mcs/opt, QPSK: fchip/mode).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct RadioConfigWire {
    pub module: u8,
    pub freq_hz: u64,
    pub spacing_hz: u32,
    pub channel: u16,
    pub bw_filter: u8,
    pub mod_kind: u8,
    pub mod_a: u8,
    pub mod_b: u8,
    pub tx_power: u8,
    pub accelerator: u8,
    pub antenna: u8,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct PluginInfoWire {
    pub id: String,
    pub name: String,
    pub version: String,
    pub active: bool,
    pub enabled: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct PluginActionBody {
    pub id: String,
    pub action: u8,
}

/// Output bytes carried in one shell reply (keeps it inside a radio frame).
pub const SHELL_SLICE: usize = 512;
/// Total output the target buffers for paging.
pub const SHELL_MAX_OUTPUT: usize = 64 * 1024;

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ShellExecBody {
    pub command: String,
    /// Kill the command after this many seconds (clamped by the target).
    pub timeout_secs: u32,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ShellFetchBody {
    pub offset: u32,
}

/// Exit status plus a slice of the combined stdout/stderr. `total` is the
/// full output length so the controller knows how much is left to page.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ShellResultBody {
    pub code: i32,
    pub total: u32,
    pub offset: u32,
    pub chunk: String,
    /// Output was longer than [`SHELL_MAX_OUTPUT`] and was cut.
    pub truncated: bool,
    pub duration_ms: u32,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ServiceRestartBody {
    pub unit: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct BlobBeginBody {
    pub purpose: u8,
    pub name: String,
    pub size: u32,
    pub sha256: Vec<u8>,
    pub chunk_size: u16,
    /// Erasure block: data chunks per block and parity shards per block
    /// (0 parity = no outer code).
    pub block_k: u8,
    pub block_m: u8,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct BlobBeginResponse {
    pub transfer: u8,
    pub next_index: u32,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct BlobRefBody {
    pub transfer: u8,
}

/// Receiver-side progress: `next_index` is the first chunk not yet received;
/// bit `i` of `received_mask` says chunk `next_index + 1 + i` already arrived.
/// `highest` is one past the highest chunk received so far and `missing`
/// lists holes below it (bounded to [`MAX_MISSING_REPORT`]), so the sender
/// can retransmit exactly what was lost on a lossy radio and keep streaming.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct BlobStatusResponse {
    pub next_index: u32,
    pub received_mask: u32,
    pub complete: bool,
    pub highest: u32,
    pub missing: Vec<u32>,
}

/// Cap on holes listed in one status reply (keeps the reply in one frame).
pub const MAX_MISSING_REPORT: usize = 96;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn announce_round_trip() {
        let info = AnnounceInfo {
            protocol: PROTOCOL_VERSION,
            flags: FLAG_ACCEPTS_PAIRING,
            codename: "b4o0cvts".into(),
            gateway_version: (0, 2, 5),
            announce_secs: 20,
        };
        let bytes = encode_announce(&info);
        assert_eq!(bytes.len(), ANNOUNCE_LEN);
        assert!(is_remote_announce(&bytes));
        assert_eq!(decode_announce(&bytes), Some(info));
        assert!(!is_remote_announce(b"KV"));
    }

    #[test]
    fn frames_round_trip() {
        for frame in [
            Frame::Request {
                id: 0x1234,
                op: op::RADIO_GET,
                body: vec![1, 2, 3],
            },
            Frame::Response {
                id: 7,
                status: status::PENDING,
                body: vec![],
            },
            Frame::Chunk {
                transfer: 3,
                index: 0xdead_beef,
                data: vec![9; CHUNK_SIZE],
            },
            Frame::Parity {
                transfer: 3,
                block: 77,
                shard: 1,
                data: vec![4; CHUNK_SIZE],
            },
            Frame::Media(MediaShard {
                stream: 2,
                block: 65000,
                shard: 4,
                k: 4,
                m: 1,
                len: 120,
                data: vec![5; 120],
            }),
        ] {
            let encoded = frame.encode();
            assert!(encoded.len() <= MEDIA_HEADER_LEN + CHUNK_SIZE);
            assert_eq!(Frame::decode(&encoded), Some(frame));
        }
        assert_eq!(Frame::decode(&[]), None);
        assert_eq!(Frame::decode(&[KIND_REQUEST, 0]), None);
        assert_eq!(Frame::decode(&[0x77, 1, 2, 3, 4, 5, 6]), None);
    }

    #[test]
    fn chunk_frame_fits_one_radio_frame() {
        let frame = Frame::Chunk {
            transfer: 0,
            index: u32::MAX,
            data: vec![0; CHUNK_SIZE],
        };
        assert!(frame.encode().len() <= MAX_LINK_PAYLOAD);
    }

    #[test]
    fn radio_config_body_is_compact() {
        let cfg = RadioConfigWire {
            module: 0,
            freq_hz: 869_535_000,
            spacing_hz: 200_000,
            channel: 3,
            bw_filter: 0,
            mod_kind: mod_kind::OFDM,
            mod_a: 6,
            mod_b: 0,
            tx_power: 10,
            accelerator: 0,
            antenna: 0,
        };
        let bytes = encode_body(&cfg);
        assert!(bytes.len() < 32, "radio config body {} B", bytes.len());
        assert_eq!(decode_body::<RadioConfigWire>(&bytes).unwrap(), cfg);
    }

    #[test]
    fn version_parsing() {
        assert_eq!(parse_version("0.2.5"), (0, 2, 5));
        assert_eq!(parse_version("1.10.300-beta"), (1, 10, 255));
        assert_eq!(parse_version("garbage"), (0, 0, 0));
    }
}
