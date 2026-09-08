//! Queries over transactions. Every one of them goes through [`TxScope`].

use rusqlite::{Connection, types::Value};
use schemars::JsonSchema;
use serde::Serialize;

use crate::domain::{BudgetDate, Money};
use crate::queries::scope::{FROM, TxScope};

/// What a transaction with no category is called in output. Transactions whose
/// category was deleted land here too, since the view nulls those out.
pub const UNCATEGORISED: &str = "Uncategorised";

#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
pub struct CategoryTotal {
    pub category: String,
    /// Income categories carry positive totals; spending categories negative.
    pub is_income: bool,
    pub total: Money,
    pub transaction_count: usize,
}

/// Totals per category over an inclusive date range.
///
/// Uncategorised transactions are returned as their own bucket rather than
/// dropped, so the rows always sum to the overall total (FR-4.4).
pub fn spending_by_category(
    conn: &Connection,
    from: BudgetDate,
    to: BudgetDate,
    scope: TxScope,
) -> Result<Vec<CategoryTotal>, rusqlite::Error> {
    let sql = format!(
        "SELECT COALESCE(cat.name, '{UNCATEGORISED}') AS category,
                COALESCE(cat.is_income, 0)            AS is_income,
                SUM(t.amount)                         AS total,
                COUNT(*)                              AS n
         FROM {FROM}
         LEFT JOIN categories cat ON cat.id = t.category
         WHERE {scope} AND t.date BETWEEN ?1 AND ?2
         GROUP BY cat.id
         ORDER BY total ASC",
        scope = scope.where_sql(),
    );

    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map([from.as_int(), to.as_int()], |r| {
        Ok(CategoryTotal {
            category: r.get(0)?,
            is_income: r.get::<_, i64>(1)? != 0,
            total: Money::from_cents(r.get(2)?),
            transaction_count: r.get::<_, i64>(3)? as usize,
        })
    })?;

    rows.collect()
}

/// Default page size, and the ceiling a caller may raise it to (FR-5.5).
pub const DEFAULT_LIMIT: usize = 100;
pub const MAX_LIMIT: usize = 1000;

#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
pub struct TransactionRow {
    pub date: BudgetDate,
    /// Who the money went to or came from. For a transfer this is the account
    /// on the other side.
    pub payee: Option<String>,
    /// Absent when the transaction has no category.
    pub category: Option<String>,
    pub account: String,
    pub amount: Money,
    pub notes: Option<String>,
    /// Cleared transactions have been confirmed against the bank.
    pub cleared: bool,
    /// True when this moves money between the user's own accounts rather than
    /// in or out of the budget.
    pub is_transfer: bool,
}

