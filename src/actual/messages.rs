use crate::actual::{error::ActualError, proto};
use prost::Message as _;

/// The epoch-zero HLC. The server compares `since` as a plain string
/// (`WHERE timestamp > ?`), so this sorts before every real message and
/// returns the full backlog.
pub const ZERO_CLOCK: &str = "1970-01-01T00:00:00.000Z-0000-0000000000000000";

#[derive(Debug, Clone, PartialEq)]
pub struct SyncMessage {
    pub timestamp: String,
    pub dataset: String,
    pub row: String,
    pub column: String,
    pub value: CrdtValue,
}

#[derive(Debug, Clone, PartialEq)]
pub enum CrdtValue {
    Str(String),
    Num(f64),
    Null,
}

impl CrdtValue {
    /// Actual serializes CRDT values with a two-character type tag:
    /// `S:` string, `N:` number, `0:` null.
    /// See `serializeValue` in loot-core's `server/sync/index.ts`
    ///
    /// Returns the reason on failure. The caller attaches the timestamp
    pub fn parse(raw: &str) -> Result<Self, String> {
        if let Some(s) = raw.strip_prefix("S:") {
            return Ok(Self::Str(s.to_string()));
        }
        if let Some(n) = raw.strip_prefix("N:") {
            return n
                .parse::<f64>()
                .map(Self::Num)
                .map_err(|e| format!("{raw:?} has an N: prefix but {e}"));
        }
        if raw == "0:" {
            return Ok(Self::Null);
        }
        Err(format!("unknown prefix value in {raw:?}"))
    }
}

/// Functional core: turn the raw protobuf body of a `/sync/sync` response into
/// domain messages. Pure, so every branch is unit-testable.
pub fn decode_response(bytes: &[u8]) -> Result<Vec<SyncMessage>, ActualError> {
    let response = proto::SyncResponse::decode(bytes).map_err(ActualError::BadSyncResponse)?;
    response.messages.into_iter().map(decode_envelope).collect()
}

