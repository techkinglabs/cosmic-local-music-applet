#[derive(Debug, thiserror::Error)]
pub enum AppError {
    #[error("D-Bus error: {0}")]
    Dbus(#[from] zbus::Error),

    #[error("MPRIS error: {0}")]
    Mpris(String),

    #[error("No active media source found")]
    NoActiveSource,

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Scan error: {0}")]
    Scan(String),
}

impl AppError {
    pub fn new<S: Into<String>>(msg: S) -> Self {
        AppError::Mpris(msg.into())
    }
}
