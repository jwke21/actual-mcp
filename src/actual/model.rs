use serde::Deserialize;

#[derive(Debug, Deserialize)]
#[serde(tag = "status")]
pub enum Envelope<T> {
    #[serde(rename = "ok")]
    Success { data: T },
    #[serde(rename = "error")]
    Failure {
        reason: String,
        #[serde(default)]
        details: Option<String>,
    },
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UserFile {
    pub file_id: String,
    pub group_id: Option<String>,
    pub name: String,
    pub encrypt_key_id: Option<String>,
    #[serde(default)]
    pub deleted: i32, // server sends an int, not a bool
}

#[derive(Debug, Deserialize)]
pub struct LoginData {
    pub token: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Metadata {
    pub id: String,
    pub budget_name: String,
    pub cloud_file_id: String,
    pub group_id: Option<String>,
    pub last_synced_timestamp: Option<String>,
    #[serde(default)]
    pub reset_clock: bool,
}

#[derive(Debug)]
pub struct Snapshot {
    pub db_bytes: Vec<u8>,
    pub metadata: Metadata,
}

#[derive(Debug, Deserialize)]
pub(crate) struct ApiError {
    pub reason: String,
}