/// Filters, all optional and combined with AND. Ids, not names: resolving a
/// name is the caller's job, because an ambiguous name needs an answer the
/// user can act on rather than a silently-picked row.
#[derive(Debug, Clone, Default)]
pub struct TxFilters {
    pub from: Option<BudgetDate>,
    pub to: Option<BudgetDate>,
    pub account_id: Option<String>,
    pub category_id: Option<String>,
    pub payee_id: Option<String>,
    pub min_amount: Option<Money>,
    pub max_amount: Option<Money>,
    pub notes_contains: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TransactionPage {
    pub rows: Vec<TransactionRow>,
    /// How many matched in total, so a truncated page can say so.
    pub total_matching: usize,
}

/// Newest first. `limit` is clamped to [`MAX_LIMIT`].
pub fn list(
    conn: &Connection,
    filters: &TxFilters,
    scope: TxScope,
    limit: usize,
) -> Result<TransactionPage, rusqlite::Error> {
    let mut predicates = vec![scope.where_sql()];
    let mut params: Vec<Value> = Vec::new();

    let mut push = |sql: &str, value: Value| {
        predicates.push(sql.to_string());
        params.push(value);
    };

    if let Some(from) = filters.from {
        push("t.date >= ?", Value::Integer(from.as_int().into()));
    }
    if let Some(to) = filters.to {
        push("t.date <= ?", Value::Integer(to.as_int().into()));
    }
    if let Some(id) = &filters.account_id {
        push("t.account = ?", Value::Text(id.clone()));
    }
    if let Some(id) = &filters.category_id {
        push("t.category = ?", Value::Text(id.clone()));
    }
    if let Some(id) = &filters.payee_id {
        push("t.payee = ?", Value::Text(id.clone()));
    }
    if let Some(min) = filters.min_amount {
        push("t.amount >= ?", Value::Integer(min.cents()));
    }
    if let Some(max) = filters.max_amount {
        push("t.amount <= ?", Value::Integer(max.cents()));
    }
    if let Some(text) = &filters.notes_contains {
        // Match the payee too: "coffee" should find a coffee shop, not only a
        // transaction someone happened to annotate.
        push(
            "(COALESCE(t.notes, '') LIKE ?1 ESCAPE '\\' OR COALESCE(vp.name, '') LIKE ?1 ESCAPE '\\')",
            Value::Text(format!("%{}%", escape_like(text))),
        );
    }

    let joins = "LEFT JOIN v_payees vp ON vp.id = t.payee \
                 LEFT JOIN categories cat ON cat.id = t.category";
    let where_sql = predicates.join(" AND ");

    let total: i64 = conn.query_row(
        &format!("SELECT COUNT(*) FROM {FROM} {joins} WHERE {where_sql}"),
        rusqlite::params_from_iter(params.iter()),
        |r| r.get(0),
    )?;

    let mut stmt = conn.prepare(&format!(
        "SELECT t.date, vp.name, cat.name, a.name, t.amount, t.notes, t.cleared,
                t.transfer_id IS NOT NULL
         FROM {FROM} {joins}
         WHERE {where_sql}
         ORDER BY t.date DESC, t.sort_order DESC
         LIMIT {}",
        limit.min(MAX_LIMIT),
    ))?;

    let rows = stmt
        .query_map(rusqlite::params_from_iter(params.iter()), |r| {
            Ok(TransactionRow {
                date: BudgetDate::from_int(r.get::<_, u32>(0)?).unwrap_or(BudgetDate::EPOCH),
                payee: r.get(1)?,
                category: r.get(2)?,
                account: r.get(3)?,
                amount: Money::from_cents(r.get(4)?),
                notes: r.get(5)?,
                cleared: r.get::<_, i64>(6)? != 0,
                is_transfer: r.get::<_, i64>(7)? != 0,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;

    Ok(TransactionPage {
        rows,
        total_matching: total as usize,
    })
}

/// `%` and `_` are wildcards in LIKE; a user searching for "50% off" means the
/// literal characters.
fn escape_like(raw: &str) -> String {
    raw.replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_")
}

/// Split category totals into spending and income.
///
/// Classified by the `is_income` flag, deliberately **not** by sign. A spending
/// category whose refunds exceeded its purchases in a given month still
/// represents spending; classifying by sign would silently reclassify it as
/// income and drop its outgoings from the spending total.
pub fn split_totals(categories: &[CategoryTotal]) -> (Money, Money) {
    let spending = categories
        .iter()
        .filter(|c| !c.is_income)
        .map(|c| c.total)
        .sum();
    let income = categories
        .iter()
        .filter(|c| c.is_income)
        .map(|c| c.total)
        .sum();
    (spending, income)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A miniature budget with one of everything the scope rules care about.
    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "CREATE TABLE accounts (id TEXT PRIMARY KEY, name TEXT, offbudget INTEGER DEFAULT 0,
                                    closed INTEGER DEFAULT 0, tombstone INTEGER DEFAULT 0);
             CREATE TABLE categories (id TEXT PRIMARY KEY, name TEXT, is_income INTEGER DEFAULT 0,
                                      tombstone INTEGER DEFAULT 0);
             CREATE TABLE payees (id TEXT PRIMARY KEY, name TEXT, transfer_acct TEXT);
             CREATE TABLE transactions (id TEXT PRIMARY KEY, acct TEXT, category TEXT, amount INTEGER,
                                        date INTEGER, is_parent INTEGER DEFAULT 0,
                                        starting_balance_flag INTEGER DEFAULT 0,
                                        transfer_id TEXT, payee TEXT, tombstone INTEGER DEFAULT 0);
             -- stands in for the real view, with the same column names
             CREATE VIEW v_transactions AS
               SELECT _.id, _.acct AS account, _.category, _.amount, _.date, _.is_parent,
                      _.starting_balance_flag, _.transfer_id, _.payee
               FROM transactions _ WHERE _.tombstone = 0;

             INSERT INTO accounts (id, name, offbudget) VALUES
               ('on', 'Checking', 0), ('on2', 'Savings', 0), ('off', '401k', 1);
             -- transfer payees: one pointing at an on-budget account, one off-budget
             INSERT INTO payees (id, name, transfer_acct) VALUES
               ('to_on2', 'Savings', 'on2'), ('to_off', '401k', 'off');
             INSERT INTO categories (id, name, is_income) VALUES
               ('groc', 'Groceries', 0), ('pay', 'Salary', 1);",
        )
        .unwrap();
        c
    }

    fn insert(c: &Connection, id: &str, acct: &str, cat: Option<&str>, amount: i64, date: u32) {
        c.execute(
            "INSERT INTO transactions (id, acct, category, amount, date) VALUES (?1,?2,?3,?4,?5)",
            rusqlite::params![id, acct, cat, amount, date],
        )
        .unwrap();
    }

    fn run(c: &Connection, from: u32, to: u32) -> Vec<CategoryTotal> {
        spending_by_category(
            c,
            BudgetDate::from_int(from).unwrap(),
            BudgetDate::from_int(to).unwrap(),
            TxScope::spending(),
        )
        .unwrap()
    }

    fn find<'a>(rows: &'a [CategoryTotal], name: &str) -> Option<&'a CategoryTotal> {
        rows.iter().find(|r| r.category == name)
    }

