#[derive(Clone, PartialEq, prost::Message)]
pub struct Message {
    #[prost(string, tag = "1")]
    pub dataset: String,
    #[prost(string, tag = "2")]
    pub row: String,
    #[prost(string, tag = "3")]
    pub column: String,
    #[prost(string, tag = "4")]
    pub value: String,
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct MessageEnvelope {
    #[prost(string, tag = "1")]
    pub timestamp: String,
    #[prost(bool, tag = "2")]
    pub is_encrypted: bool,
    #[prost(bytes = "vec", tag = "3")]
    pub content: Vec<u8>,
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct SyncRequest {
    #[prost(message, repeated, tag = "1")]
    pub messages: Vec<MessageEnvelope>,
    #[prost(string, tag = "2")]
    pub file_id: String,
    #[prost(string, tag = "3")]
    pub group_id: String,
    // tag 4 is reserved in the .proto, skip it
    #[prost(string, tag = "5")]
    pub key_id: String,
    #[prost(string, tag = "6")]
    pub since: String,
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct SyncResponse {
    #[prost(message, repeated, tag = "1")]
    pub messages: Vec<MessageEnvelope>,
    #[prost(string, tag = "2")]
    pub merkle: String,
}
