use std::path::PathBuf;

/// The replica layer speaks SQLite and the filesystem — a different vocabulary
/// from `ActualError`, so it gets its own enum.
#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("could not create the cache directory {path}")]
    CacheDir {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("could not write the replica at {path}")]
    Write {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("replica database error")]
    Sqlite(#[from] rusqlite::Error),

    #[error("sync message {timestamp} targets unknown table {dataset:?}")]
    UnknownTable { timestamp: String, dataset: String },

    #[error("sync message {timestamp} targets unknown column {column:?} on table {dataset}")]
    UnknownColumn {
        timestamp: String,
        dataset: String,
        column: String,
    },

    #[error("sync message {timestamp} tries to rewrite the primary key of {dataset}")]
    RewritesPrimaryKey { timestamp: String, dataset: String },

    #[error("sync message {timestamp} carries a number that cannot be stored: {value}")]
    UnrepresentableNumber { timestamp: String, value: f64 },

    #[error("the replica has no stored clock; it may be corrupt — delete the cache directory")]
    NoClock,

    #[error("replica task failed")]
    Join(#[from] tokio::task::JoinError),
}
