use std::{
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use rusqlite::Connection;

use crate::actual::{
    messages::{SyncMessage, ZERO_CLOCK},
    model::Snapshot,
};
use crate::store::{apply::apply_messages, error::StoreError, freshness::Freshness};

const CLOCK_KEY: &str = "clock";
const LAST_RETRIEVAL_KEY: &str = "last_retrieval";

/// A local, materialised copy of the budget.
///
/// This is **cache, not state**: everything in it is derived from the Actual
/// server and can be rebuilt by deleting the file (FR-2.8). Nothing
/// user-meaningful lives only here.
pub struct Replica {
    conn: Arc<Mutex<Connection>>,
    path: PathBuf,
}

impl Replica {
    fn file_name(group_id: &str) -> String {
        format!("replica-{group_id}.sqlite")
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Write a freshly downloaded snapshot and open it.
    ///
    /// Separate from [`Replica::open`] because installing *replaces the file*,
    /// which cannot be done underneath a live connection.
    pub fn install(
        cache_dir: &Path,
        group_id: &str,
        snapshot: &Snapshot,
    ) -> Result<Self, StoreError> {
        std::fs::create_dir_all(cache_dir).map_err(|source| StoreError::CacheDir {
            path: cache_dir.to_path_buf(),
            source,
        })?;

        let path = cache_dir.join(Self::file_name(group_id));
        std::fs::write(&path, &snapshot.db_bytes).map_err(|source| StoreError::Write {
            path: path.clone(),
            source,
        })?;

        let conn = Connection::open(&path)?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS mcp_state (key TEXT PRIMARY KEY, value TEXT NOT NULL)",
        )?;

        // A reset-clock upload carries no timestamp; epoch zero asks for everything.
        let clock = snapshot
            .metadata
            .last_synced_timestamp
            .as_deref()
            .unwrap_or(ZERO_CLOCK);
        set_state(&conn, CLOCK_KEY, clock)?;

        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
            path,
        })
    }

    /// Open an existing replica, or `None` if there isn't a usable one.
    pub fn open(cache_dir: &Path, group_id: &str) -> Result<Option<Self>, StoreError> {
        let path = cache_dir.join(Self::file_name(group_id));
        if !path.exists() {
            return Ok(None);
        }

        let conn = Connection::open(&path)?;

        // A file without our state table was not installed by us, or was
        // installed by a version that predates it. Treat it as absent and let
        // the caller re-install rather than guessing at its contents.
        let initialised: i64 = conn.query_row(
            "SELECT count(*) FROM sqlite_master WHERE type = 'table' AND name = 'mcp_state'",
            [],
            |r| r.get(0),
        )?;
        if initialised == 0 {
            return Ok(None);
        }

        Ok(Some(Self {
            conn: Arc::new(Mutex::new(conn)),
            path,
        }))
    }

    /// The HLC to pass as `since` on the next fetch.
    pub fn clock(&self) -> Result<String, StoreError> {
        let conn = self.conn.lock().expect("replica mutex poisoned");
        get_state(&conn, CLOCK_KEY)?.ok_or(StoreError::NoClock)
    }

    /// Apply a batch and advance the clock **in one transaction**.
    ///
    /// If any message fails, the whole batch rolls back and the clock stays
    /// where it was — a partial apply with an advanced clock would lose those
    /// edits permanently, since the server would never send them again.
    pub fn apply(
        &self,
        messages: &[SyncMessage],
        new_clock: &str,
        now: SystemTime,
    ) -> Result<usize, StoreError> {
        let mut conn = self.conn.lock().expect("replica mutex poisoned");
        let tx = conn.transaction()?;

        let applied = apply_messages(&tx, messages)?;
        set_state(&tx, CLOCK_KEY, new_clock)?;
        set_state(&tx, LAST_RETRIEVAL_KEY, &epoch_secs(now).to_string())?;

        tx.commit()?;
        Ok(applied)
    }

    pub fn freshness(&self) -> Result<Freshness, StoreError> {
        let conn = self.conn.lock().expect("replica mutex poisoned");

        let last_retrieval = get_state(&conn, LAST_RETRIEVAL_KEY)?
            .and_then(|raw| raw.parse::<u64>().ok())
            .map(|secs| UNIX_EPOCH + Duration::from_secs(secs));

        let newest_transaction: Option<u32> = conn.query_row(
            "SELECT max(date) FROM transactions WHERE tombstone = 0",
            [],
            |r| r.get(0),
        )?;

        Ok(Freshness {
            last_retrieval,
            newest_transaction,
        })
    }

    /// Run a read on the blocking pool.
    ///
    /// The mutex guard never crosses an `.await`: it is taken and released
    /// inside the closure that `spawn_blocking` owns.
    pub async fn read<T, F>(&self, f: F) -> Result<T, StoreError>
    where
        F: FnOnce(&Connection) -> Result<T, StoreError> + Send + 'static,
        T: Send + 'static,
    {
        let conn = Arc::clone(&self.conn);
        tokio::task::spawn_blocking(move || {
            let guard = conn.lock().expect("replica mutex poisoned");
            f(&guard)
        })
        .await?
    }
}

