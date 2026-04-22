//! Wire format for VPN announce + in-link control messages (msgpack).
//!
//! Announce payload (carried in Reticulum announce `app_data`):
//!   [ ANNOUNCE_MAGIC (4B) ][ version (1B) ][ msgpack(Announce) ]
//!
//! In-link payload (carried inside a Reticulum link data packet):
//!   - First byte `CTRL_TAG` (0xFB) + msgpack(Ctrl)   -- control frame
//!   - Otherwise raw IPv4/IPv6 packet
//!
//! 0xFB is chosen because valid IP packet first bytes start with 0x4X (IPv4)
//! or 0x6X (IPv6), so a one-byte check unambiguously separates them.
//!
//! Routes are encoded as compact `(u32_be_ip, u8_prefix)` tuples.

use std::net::Ipv4Addr;

use cidr::Ipv4Cidr;
use serde::{Deserialize, Serialize};

pub const ANNOUNCE_MAGIC: &[u8; 4] = b"kvpn";
pub const ANNOUNCE_VERSION: u8 = 2;
pub const CTRL_TAG: u8 = 0xFB;

#[derive(Debug)]
pub enum CodecError {
    Short,
    BadMagic,
    Version(u8),
    Decode(String),
    Encode(String),
    Prefix(u8),
}

impl std::fmt::Display for CodecError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Short => write!(f, "payload too short"),
            Self::BadMagic => write!(f, "bad magic"),
            Self::Version(v) => write!(f, "unsupported version {v}"),
            Self::Decode(e) => write!(f, "msgpack decode: {e}"),
            Self::Encode(e) => write!(f, "msgpack encode: {e}"),
            Self::Prefix(p) => write!(f, "invalid subnet prefix {p}"),
        }
    }
}

impl std::error::Error for CodecError {}

#[derive(Clone, Serialize, Deserialize)]
pub struct Subnet(pub u32, pub u8);

impl Subnet {
    pub fn from_cidr(cidr: Ipv4Cidr) -> Self {
        Self(u32::from(cidr.first_address()), cidr.network_length())
    }

    pub fn to_cidr(&self) -> Result<Ipv4Cidr, CodecError> {
        if self.1 > 32 {
            return Err(CodecError::Prefix(self.1));
        }
        Ipv4Cidr::new(Ipv4Addr::from(self.0), self.1).map_err(|_| CodecError::Prefix(self.1))
    }
}

/// Announce body (msgpack after the magic + version prefix).
///
/// Routes in the announce are optional — announce broadcasts should stay small
/// so route lists prefer to flow over link `Ctrl::Routes` messages instead.
#[derive(Default, Serialize, Deserialize)]
pub struct Announce {
    #[serde(default)]
    pub routes: Vec<Subnet>,
}

/// Control frames carried inside a link. Uses short integer tags for compactness.
#[derive(Serialize, Deserialize)]
#[serde(tag = "t", content = "v")]
pub enum Ctrl {
    /// Sent once by each side when a link activates; carries the sender's exported routes.
    #[serde(rename = "h")]
    Hello { routes: Vec<Subnet> },
    /// Periodic/incremental route refresh.
    #[serde(rename = "r")]
    Routes { routes: Vec<Subnet> },
    /// Heartbeat; receiver updates link liveness.
    #[serde(rename = "p")]
    Ping,
}

// ── Announce ─────────────────────────────────────────────────────────────────

pub fn encode_announce(routes: &[Ipv4Cidr]) -> Result<Vec<u8>, CodecError> {
    let body = Announce {
        routes: routes.iter().copied().map(Subnet::from_cidr).collect(),
    };
    let mut out = Vec::with_capacity(8 + routes.len() * 6);
    out.extend_from_slice(ANNOUNCE_MAGIC);
    out.push(ANNOUNCE_VERSION);
    rmp_serde::encode::write_named(&mut out, &body)
        .map_err(|e| CodecError::Encode(e.to_string()))?;
    Ok(out)
}

