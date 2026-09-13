//! Host integration: the runtime authenticates and authorizes every request,
//! then hands the typed command to a [`CommandHandler`] implemented by the
//! host (the gateway). Handlers never see unauthenticated traffic.

use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;

use reticulum::hash::AddressHash;

use crate::protocol::{
    op, status, InfoBody, PluginInfoWire, RadioConfigWire, ServicesBody, ShellResultBody,
};

pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Typed, already-authorized request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    Ping,
    Info,
    /// Which plugin destinations this node serves.
    Services,
    RadioGet {
        module: u8,
    },
    RadioSet(RadioConfigWire),
    PluginList,
    PluginAction {
        id: String,
        action: u8,
    },
    SystemReboot,
    ServiceRestart {
        unit: String,
    },
    /// Run a shell command and buffer its output for paging.
    ShellExec {
        command: String,
        timeout_secs: u32,
    },
    /// Next slice of the last command's output.
    ShellFetch {
        offset: u32,
    },
    /// A fully received and hash-verified blob at `path`; the handler consumes
    /// (moves/deletes) the file.
    ApplyBlob {
        purpose: u8,
        name: String,
        path: PathBuf,
    },
}

impl Command {
    pub fn op(&self) -> u8 {
        match self {
            Command::Ping => op::PING,
            Command::Info => op::INFO,
            Command::Services => op::SERVICES,
            Command::RadioGet { .. } => op::RADIO_GET,
            Command::RadioSet(_) => op::RADIO_SET,
            Command::PluginList => op::PLUGIN_LIST,
            Command::PluginAction { .. } => op::PLUGIN_ACTION,
            Command::SystemReboot => op::SYSTEM_REBOOT,
            Command::ServiceRestart { .. } => op::SERVICE_RESTART,
            Command::ShellExec { .. } => op::SHELL_EXEC,
            Command::ShellFetch { .. } => op::SHELL_FETCH,
            Command::ApplyBlob { .. } => op::BLOB_END,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reply {
    Empty,
    Detail(String),
    Info(InfoBody),
    Services(ServicesBody),
    Radio(RadioConfigWire),
    Plugins(Vec<PluginInfoWire>),
    Shell(ShellResultBody),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteError {
    pub status: u8,
    pub detail: String,
}

impl RemoteError {
    pub fn new(status: u8, detail: impl Into<String>) -> Self {
        Self {
            status,
            detail: detail.into(),
        }
    }

    pub fn error(detail: impl Into<String>) -> Self {
        Self::new(status::ERROR, detail)
    }

    pub fn unsupported(detail: impl Into<String>) -> Self {
        Self::new(status::UNSUPPORTED, detail)
    }

    pub fn bad_request(detail: impl Into<String>) -> Self {
        Self::new(status::BAD_REQUEST, detail)
    }

    pub fn not_found(detail: impl Into<String>) -> Self {
        Self::new(status::NOT_FOUND, detail)
    }
}

impl std::fmt::Display for RemoteError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.detail.is_empty() {
            f.write_str(status::name(self.status))
        } else {
            write!(f, "{}: {}", status::name(self.status), self.detail)
        }
    }
}

impl std::error::Error for RemoteError {}

/// Traffic class a session asks the radio layer to use for a destination.
/// The host maps this onto its FEC/modulation policy at runtime; nothing is
/// persisted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkClass {
    /// Small request/response traffic: maximum correction.
    Control,
    /// Streaming chunks: cheapest code the measured link supports.
    Bulk,
    /// Real-time media: minimum airtime and latency.
    Media,
}

/// Host hook for per-destination link tuning (e.g. FEC selection).
pub trait LinkPolicy: Send + Sync {
    /// `capable` says whether the peer advertised support for code
    /// selection; hosts must stay wire-compatible when it is false.
    fn set_class(&self, destination: AddressHash, class: LinkClass, capable: bool);
    fn clear(&self, destination: &AddressHash);
    /// Last measured signal strength from `destination`, if known.
    fn link_rssi(&self, destination: &AddressHash) -> Option<i8>;
}

pub trait CommandHandler: Send + Sync {
    fn handle(&self, command: Command) -> BoxFuture<'_, Result<Reply, RemoteError>>;
}

/// Handler that rejects everything — useful for hosts that only act as
/// controllers, and for tests.
pub struct NoopHandler;

impl CommandHandler for NoopHandler {
    fn handle(&self, command: Command) -> BoxFuture<'_, Result<Reply, RemoteError>> {
        Box::pin(async move {
            match command {
                Command::Ping => Ok(Reply::Empty),
                other => Err(RemoteError::unsupported(op::name(other.op()))),
            }
        })
    }
}