    #[test]
    fn groups_and_sums_by_category() {
        let c = db();
        insert(&c, "t1", "on", Some("groc"), -1000, 20260815);
        insert(&c, "t2", "on", Some("groc"), -500, 20260816);
        insert(&c, "t3", "on", Some("pay"), 250_000, 20260801);

        let rows = run(&c, 20260801, 20260831);

        let groceries = find(&rows, "Groceries").expect("groceries");
        assert_eq!(groceries.total, Money::from_cents(-1500));
        assert_eq!(groceries.transaction_count, 2);
        assert!(!groceries.is_income);

        let salary = find(&rows, "Salary").expect("salary");
        assert_eq!(salary.total, Money::from_cents(250_000));
        assert!(salary.is_income);
    }

    /// FR-4.4: the buckets must sum to the whole, so uncategorised can't vanish.
    #[test]
    fn uncategorised_gets_its_own_bucket() {
        let c = db();
        insert(&c, "t1", "on", Some("groc"), -1000, 20260815);
        insert(&c, "t2", "on", None, -700, 20260816);

        let rows = run(&c, 20260801, 20260831);
        assert_eq!(
            find(&rows, UNCATEGORISED).unwrap().total,
            Money::from_cents(-700)
        );

        let sum: Money = rows.iter().map(|r| r.total).sum();
        assert_eq!(sum, Money::from_cents(-1700));
    }

    #[test]
    fn the_date_range_is_inclusive_at_both_ends() {
        let c = db();
        insert(&c, "t1", "on", Some("groc"), -100, 20260801);
        insert(&c, "t2", "on", Some("groc"), -200, 20260831);
        insert(&c, "t3", "on", Some("groc"), -400, 20260731);
        insert(&c, "t4", "on", Some("groc"), -800, 20260901);

        let rows = run(&c, 20260801, 20260831);
        assert_eq!(
            find(&rows, "Groceries").unwrap().total,
            Money::from_cents(-300)
        );
    }

    #[test]
    fn transfers_are_excluded() {
        let c = db();
        insert(&c, "t1", "on", Some("groc"), -1000, 20260815);
        // a transfer between two on-budget accounts: not spending
        c.execute(
            "INSERT INTO transactions (id, acct, category, amount, date, transfer_id, payee)
             VALUES ('t2','on','groc',-9999,20260815,'other-side','to_on2')",
            [],
        )
        .unwrap();

        assert_eq!(
            find(&run(&c, 20260801, 20260831), "Groceries")
                .unwrap()
                .total,
            Money::from_cents(-1000)
        );
    }