pub fn decode_announce(data: &[u8]) -> Result<Vec<Ipv4Cidr>, CodecError> {
    if data.len() < 5 {
        return Err(CodecError::Short);
    }
    if &data[..4] != ANNOUNCE_MAGIC {
        return Err(CodecError::BadMagic);
    }
    let version = data[4];
    if version != ANNOUNCE_VERSION {
        return Err(CodecError::Version(version));
    }
    let body: Announce =
        rmp_serde::from_slice(&data[5..]).map_err(|e| CodecError::Decode(e.to_string()))?;
    body.routes.iter().map(Subnet::to_cidr).collect()
}

pub fn is_announce(data: &[u8]) -> bool {
    data.len() >= 5 && &data[..4] == ANNOUNCE_MAGIC
}

// ── In-link Ctrl ─────────────────────────────────────────────────────────────

pub fn encode_ctrl(msg: &Ctrl) -> Result<Vec<u8>, CodecError> {
    let mut out = Vec::with_capacity(32);
    out.push(CTRL_TAG);
    rmp_serde::encode::write_named(&mut out, msg).map_err(|e| CodecError::Encode(e.to_string()))?;
    Ok(out)
}

pub fn encode_hello(routes: &[Ipv4Cidr]) -> Result<Vec<u8>, CodecError> {
    encode_ctrl(&Ctrl::Hello {
        routes: routes.iter().copied().map(Subnet::from_cidr).collect(),
    })
}

pub fn encode_routes(routes: &[Ipv4Cidr]) -> Result<Vec<u8>, CodecError> {
    encode_ctrl(&Ctrl::Routes {
        routes: routes.iter().copied().map(Subnet::from_cidr).collect(),
    })
}

pub fn encode_ping() -> Vec<u8> {
    // Ping is tiny & never fails — serialise once into a local buf.
    encode_ctrl(&Ctrl::Ping).expect("encode ping")
}

pub fn decode_ctrl(data: &[u8]) -> Result<Ctrl, CodecError> {
    if data.is_empty() || data[0] != CTRL_TAG {
        return Err(CodecError::BadMagic);
    }
    rmp_serde::from_slice(&data[1..]).map_err(|e| CodecError::Decode(e.to_string()))
}

#[inline]
pub fn is_ctrl(data: &[u8]) -> bool {
    matches!(data.first(), Some(&CTRL_TAG))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn announce_round_trips() {
        let routes: Vec<Ipv4Cidr> = vec![
            "192.168.10.0/24".parse().unwrap(),
            "10.0.0.0/8".parse().unwrap(),
        ];
        let bytes = encode_announce(&routes).unwrap();
        assert!(is_announce(&bytes));
        let got = decode_announce(&bytes).unwrap();
        assert_eq!(got, routes);
    }

    #[test]
    fn empty_announce_round_trips() {
        let bytes = encode_announce(&[]).unwrap();
        assert!(decode_announce(&bytes).unwrap().is_empty());
    }

    #[test]
    fn ctrl_hello_round_trips() {
        let routes: Vec<Ipv4Cidr> = vec!["192.168.77.0/24".parse().unwrap()];
        let bytes = encode_hello(&routes).unwrap();
        assert!(is_ctrl(&bytes));
        match decode_ctrl(&bytes).unwrap() {
            Ctrl::Hello { routes: got } => {
                let got: Vec<Ipv4Cidr> = got.iter().map(|s| s.to_cidr().unwrap()).collect();
                assert_eq!(got, routes);
            }
            _ => panic!("expected Hello"),
        }
    }

    #[test]
    fn ping_encodes_small() {
        let p = encode_ping();
        assert!(is_ctrl(&p));
        assert!(p.len() < 24, "ping should stay compact: {} bytes", p.len());
        assert!(matches!(decode_ctrl(&p).unwrap(), Ctrl::Ping));
    }

    #[test]
    fn ip_packet_is_not_ctrl() {
        // 0x45 = IPv4, header length 5 words
        let ip_pkt = [0x45u8, 0x00, 0x00, 0x28];
        assert!(!is_ctrl(&ip_pkt));
        assert!(!is_announce(&ip_pkt));
    }
}