fn decode_envelope(envelope: proto::MessageEnvelope) -> Result<SyncMessage, ActualError> {
    // attach timestamp that `CrdtValue::parse` cannot know about
    let bad = |reason: String| ActualError::BadMessage {
        timestamp: envelope.timestamp.clone(),
        reason,
    };

    // should be unreachable: `select` rejects encrypted budgets up front
    if envelope.is_encrypted {
        return Err(bad(
            "message is encrypted, encrypted budgets are not supported".to_string(),
        ));
    }

    // the double decode: `content` is itself an encoded message
    let inner = proto::Message::decode(&envelope.content[..])
        .map_err(|e| bad(format!("content is not a valid message: {e}")))?;

    let value = CrdtValue::parse(&inner.value).map_err(bad)?;

    Ok(SyncMessage {
        timestamp: envelope.timestamp,
        dataset: inner.dataset,
        row: inner.row,
        column: inner.column,
        value,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const TS: &str = "2026-09-06T20:45:09.463Z-0001-9b548650683a3f7d";
    const TS2: &str = "2026-09-06T20:45:09.463Z-0002-9b548650683a3f7d";

    /// An envelope carrying a properly encoded inner Message.
    fn envelope(
        timestamp: &str,
        dataset: &str,
        column: &str,
        value: &str,
    ) -> proto::MessageEnvelope {
        let inner = proto::Message {
            dataset: dataset.to_string(),
            row: "row-1".to_string(),
            column: column.to_string(),
            value: value.to_string(),
        };
        proto::MessageEnvelope {
            timestamp: timestamp.to_string(),
            is_encrypted: false,
            content: inner.encode_to_vec(),
        }
    }

    fn wire(messages: Vec<proto::MessageEnvelope>) -> Vec<u8> {
        proto::SyncResponse {
            messages,
            merkle: "{}".to_string(),
        }
        .encode_to_vec()
    }

    /// Unwrap a BadMessage so tests can assert on *which* message failed and why.
    fn bad_message(err: ActualError) -> (String, String) {
        match err {
            ActualError::BadMessage { timestamp, reason } => (timestamp, reason),
            other => panic!("expected BadMessage, got {other:?}"),
        }
    }

    // ---- decode_response ----

    #[test]
    fn decodes_multiple_messages() {
        let bytes = wire(vec![
            envelope(TS, "transactions", "amount", "N:-94300"),
            envelope(TS2, "transactions", "notes", "S:coffee"),
        ]);

        let msgs = decode_response(&bytes).expect("should decode");

        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[0].timestamp, TS);
        assert_eq!(msgs[0].dataset, "transactions");
        assert_eq!(msgs[0].column, "amount");
        assert_eq!(msgs[0].value, CrdtValue::Num(-94300.0));
        assert_eq!(msgs[1].value, CrdtValue::Str("coffee".to_string()));
    }

    /// The steady state: nothing new since `since`. Must not be an error.
    ///
    /// This is also what a *wrong group id* returns, because the server creates
    /// an empty group database rather than failing — so a live test needs to
    /// assert count > 0 to tell the two apart.
    #[test]
    fn empty_response_is_ok() {
        let msgs = decode_response(&wire(vec![])).expect("empty response is valid");
        assert!(msgs.is_empty());
    }

    #[test]
    fn null_values_decode() {
        let bytes = wire(vec![envelope(TS, "transactions", "category", "0:")]);
        let msgs = decode_response(&bytes).unwrap();
        assert_eq!(msgs[0].value, CrdtValue::Null);
    }

    #[test]
    fn encrypted_message_is_rejected() {
        let mut env = envelope(TS, "transactions", "amount", "N:1");
        env.is_encrypted = true;

        let (timestamp, reason) = bad_message(decode_response(&wire(vec![env])).unwrap_err());
        assert_eq!(timestamp, TS, "the failing message must be identifiable");
        assert!(reason.contains("encrypted"), "got {reason:?}");
    }

    #[test]
    fn garbage_content_is_rejected() {
        let env = proto::MessageEnvelope {
            timestamp: TS.to_string(),
            is_encrypted: false,
            // not a valid encoded Message: field 1 claims 200 bytes that aren't there
            content: vec![0x0a, 0xc8, 0x01, 0x00],
        };

        let (timestamp, reason) = bad_message(decode_response(&wire(vec![env])).unwrap_err());
        assert_eq!(timestamp, TS);
        assert!(reason.contains("not a valid message"), "got {reason:?}");
    }

    #[test]
    fn unknown_value_prefix_is_rejected() {
        let bytes = wire(vec![envelope(TS, "transactions", "amount", "X:whatever")]);

        let (timestamp, reason) = bad_message(decode_response(&bytes).unwrap_err());
        assert_eq!(timestamp, TS);
        assert!(reason.contains("unknown"), "got {reason:?}");
        assert!(
            reason.contains("X:whatever"),
            "reason should quote the value: {reason:?}"
        );
    }

    /// A failure anywhere aborts the batch: a partially applied set of CRDT
    /// messages would leave the replica in a state no clock describes.
    #[test]
    fn one_bad_message_fails_the_batch() {
        let bytes = wire(vec![
            envelope(TS, "transactions", "amount", "N:1"),
            envelope(TS2, "transactions", "amount", "X:bad"),
        ]);
        assert!(decode_response(&bytes).is_err());
    }

    #[test]
    fn truncated_response_is_rejected() {
        let bytes = wire(vec![envelope(TS, "transactions", "amount", "N:1")]);
        let truncated = &bytes[..bytes.len() - 1];

        match decode_response(truncated) {
            Err(ActualError::BadSyncResponse(_)) => {}
            other => panic!("expected BadSyncResponse, got {other:?}"),
        }
    }

    // ---- CrdtValue::parse ----

    #[test]
    fn parses_each_value_tag() {
        assert_eq!(
            CrdtValue::parse("S:hello"),
            Ok(CrdtValue::Str("hello".into()))
        );
        assert_eq!(CrdtValue::parse("N:-94300"), Ok(CrdtValue::Num(-94300.0)));
        assert_eq!(CrdtValue::parse("0:"), Ok(CrdtValue::Null));
    }

    #[test]
    fn empty_string_value_is_a_valid_string() {
        assert_eq!(CrdtValue::parse("S:"), Ok(CrdtValue::Str(String::new())));
    }

    #[test]
    fn non_numeric_number_is_rejected() {
        let reason = CrdtValue::parse("N:notanumber").unwrap_err();
        assert!(reason.contains("N: prefix"), "got {reason:?}");
    }

    /// Short inputs must not panic — this is why `parse` uses `strip_prefix`
    /// rather than slicing `&raw[2..]`.
    #[test]
    fn short_inputs_do_not_panic() {
        assert!(CrdtValue::parse("").is_err());
        assert!(CrdtValue::parse("S").is_err());
        assert!(CrdtValue::parse("0").is_err());
    }

    /// Actual's JS reader switches on the first character only, so it would
    /// read "Sfoo" as "oo". We require the colon; corruption should not decode.
    #[test]
    fn missing_colon_is_rejected() {
        assert!(CrdtValue::parse("Sfoo").is_err());
    }
}
