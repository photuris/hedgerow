//! Error type shared by every retkerno-core module.

/// Everything that can go wrong in retkerno-core.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// Filesystem failure.
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    /// SQLite failure.
    #[error("database: {0}")]
    Db(#[from] rusqlite::Error),
    /// Malformed or unusable configuration.
    #[error("config: {0}")]
    Config(String),
    /// A required setting is missing.
    #[error("not configured: {0}")]
    NotConfigured(String),
    /// Caller-supplied input failed validation.
    #[error("invalid: {0}")]
    Invalid(String),
    /// The operation needs a connected radio.
    #[error("not connected")]
    NotConnected,
    /// Transport, timeout, or library failure: the outcome is unknown.
    #[error("radio: {0}")]
    Radio(String),
    /// The device answered with an Error frame: nothing changed.
    #[error("rejected by radio: {0}")]
    Rejected(String),
    /// The database was written by a newer build.
    #[error("database schema {found} is newer than this build ({supported})")]
    SchemaTooNew {
        /// Schema version found in the database.
        found: u32,
        /// Newest schema version this build understands.
        supported: u32,
    },
}

/// Result alias using [`Error`].
pub type Result<T> = std::result::Result<T, Error>;
