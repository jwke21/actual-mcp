use crate::actual::{
    error::ActualError,
    model::{Metadata, Snapshot},
};
use std::io::{Cursor, Read};

pub fn parse(bytes: &[u8]) -> Result<Snapshot, ActualError> {
    let mut archive =
        zip::ZipArchive::new(Cursor::new(bytes)).map_err(|e| ActualError::BadSnapshot {
            reason: format!("not a valid zip: {e}"),
        })?;

    // by_name borrows the archive mutably and reads each entry fully before the next
    let mut db_bytes = Vec::new();
    archive
        .by_name("db.sqlite")
        .map_err(|e| ActualError::BadSnapshot {
            reason: format!("db.sqlite: {e}"),
        })?
        .read_to_end(&mut db_bytes)
        .map_err(|e| ActualError::BadSnapshot {
            reason: format!("reading db.sqlite: {e}"),
        })?;

    let mut raw = String::new();
    archive
        .by_name("metadata.json")
        .map_err(|e| ActualError::BadSnapshot {
            reason: format!("metadata.json: {e}"),
        })?
        .read_to_string(&mut raw)
        .map_err(|e| ActualError::BadSnapshot {
            reason: format!("reading metadata.json: {e}"),
        })?;

    let metadata: Metadata = serde_json::from_str(&raw).map_err(|e| ActualError::BadSnapshot {
        reason: format!("metadata.json: {e}"),
    })?;

    if !db_bytes.starts_with(b"SQLite format 3\0") {
        return Err(ActualError::BadSnapshot {
            reason: "db.sqlite is not a SQLite database".into(),
        });
    }

    Ok(Snapshot { db_bytes, metadata })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    const SQLITE_HEADER: &[u8] = b"SQLite format 3\0";

    /// Matches the shape of the real metadata.json, including the nulls that
    /// a reset-clock upload produces.
    const METADATA: &str = r#"{
        "id": "Test-Budget-0000000",
        "budgetName": "Test Budget",
        "cloudFileId": "11111111-1111-1111-1111-111111111111",
        "groupId": null,
        "lastSyncedTimestamp": null,
        "resetClock": true
    }"#;

    fn make_zip(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut buf = Vec::new();
        {
            let mut w = zip::ZipWriter::new(Cursor::new(&mut buf));
            // Stored keeps this independent of which compression features are on
            let opts = zip::write::SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Stored);
            for (name, data) in entries {
                w.start_file(*name, opts).expect("start_file");
                w.write_all(data).expect("write_all");
            }
            w.finish().expect("finish");
        }
        buf
    }

    fn fake_db() -> Vec<u8> {
        let mut v = SQLITE_HEADER.to_vec();
        v.extend_from_slice(b"the rest of a database");
        v
    }

    /// Unwrap a BadSnapshot reason so tests can assert *which* failure it was,
    /// not merely that something failed.
    fn reason(err: ActualError) -> String {
        match err {
            ActualError::BadSnapshot { reason } => reason,
            other => panic!("expected BadSnapshot, got {other:?}"),
        }
    }

    #[test]
    fn parses_a_valid_snapshot() {
        let db = fake_db();
        let zip = make_zip(&[("db.sqlite", &db), ("metadata.json", METADATA.as_bytes())]);

        let snap = parse(&zip).expect("should parse");

        assert_eq!(snap.metadata.budget_name, "Test Budget");
        assert_eq!(
            snap.metadata.cloud_file_id,
            "11111111-1111-1111-1111-111111111111"
        );
        assert!(snap.metadata.group_id.is_none());
        assert!(snap.metadata.last_synced_timestamp.is_none());
        assert!(snap.metadata.reset_clock);
        assert_eq!(snap.db_bytes, db);
    }

    /// Zip entry order is not guaranteed, so lookups must be by name.
    #[test]
    fn entry_order_does_not_matter() {
        let db = fake_db();
        let zip = make_zip(&[("metadata.json", METADATA.as_bytes()), ("db.sqlite", &db)]);
        assert!(parse(&zip).is_ok());
    }

    #[test]
    fn not_a_zip_is_rejected() {
        let err = parse(b"definitely not a zip").unwrap_err();
        assert!(reason(err).contains("not a valid zip"));
    }

    #[test]
    fn missing_db_is_rejected() {
        let zip = make_zip(&[("metadata.json", METADATA.as_bytes())]);
        let err = parse(&zip).unwrap_err();
        assert!(reason(err).contains("db.sqlite"));
    }

    #[test]
    fn missing_metadata_is_rejected() {
        let db = fake_db();
        let zip = make_zip(&[("db.sqlite", &db)]);
        let err = parse(&zip).unwrap_err();
        assert!(reason(err).contains("metadata.json"));
    }

    #[test]
    fn malformed_metadata_is_rejected() {
        let db = fake_db();
        let zip = make_zip(&[("db.sqlite", &db), ("metadata.json", b"{ not json")]);
        let err = parse(&zip).unwrap_err();
        assert!(reason(err).contains("metadata.json"));
    }

    /// Guards the magic-header check: a truncated or misrouted download must
    /// fail here rather than as a confusing rusqlite error much later.
    #[test]
    fn non_sqlite_db_is_rejected() {
        let zip = make_zip(&[
            ("db.sqlite", b"PK\x03\x04 not a database"),
            ("metadata.json", METADATA.as_bytes()),
        ]);
        let err = parse(&zip).unwrap_err();
        assert!(reason(err).contains("not a SQLite database"));
    }
}
