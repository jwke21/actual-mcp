use std::collections::{HashMap, HashSet};

use rusqlite::{Connection, Transaction, types::Value};

use crate::actual::messages::{CrdtValue, SyncMessage};
use crate::store::error::StoreError;

/// Datasets that carry no table behind them. Actual's own `apply` has the same
/// guard: "Do nothing, it doesn't exist in the db".
const PSEUDO_DATASETS: &[&str] = &["prefs"];

/// The replica's table and column names, read once per batch.
///
/// `dataset` and `column` arrive from the server and are interpolated into SQL
/// as *identifiers* — `?` placeholders only bind values, never identifiers — so
/// both must be checked against the real schema before use.
pub(crate) struct Schema {
    tables: HashMap<String, HashSet<String>>,
}

impl Schema {
    pub(crate) fn load(conn: &Connection) -> Result<Self, StoreError> {
        let names: Vec<String> = {
            let mut stmt = conn.prepare("SELECT name FROM sqlite_master WHERE type = 'table'")?;
            let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
            rows.collect::<Result<_, _>>()?
        };

        let mut tables = HashMap::with_capacity(names.len());
        for name in names {
            // `name` came from sqlite_master, so it is a real identifier.
            let mut stmt = conn.prepare(&format!(r#"PRAGMA table_info("{name}")"#))?;
            let cols = stmt.query_map([], |r| r.get::<_, String>(1))?;
            tables.insert(name, cols.collect::<Result<HashSet<_>, _>>()?);
        }

        Ok(Self { tables })
    }

    fn columns_of(&self, msg: &SyncMessage) -> Result<&HashSet<String>, StoreError> {
        self.tables
            .get(&msg.dataset)
            .ok_or_else(|| StoreError::UnknownTable {
                timestamp: msg.timestamp.clone(),
                dataset: msg.dataset.clone(),
            })
    }
}

/// Apply a batch of CRDT messages as upserts.
///
/// Caller supplies the transaction: the clock must advance in the same one, so
/// a partial apply can never be committed with a clock that claims it finished.
pub(crate) fn apply_messages(
    tx: &Transaction<'_>,
    messages: &[SyncMessage],
) -> Result<usize, StoreError> {
    let schema = Schema::load(tx)?;

    // Last write wins falls out of applying in timestamp order. The server
    // already sorts, but a batch must not depend on that.
    let mut ordered: Vec<&SyncMessage> = messages.iter().collect();
    ordered.sort_by(|a, b| a.timestamp.cmp(&b.timestamp));

    let mut applied = 0usize;
    for msg in ordered {
        if PSEUDO_DATASETS.contains(&msg.dataset.as_str()) {
            continue;
        }

        let columns = schema.columns_of(msg)?;
        if !columns.contains(&msg.column) {
            return Err(StoreError::UnknownColumn {
                timestamp: msg.timestamp.clone(),
                dataset: msg.dataset.clone(),
                column: msg.column.clone(),
            });
        }
        if msg.column == "id" {
            return Err(StoreError::RewritesPrimaryKey {
                timestamp: msg.timestamp.clone(),
                dataset: msg.dataset.clone(),
            });
        }

        // Every table that receives messages has `id` as its sole primary key,
        // so one upsert covers both the insert and the update case.
        let sql = format!(
            r#"INSERT INTO "{table}" (id, "{column}") VALUES (?1, ?2)
               ON CONFLICT(id) DO UPDATE SET "{column}" = ?2"#,
            table = msg.dataset,
            column = msg.column,
        );

        tx.execute(&sql, rusqlite::params![&msg.row, to_sql(msg)?])?;
        applied += 1;
    }

    Ok(applied)
}

/// CRDT values are JSON numbers, but `amount` and friends are INTEGER columns
/// holding cents. Bind whole numbers as integers rather than leaning on
/// SQLite's type affinity to round-trip a REAL correctly.
fn to_sql(msg: &SyncMessage) -> Result<Value, StoreError> {
    Ok(match &msg.value {
        CrdtValue::Str(s) => Value::Text(s.clone()),
        CrdtValue::Null => Value::Null,
        CrdtValue::Num(n) => {
            if !n.is_finite() {
                return Err(StoreError::UnrepresentableNumber {
                    timestamp: msg.timestamp.clone(),
                    value: *n,
                });
            }
            let integral = n.fract() == 0.0;
            let in_range = *n >= i64::MIN as f64 && *n <= i64::MAX as f64;
            if integral && in_range {
                Value::Integer(*n as i64)
            } else {
                Value::Real(*n)
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const T1: &str = "2026-09-06T20:00:00.000Z-0001-aaaaaaaaaaaaaaaa";
    const T2: &str = "2026-09-06T20:00:00.000Z-0002-aaaaaaaaaaaaaaaa";
    const T3: &str = "2026-09-06T20:00:00.000Z-0003-aaaaaaaaaaaaaaaa";

    fn db() -> Connection {
        let c = Connection::open_in_memory().expect("in-memory db");
        c.execute_batch(
            "CREATE TABLE transactions (
                 id TEXT PRIMARY KEY,
                 amount INTEGER,
                 notes TEXT,
                 tombstone INTEGER DEFAULT 0
             );
             CREATE TABLE accounts (id TEXT PRIMARY KEY, name TEXT);",
        )
        .expect("schema");
        c
    }

    fn msg(ts: &str, dataset: &str, row: &str, column: &str, value: CrdtValue) -> SyncMessage {
        SyncMessage {
            timestamp: ts.to_string(),
            dataset: dataset.to_string(),
            row: row.to_string(),
            column: column.to_string(),
            value,
        }
    }

    /// Apply and commit, returning how many were applied.
    fn run(conn: &mut Connection, messages: &[SyncMessage]) -> Result<usize, StoreError> {
        let tx = conn.transaction()?;
        let n = apply_messages(&tx, messages)?;
        tx.commit()?;
        Ok(n)
    }

    #[test]
    fn inserts_a_missing_row() {
        let mut c = db();
        run(
            &mut c,
            &[msg(
                T1,
                "transactions",
                "t1",
                "amount",
                CrdtValue::Num(-94300.0),
            )],
        )
        .unwrap();

        let amount: i64 = c
            .query_row("SELECT amount FROM transactions WHERE id = 't1'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(amount, -94300);
    }

    #[test]
    fn updates_an_existing_row() {
        let mut c = db();
        c.execute(
            "INSERT INTO transactions (id, amount) VALUES ('t1', 100)",
            [],
        )
        .unwrap();

        run(
            &mut c,
            &[msg(
                T1,
                "transactions",
                "t1",
                "amount",
                CrdtValue::Num(-94300.0),
            )],
        )
        .unwrap();

        let amount: i64 = c
            .query_row("SELECT amount FROM transactions WHERE id = 't1'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(amount, -94300, "upsert should update, not duplicate");

        let rows: i64 = c
            .query_row("SELECT count(*) FROM transactions", [], |r| r.get(0))
            .unwrap();
        assert_eq!(rows, 1);
    }

    /// Money is cents in an INTEGER column. A whole number must not land as REAL.
    #[test]
    fn whole_numbers_are_stored_as_integers() {
        let mut c = db();
        run(
            &mut c,
            &[msg(
                T1,
                "transactions",
                "t1",
                "amount",
                CrdtValue::Num(-94300.0),
            )],
        )
        .unwrap();

        let kind: String = c
            .query_row(
                "SELECT typeof(amount) FROM transactions WHERE id = 't1'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(kind, "integer");
    }

    #[test]
    fn null_values_are_stored_as_null() {
        let mut c = db();
        run(
            &mut c,
            &[msg(T1, "transactions", "t1", "notes", CrdtValue::Null)],
        )
        .unwrap();

        let kind: String = c
            .query_row(
                "SELECT typeof(notes) FROM transactions WHERE id = 't1'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(kind, "null");
    }

    /// Last write wins comes from ordering, so out-of-order input must be sorted.
    #[test]
    fn applies_in_timestamp_order() {
        let mut c = db();
        run(
            &mut c,
            &[
                msg(
                    T3,
                    "transactions",
                    "t1",
                    "notes",
                    CrdtValue::Str("last".into()),
                ),
                msg(
                    T1,
                    "transactions",
                    "t1",
                    "notes",
                    CrdtValue::Str("first".into()),
                ),
                msg(
                    T2,
                    "transactions",
                    "t1",
                    "notes",
                    CrdtValue::Str("middle".into()),
                ),
            ],
        )
        .unwrap();

        let notes: String = c
            .query_row("SELECT notes FROM transactions WHERE id = 't1'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(notes, "last");
    }

    /// Actual's own apply has this guard: `prefs` has no table behind it.
    #[test]
    fn prefs_dataset_is_skipped() {
        let mut c = db();
        let applied = run(
            &mut c,
            &[msg(
                T1,
                "prefs",
                "theme",
                "value",
                CrdtValue::Str("dark".into()),
            )],
        )
        .expect("prefs must not be an error");
        assert_eq!(applied, 0);
    }

    #[test]
    fn unknown_table_is_rejected() {
        let mut c = db();
        let err = run(&mut c, &[msg(T1, "not_a_table", "x", "y", CrdtValue::Null)]).unwrap_err();
        assert!(
            matches!(err, StoreError::UnknownTable { .. }),
            "got {err:?}"
        );
    }

    #[test]
    fn unknown_column_is_rejected() {
        let mut c = db();
        let err = run(
            &mut c,
            &[msg(T1, "transactions", "t1", "nope", CrdtValue::Null)],
        )
        .unwrap_err();
        assert!(
            matches!(err, StoreError::UnknownColumn { .. }),
            "got {err:?}"
        );
    }

    /// Identifiers cannot be bound as parameters, so a hostile or drifted
    /// column name must be rejected rather than formatted into SQL.
    #[test]
    fn sql_injection_in_column_is_rejected() {
        let mut c = db();
        let err = run(
            &mut c,
            &[msg(
                T1,
                "transactions",
                "t1",
                r#"amount"; DROP TABLE accounts; --"#,
                CrdtValue::Null,
            )],
        )
        .unwrap_err();
        assert!(
            matches!(err, StoreError::UnknownColumn { .. }),
            "got {err:?}"
        );

        let still_there: i64 = c
            .query_row(
                "SELECT count(*) FROM sqlite_master WHERE name = 'accounts'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(still_there, 1, "accounts table should be untouched");
    }

    #[test]
    fn rewriting_the_primary_key_is_rejected() {
        let mut c = db();
        let err = run(
            &mut c,
            &[msg(
                T1,
                "transactions",
                "t1",
                "id",
                CrdtValue::Str("t2".into()),
            )],
        )
        .unwrap_err();
        assert!(
            matches!(err, StoreError::RewritesPrimaryKey { .. }),
            "got {err:?}"
        );
    }

    /// A failure anywhere must leave the database exactly as it was.
    #[test]
    fn a_bad_message_rolls_back_the_whole_batch() {
        let mut c = db();
        let result = run(
            &mut c,
            &[
                msg(T1, "transactions", "t1", "amount", CrdtValue::Num(1.0)),
                msg(T2, "transactions", "t2", "nope", CrdtValue::Null),
            ],
        );
        assert!(result.is_err());

        let rows: i64 = c
            .query_row("SELECT count(*) FROM transactions", [], |r| r.get(0))
            .unwrap();
        assert_eq!(rows, 0, "the good message must not survive a failed batch");
    }
}
