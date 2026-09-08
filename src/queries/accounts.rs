//! Accounts and their balances.

use rusqlite::Connection;
use schemars::JsonSchema;
use serde::Serialize;

use crate::domain::Money;

#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
pub struct AccountSummary {
    pub name: String,
    /// Everything that has ever happened in this account, summed. Negative for
    /// a credit card carrying a balance.
    pub balance: Money,
    /// On-budget accounts fund the envelope budget. Off-budget ones
    /// (retirement, brokerage) are tracked for net worth but never budgeted.
    pub on_budget: bool,
    pub closed: bool,
    /// Actual's account type, when the file records one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub account_type: Option<String>,
}

/// List accounts with their balances.
///
/// The balance is deliberately **not** filtered through [`TxScope`]: that scope
/// answers "what did the user spend", which excludes transfers and opening
/// balances. An account balance is the opposite question — every movement of
/// money in that account counts, however it got there. Split parents are still
/// skipped, because they duplicate the amounts of their children.
///
/// It also ignores the `balance_current` column, which is whatever the bank
/// last reported during a sync rather than what the budget believes.
///
/// [`TxScope`]: crate::queries::scope::TxScope
pub fn list(
    conn: &Connection,
    include_closed: bool,
) -> Result<Vec<AccountSummary>, rusqlite::Error> {
    let mut stmt = conn.prepare(
        "SELECT a.name,
                COALESCE((SELECT SUM(t.amount) FROM v_transactions t
                           WHERE t.account = a.id AND t.is_parent = 0), 0) AS balance,
                a.offbudget,
                a.closed,
                NULLIF(COALESCE(a.type, ''), '')
         FROM accounts a
         WHERE a.tombstone = 0 AND (?1 OR a.closed = 0)
         ORDER BY a.offbudget, a.sort_order, a.name",
    )?;

    let rows = stmt.query_map([include_closed], |r| {
        Ok(AccountSummary {
            name: r.get(0)?,
            balance: Money::from_cents(r.get(1)?),
            on_budget: r.get::<_, i64>(2)? == 0,
            closed: r.get::<_, i64>(3)? != 0,
            account_type: r.get(4)?,
        })
    })?;

    rows.collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "CREATE TABLE accounts (id TEXT PRIMARY KEY, name TEXT, offbudget INTEGER DEFAULT 0,
                                    closed INTEGER DEFAULT 0, tombstone INTEGER DEFAULT 0,
                                    sort_order REAL, type TEXT, balance_current INTEGER);
             CREATE TABLE transactions (id TEXT PRIMARY KEY, acct TEXT, amount INTEGER,
                                        is_parent INTEGER DEFAULT 0, transfer_id TEXT,
                                        starting_balance_flag INTEGER DEFAULT 0,
                                        tombstone INTEGER DEFAULT 0);
             CREATE VIEW v_transactions AS
               SELECT _.id, _.acct AS account, _.amount, _.is_parent, _.transfer_id,
                      _.starting_balance_flag
               FROM transactions _ WHERE _.tombstone = 0;",
        )
        .unwrap();
        c
    }

    fn account(c: &Connection, id: &str, name: &str, offbudget: bool, closed: bool) {
        c.execute(
            "INSERT INTO accounts (id, name, offbudget, closed, sort_order, balance_current)
             VALUES (?1, ?2, ?3, ?4, 1.0, 999999)",
            rusqlite::params![id, name, offbudget as i64, closed as i64],
        )
        .unwrap();
    }

    fn txn(c: &Connection, id: &str, acct: &str, cents: i64) {
        c.execute(
            "INSERT INTO transactions (id, acct, amount) VALUES (?1,?2,?3)",
            rusqlite::params![id, acct, cents],
        )
        .unwrap();
    }

    #[test]
    fn balance_is_the_sum_of_transactions_not_the_bank_figure() {
        let c = db();
        account(&c, "a1", "Checking", false, false);
        txn(&c, "t1", "a1", 100_000);
        txn(&c, "t2", "a1", -25_000);

        let rows = list(&c, false).unwrap();
        assert_eq!(rows[0].balance, Money::from_cents(75_000));
        assert!(rows[0].on_budget);
    }

    /// Transfers and opening balances are part of an account's balance even
    /// though they are not spending. This is the distinction from `TxScope`.
    #[test]
    fn transfers_and_opening_balances_count_towards_the_balance() {
        let c = db();
        account(&c, "a1", "Checking", false, false);
        c.execute(
            "INSERT INTO transactions (id, acct, amount, starting_balance_flag)
             VALUES ('t1','a1',50_000,1)",
            [],
        )
        .unwrap();
        c.execute(
            "INSERT INTO transactions (id, acct, amount, transfer_id)
             VALUES ('t2','a1',-20_000,'other')",
            [],
        )
        .unwrap();

        assert_eq!(
            list(&c, false).unwrap()[0].balance,
            Money::from_cents(30_000)
        );
    }

    #[test]
    fn split_parents_do_not_double_count() {
        let c = db();
        account(&c, "a1", "Checking", false, false);
        c.execute(
            "INSERT INTO transactions (id, acct, amount, is_parent) VALUES ('p','a1',-1000,1)",
            [],
        )
        .unwrap();
        txn(&c, "c1", "a1", -600);
        txn(&c, "c2", "a1", -400);

        assert_eq!(
            list(&c, false).unwrap()[0].balance,
            Money::from_cents(-1000)
        );
    }

    #[test]
    fn an_account_with_no_transactions_reads_zero() {
        let c = db();
        account(&c, "a1", "New Account", false, false);
        assert_eq!(list(&c, false).unwrap()[0].balance, Money::ZERO);
    }

    #[test]
    fn closed_accounts_are_opt_in() {
        let c = db();
        account(&c, "a1", "Open", false, false);
        account(&c, "a2", "Closed", false, true);

        assert_eq!(list(&c, false).unwrap().len(), 1);
        let all = list(&c, true).unwrap();
        assert_eq!(all.len(), 2);
        assert!(all.iter().any(|a| a.closed));
    }

    #[test]
    fn on_budget_accounts_are_listed_first() {
        let c = db();
        account(&c, "a1", "Retirement", true, false);
        account(&c, "a2", "Checking", false, false);

        let rows = list(&c, false).unwrap();
        assert!(rows[0].on_budget, "on-budget accounts lead the list");
        assert!(!rows[1].on_budget);
    }
}
