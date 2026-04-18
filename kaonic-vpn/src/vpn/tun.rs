//! Platform TUN wrapper.
//!
//! The reader owns its own stack buffer, so there is no outer Mutex on the
//! read side (the previous implementation serialised every packet through a
//! `Mutex<[u8; MTU]>` guard for no benefit — only one reader task exists).

use std::sync::Arc;

#[cfg(target_os = "linux")]
pub const TUN_NAME_TEMPLATE: &str = "kaonic-vpn%d";
pub const TUN_MTU: usize = 1400;
/// MSS clamped to the negotiated path MTU minus IPv4+TCP headers.
pub const TCP_MSS: usize = TUN_MTU - 40;

#[cfg(target_os = "linux")]
pub type SharedTun = Arc<LinuxTun>;

#[cfg(not(target_os = "linux"))]
pub type SharedTun = Arc<MockTun>;

/// Open a platform TUN, or `None` on non-Linux dev machines (no spawn).
#[cfg(target_os = "linux")]
pub fn open_platform() -> std::io::Result<Option<SharedTun>> {
    LinuxTun::open().map(Some)
}

#[cfg(not(target_os = "linux"))]
pub fn open_platform() -> std::io::Result<Option<SharedTun>> {
    Ok(None)
}

#[cfg(target_os = "linux")]
pub struct LinuxTun {
    tun: riptun::TokioTun,
    name: String,
}

#[cfg(target_os = "linux")]
impl LinuxTun {
    pub fn open() -> std::io::Result<Arc<Self>> {
        let tun = riptun::TokioTun::new(TUN_NAME_TEMPLATE, 1)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e.to_string()))?;
        let name = tun.name().to_string();
        Ok(Arc::new(Self { tun, name }))
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    /// Reads one packet into `buf` and returns the filled slice length.
    pub async fn recv(&self, buf: &mut [u8]) -> std::io::Result<usize> {
        self.tun.recv(buf).await
    }

    pub async fn send(&self, data: &[u8]) -> std::io::Result<usize> {
        self.tun.send(data).await
    }
}

#[cfg(not(target_os = "linux"))]
pub struct MockTun;

#[cfg(not(target_os = "linux"))]
impl MockTun {
    pub fn open() -> std::io::Result<Arc<Self>> {
        Ok(Arc::new(Self))
    }

    pub fn name(&self) -> &str {
        "mock"
    }

    pub async fn recv(&self, _buf: &mut [u8]) -> std::io::Result<usize> {
        // Keep the read task parked indefinitely on non-Linux targets.
        std::future::pending::<()>().await;
        unreachable!()
    }

    pub async fn send(&self, data: &[u8]) -> std::io::Result<usize> {
        Ok(data.len())
    }
}
