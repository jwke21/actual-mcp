use crate::config::ACTUAL_SYNC_ID;

#[derive(Debug, thiserror::Error)]
pub enum ActualError {
    #[error("could not reach the Actual server at {url}")]
    Transport {
        url: String,
        #[source]
        source: reqwest::Error,
    },
    // reason from the envelope
    #[error("authentication failed: {0}")]
    Auth(String),
    #[error("Actual server returned {status}: {reason}")]
    Api { status: u16, reason: String },
    #[error("unexpected response from {endpoint}")]
    Decode {
        endpoint: &'static str,
        #[source]
        source: serde_json::Error,
    },
    // encrypt_key_id was non-null
    #[error("budget '{name}' is end-to-end encrypted, which is not supported")]
    Encrypted { name: String },
    #[error("no budget file found on the Actual server")]
    NoBudget,
    #[error("multiple budgets found; set '{ACTUAL_SYNC_ID}' to one of: {}", names.join(", "))]
    AmbiguousBudget { names: Vec<String> },
    #[error("no budget with sync id '{sync_id}' found; available: {}", available.join(", "))]
    BudgetNotFound {
        sync_id: String,
        available: Vec<String>,
    },
    #[error("could not read the budget snapshot: {reason}")]
    BadSnapshot { reason: String },
    #[error("sync message {timestamp} is unusable: {reason}")]
    BadMessage { timestamp: String, reason: String },
    #[error("could not decode the sync response")]
    BadSyncResponse(#[source] prost::DecodeError),
}
