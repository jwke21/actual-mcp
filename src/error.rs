use rmcp::{ErrorData, model::ErrorCode};

use crate::actual::error::ActualError;
use crate::store::error::StoreError;

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("{0} is not set")]
    Missing(&'static str),

    #[error("{var} is invalid: {reason}")]
    Invalid { var: &'static str, reason: String },

    #[error("could not determine a cache directory; set ACTUAL_CACHE_DIR")]
    NoCacheDir,
}

#[derive(Debug, thiserror::Error)]
pub enum ToolError {
    /// the call was malformed in a way the model can correct
    #[error("{0}")]
    BadArgument(String),

    /// couldn't reach or read the budget at all
    #[error("budget data is unavailable: {0}")]
    Unavailable(#[from] ActualError),

    /// the local replica could not be read or written
    #[error("budget data is unavailable: {0}")]
    Replica(#[from] StoreError),

    /// unexpected error, model can do nothing with the detail
    #[error("internal error")]
    Internal(#[source] anyhow::Error),
}

impl From<ToolError> for ErrorData {
    fn from(err: ToolError) -> Self {
        match err {
            ToolError::BadArgument(msg) => ErrorData::invalid_params(msg, None),

            ToolError::Replica(source) => {
                tracing::error!(error = ?source, "replica error");
                ErrorData::new(
                    ErrorCode::INTERNAL_ERROR,
                    "Budget data is unavailable: the local cache could not be read".to_string(),
                    None,
                )
            }

            ToolError::Unavailable(msg) => ErrorData::new(
                ErrorCode::INTERNAL_ERROR,
                format!("Budget data is unavailable: {msg}"),
                None,
            ),

            ToolError::Internal(source) => {
                // full detail to stderr where you can debug it
                tracing::error!(error = ?source, "internal tool error");
                // a short message to the model, which can't act on a SQL string anyway
                ErrorData::internal_error("internal error", None)
            }
        }
    }
}

/// Everything that can go wrong bringing the server up. Reported once, to
/// stderr, before the process exits (FR-1.2).
#[derive(Debug, thiserror::Error)]
pub enum StartupError {
    #[error(transparent)]
    Config(#[from] ConfigError),

    #[error(transparent)]
    Actual(#[from] ActualError),

    #[error(transparent)]
    Replica(#[from] StoreError),

    #[error("the selected budget has no group id; it may never have been synced")]
    NoGroupId,
}