    /// Moving money to an off-budget account is money *leaving* the budget, and
    /// Actual counts it against whatever category it carries. Excluding it
    /// understates spending for anyone who contributes to a brokerage or
    /// retirement account from a budgeted one.
    #[test]
    fn a_transfer_to_an_offbudget_account_is_spending() {
        let c = db();
        insert(&c, "t1", "on", Some("groc"), -1000, 20260815);
        c.execute(
            "INSERT INTO transactions (id, acct, category, amount, date, transfer_id, payee)
             VALUES ('t2','on','groc',-500,20260815,'other-side','to_off')",
            [],
        )
        .unwrap();

        assert_eq!(
            find(&run(&c, 20260801, 20260831), "Groceries")
                .unwrap()
                .total,
            Money::from_cents(-1500),
            "a contribution to an off-budget account counts as spending"
        );
    }

    #[test]
    fn offbudget_accounts_are_excluded() {
        let c = db();
        insert(&c, "t1", "on", Some("groc"), -1000, 20260815);
        insert(&c, "t2", "off", Some("groc"), -50_000, 20260815);

        assert_eq!(
            find(&run(&c, 20260801, 20260831), "Groceries")
                .unwrap()
                .total,
            Money::from_cents(-1000)
        );
    }

    #[test]
    fn split_parents_are_excluded() {
        let c = db();
        insert(&c, "t1", "on", Some("groc"), -1000, 20260815);
        c.execute(
            "INSERT INTO transactions (id, acct, category, amount, date, is_parent)
             VALUES ('p','on','groc',-1000,20260815,1)",
            [],
        )
        .unwrap();

        assert_eq!(
            find(&run(&c, 20260801, 20260831), "Groceries")
                .unwrap()
                .total,
            Money::from_cents(-1000),
            "the parent must not double the children"
        );
    }

    #[test]
    fn an_empty_range_returns_no_rows_rather_than_failing() {
        let c = db();
        insert(&c, "t1", "on", Some("groc"), -1000, 20260815);
        assert!(run(&c, 20260101, 20260131).is_empty());
    }

    fn total(name: &str, is_income: bool, cents: i64) -> CategoryTotal {
        CategoryTotal {
            category: name.to_string(),
            is_income,
            total: Money::from_cents(cents),
            transaction_count: 1,
        }
    }

    #[test]
    fn totals_split_on_the_income_flag() {
        let rows = [
            total("Groceries", false, -78_376),
            total("Dining", false, -50_578),
            total("Salary", true, 236_406),
        ];
        let (spending, income) = split_totals(&rows);
        assert_eq!(spending, Money::from_cents(-128_954));
        assert_eq!(income, Money::from_cents(236_406));
    }

    /// The reason this is not classified by sign. A month where medical
    /// reimbursements exceed medical spending must not turn Medical into
    /// income and quietly remove its outgoings from the spending total.
    #[test]
    fn a_spending_category_that_nets_positive_is_still_spending() {
        let rows = [
            total("Groceries", false, -10_000),
            total("Medical", false, 15_000), // big reimbursement month
        ];
        let (spending, income) = split_totals(&rows);
        assert_eq!(
            spending,
            Money::from_cents(5_000),
            "both belong to spending"
        );
        assert_eq!(income, Money::ZERO, "neither is an income category");
    }

    #[test]
    fn an_income_category_that_nets_negative_is_still_income() {
        let rows = [total("Salary", true, -500)];
        let (spending, income) = split_totals(&rows);
        assert_eq!(spending, Money::ZERO);
        assert_eq!(income, Money::from_cents(-500));
    }

    #[test]
    fn no_categories_totals_to_zero() {
        let (spending, income) = split_totals(&[]);
        assert_eq!(spending, Money::ZERO);
        assert_eq!(income, Money::ZERO);
    }
}
