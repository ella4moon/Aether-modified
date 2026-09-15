use thiserror::Error;

#[derive(Debug, Error)]
pub enum AetherError {
    #[error("Aether is already running")]
    AlreadyRunning,
    #[error("Aether binary not found at {0}")]
    BinaryMissing(String),
    #[error("failed to launch Aether: {0}")]
    SpawnFailed(String),
    #[error("port {0} is already in use by another process")]
    PortInUse(u16),
    #[error("no active connection")]
    NotConnected,
    #[error("final proxy: {0}")]
    FinalProxy(String),
    #[error("whole laptop: {0}")]
    WholeLaptop(String),
    #[error("internal error: {0}")]
    Internal(String),
}

// Tauri v2 command errors must be Serialize; Aether-GUI has no need to
// distinguish error variants on the frontend beyond the message text, so
// this serializes to a plain string rather than a tagged enum.
impl serde::Serialize for AetherError {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(&self.to_string())
    }
}