fn set_state(conn: &Connection, key: &str, value: &str) -> Result<(), StoreError> {
    conn.execute(
        "INSERT INTO mcp_state (key, value) VALUES (?1, ?2)
         ON CONFLICT(key) DO UPDATE SET value = ?2",
        (key, value),
    )?;
    Ok(())
}

fn get_state(conn: &Connection, key: &str) -> Result<Option<String>, StoreError> {
    conn.query_row("SELECT value FROM mcp_state WHERE key = ?1", [key], |r| {
        r.get(0)
    })
    .map(Some)
    .or_else(|e| match e {
        rusqlite::Error::QueryReturnedNoRows => Ok(None),
        other => Err(StoreError::from(other)),
    })
}

fn epoch_secs(t: SystemTime) -> u64 {
    t.duration_since(UNIX_EPOCH).unwrap_or_default().as_secs()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::actual::{messages::CrdtValue, model::Metadata};

    const T1: &str = "2026-09-06T20:00:00.000Z-0001-aaaaaaaaaaaaaaaa";
    const CLOCK_A: &str = "2026-09-06T21:00:00.000Z-0000-aaaaaaaaaaaaaaaa";
    const CLOCK_B: &str = "2026-09-06T22:00:00.000Z-0000-aaaaaaaaaaaaaaaa";

    /// A minimal but real SQLite file, the way a snapshot arrives.
    fn db_bytes() -> Vec<u8> {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("seed.sqlite");
        let conn = Connection::open(&path).expect("open seed");
        conn.execute_batch(
            "CREATE TABLE transactions (
                 id TEXT PRIMARY KEY,
                 amount INTEGER,
                 date INTEGER,
                 tombstone INTEGER DEFAULT 0
             );
             INSERT INTO transactions (id, amount, date) VALUES ('t0', 100, 20260101);",
        )
        .expect("seed schema");
        drop(conn);
        std::fs::read(&path).expect("read seed")
    }

    fn snapshot(clock: Option<&str>) -> Snapshot {
        Snapshot {
            db_bytes: db_bytes(),
            metadata: Metadata {
                id: "Test-Budget-0000000".to_string(),
                budget_name: "Test Budget".to_string(),
                cloud_file_id: "11111111-1111-1111-1111-111111111111".to_string(),
                group_id: None,
                last_synced_timestamp: clock.map(String::from),
                reset_clock: true,
            },
        }
    }

    fn msg(ts: &str, column: &str, value: CrdtValue) -> SyncMessage {
        SyncMessage {
            timestamp: ts.to_string(),
            dataset: "transactions".to_string(),
            row: "t0".to_string(),
            column: column.to_string(),
            value,
        }
    }

    fn now() -> SystemTime {
        UNIX_EPOCH + Duration::from_secs(1_800_000_000)
    }

    #[test]
    fn install_without_a_snapshot_clock_starts_at_epoch_zero() {
        let dir = tempfile::tempdir().unwrap();
        let replica = Replica::install(dir.path(), "g-1", &snapshot(None)).unwrap();
        assert_eq!(replica.clock().unwrap(), ZERO_CLOCK);
    }

    #[test]
    fn install_uses_the_snapshot_clock_when_present() {
        let dir = tempfile::tempdir().unwrap();
        let replica = Replica::install(dir.path(), "g-1", &snapshot(Some(CLOCK_A))).unwrap();
        assert_eq!(replica.clock().unwrap(), CLOCK_A);
    }

    #[test]
    fn open_returns_none_when_there_is_no_replica() {
        let dir = tempfile::tempdir().unwrap();
        assert!(Replica::open(dir.path(), "g-1").unwrap().is_none());
    }

    #[test]
    fn open_finds_an_installed_replica_and_its_clock() {
        let dir = tempfile::tempdir().unwrap();
        Replica::install(dir.path(), "g-1", &snapshot(Some(CLOCK_A))).unwrap();

        let reopened = Replica::open(dir.path(), "g-1")
            .unwrap()
            .expect("should exist");
        assert_eq!(reopened.clock().unwrap(), CLOCK_A);
    }

    /// A bare SQLite file we did not install has no `mcp_state`, so we cannot
    /// know its clock. Report it absent so the caller re-installs.
    #[test]
    fn open_ignores_a_file_without_our_state_table() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(Replica::file_name("g-1")), db_bytes()).unwrap();
        assert!(Replica::open(dir.path(), "g-1").unwrap().is_none());
    }

    #[test]
    fn apply_advances_the_clock_and_the_data() {
        let dir = tempfile::tempdir().unwrap();
        let replica = Replica::install(dir.path(), "g-1", &snapshot(Some(CLOCK_A))).unwrap();

        let applied = replica
            .apply(
                &[msg(T1, "amount", CrdtValue::Num(-94300.0))],
                CLOCK_B,
                now(),
            )
            .unwrap();

        assert_eq!(applied, 1);
        assert_eq!(replica.clock().unwrap(), CLOCK_B);

        let conn = replica.conn.lock().unwrap();
        let amount: i64 = conn
            .query_row("SELECT amount FROM transactions WHERE id = 't0'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(amount, -94300);
    }

    /// The invariant that makes a failed refresh recoverable: if the batch
    /// rolls back, the clock must not move. Otherwise the server would never
    /// send those messages again and the edits would be lost for good.
    #[test]
    fn a_failed_apply_leaves_the_clock_untouched() {
        let dir = tempfile::tempdir().unwrap();
        let replica = Replica::install(dir.path(), "g-1", &snapshot(Some(CLOCK_A))).unwrap();

        let result = replica.apply(
            &[
                msg(T1, "amount", CrdtValue::Num(-94300.0)),
                msg(T1, "no_such_column", CrdtValue::Null),
            ],
            CLOCK_B,
            now(),
        );

        assert!(result.is_err());
        assert_eq!(replica.clock().unwrap(), CLOCK_A, "clock must not advance");

        let conn = replica.conn.lock().unwrap();
        let amount: i64 = conn
            .query_row("SELECT amount FROM transactions WHERE id = 't0'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(amount, 100, "data must be unchanged too");
    }

    #[test]
    fn freshness_reports_the_newest_transaction_and_last_retrieval() {
        let dir = tempfile::tempdir().unwrap();
        let replica = Replica::install(dir.path(), "g-1", &snapshot(Some(CLOCK_A))).unwrap();

        // Before any apply there has been no retrieval.
        let before = replica.freshness().unwrap();
        assert_eq!(before.last_retrieval, None);
        assert_eq!(before.newest_transaction, Some(20260101));

        replica
            .apply(
                &[msg(T1, "date", CrdtValue::Num(20260903.0))],
                CLOCK_B,
                now(),
            )
            .unwrap();

        let after = replica.freshness().unwrap();
        assert_eq!(after.last_retrieval, Some(now()));
        assert_eq!(after.newest_transaction, Some(20260903));
    }
}
