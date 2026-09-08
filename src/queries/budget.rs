//! The monthly budget: what was set aside, what was spent, what remains.
//!
//! Actual runs an envelope budget. A category's balance is not simply
//! `budgeted + spent` — a surplus rolls into the next month, so the balance has
//! to be accumulated forward from the first month the budget has any history.
//! Overspending does *not* roll (it is absorbed into the month's "overspent"
//! figure) unless the category's carryover flag is set.

use std::collections::HashMap;

use rusqlite::Connection;
use schemars::JsonSchema;
use serde::Serialize;

use crate::domain::{BudgetMonth, Money};
use crate::queries::scope::{FROM, TxScope};

#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
pub struct CategoryBudget {
    pub category: String,
    /// The category group it sits under, e.g. "Regular Expense".
    pub group: String,
    pub budgeted: Money,
    /// Negative for money spent.
    pub spent: Money,
    /// `budgeted + spent`, plus any surplus rolled in from previous months.
    pub balance: Money,
}

#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
pub struct IncomeTotal {
    pub category: String,
    pub received: Money,
}

#[derive(Debug, Clone, PartialEq)]
pub struct MonthlyBudget {
    pub categories: Vec<CategoryBudget>,
    pub income: Vec<IncomeTotal>,
    /// True when the file holds tracking-budget rows, which this query does not
    /// understand (FR-4.8). Reported rather than silently returning zeros.
    pub uses_tracking_budget: bool,
}

struct Category {
    name: String,
    group: String,
    is_income: bool,
}

pub fn monthly_budget(
    conn: &Connection,
    month: BudgetMonth,
    scope: TxScope,
) -> Result<MonthlyBudget, rusqlite::Error> {
    let categories = load_categories(conn)?;
    let budgets = load_budgets(conn, month)?;
    let spending = load_spending(conn, month, scope)?;

    let uses_tracking_budget: i64 =
        conn.query_row("SELECT count(*) FROM reflect_budgets", [], |r| r.get(0))?;

    // Walk from the first month with any history so surpluses accumulate.
    let start = budgets
        .keys()
        .chain(spending.keys())
        .map(|(_, m)| *m)
        .min()
        .unwrap_or(month);

    let mut rows = Vec::new();
    let mut income = Vec::new();

    for (id, cat) in &categories {
        let spent = spending
            .get(&(id.clone(), month))
            .copied()
            .unwrap_or_default();

        if cat.is_income {
            if spent != Money::ZERO {
                income.push(IncomeTotal {
                    category: cat.name.clone(),
                    received: spent,
                });
            }
            continue;
        }

        let mut balance = Money::ZERO;
        let mut m = start;
        loop {
            let entry = budgets.get(&(id.clone(), m));
            let budgeted = entry.map(|e| e.0).unwrap_or_default();
            let carryover = entry.is_some_and(|e| e.1);
            let month_spent = spending.get(&(id.clone(), m)).copied().unwrap_or_default();

            // A surplus rolls forward; overspending is absorbed by the month
            // unless the category is explicitly set to carry it.
            let carried = if balance.cents() > 0 || carryover {
                balance
            } else {
                Money::ZERO
            };
            balance = carried + budgeted + month_spent;

            if m == month {
                break;
            }
            m = m.next();
        }

        rows.push(CategoryBudget {
            category: cat.name.clone(),
            group: cat.group.clone(),
            budgeted: budgets
                .get(&(id.clone(), month))
                .map(|e| e.0)
                .unwrap_or_default(),
            spent,
            balance,
        });
    }

    rows.sort_by(|a, b| {
        a.group
            .cmp(&b.group)
            .then_with(|| a.category.cmp(&b.category))
    });
    income.sort_by(|a, b| a.category.cmp(&b.category));

    Ok(MonthlyBudget {
        categories: rows,
        income,
        uses_tracking_budget: uses_tracking_budget > 0,
    })
}

fn load_categories(conn: &Connection) -> Result<Vec<(String, Category)>, rusqlite::Error> {
    let mut stmt = conn.prepare(
        "SELECT c.id, c.name, COALESCE(g.name, ''), c.is_income
         FROM categories c
         LEFT JOIN category_groups g ON g.id = c.cat_group
         WHERE c.tombstone = 0",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok((
            r.get::<_, String>(0)?,
            Category {
                name: r.get(1)?,
                group: r.get(2)?,
                is_income: r.get::<_, i64>(3)? != 0,
            },
        ))
    })?;
    rows.collect()
}

/// `(category, month) -> (budgeted, carryover flag)`, up to and including `month`.
type Budgets = HashMap<(String, BudgetMonth), (Money, bool)>;

fn load_budgets(conn: &Connection, month: BudgetMonth) -> Result<Budgets, rusqlite::Error> {
    let mut stmt = conn
        .prepare("SELECT category, month, amount, carryover FROM zero_budgets WHERE month <= ?1")?;
    let rows = stmt.query_map([month.as_int()], |r| {
        let m = BudgetMonth::from_int(r.get::<_, u32>(1)?).unwrap_or(month);
        Ok((
            (r.get::<_, String>(0)?, m),
            (Money::from_cents(r.get(2)?), r.get::<_, i64>(3)? != 0),
        ))
    })?;
    rows.collect()
}

type Spending = HashMap<(String, BudgetMonth), Money>;

fn load_spending(
    conn: &Connection,
    month: BudgetMonth,
    scope: TxScope,
) -> Result<Spending, rusqlite::Error> {
    let sql = format!(
        "SELECT t.category, t.date / 100 AS mo, SUM(t.amount)
         FROM {FROM}
         WHERE {scope} AND t.category IS NOT NULL AND t.date / 100 <= ?1
         GROUP BY t.category, mo",
        scope = scope.where_sql(),
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map([month.as_int()], |r| {
        let m = BudgetMonth::from_int(r.get::<_, u32>(1)?).unwrap_or(month);
        Ok(((r.get::<_, String>(0)?, m), Money::from_cents(r.get(2)?)))
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
                                    closed INTEGER DEFAULT 0, tombstone INTEGER DEFAULT 0);
             CREATE TABLE category_groups (id TEXT PRIMARY KEY, name TEXT);
             CREATE TABLE categories (id TEXT PRIMARY KEY, name TEXT, cat_group TEXT,
                                      is_income INTEGER DEFAULT 0, tombstone INTEGER DEFAULT 0);
             CREATE TABLE payees (id TEXT PRIMARY KEY, name TEXT, transfer_acct TEXT);
             CREATE TABLE transactions (id TEXT PRIMARY KEY, acct TEXT, category TEXT, amount INTEGER,
                                        date INTEGER, is_parent INTEGER DEFAULT 0,
                                        starting_balance_flag INTEGER DEFAULT 0,
                                        transfer_id TEXT, payee TEXT, tombstone INTEGER DEFAULT 0);
             CREATE VIEW v_transactions AS
               SELECT _.id, _.acct AS account, _.category, _.amount, _.date, _.is_parent,
                      _.starting_balance_flag, _.transfer_id, _.payee
               FROM transactions _ WHERE _.tombstone = 0;
             CREATE TABLE zero_budgets (id TEXT PRIMARY KEY, month INTEGER, category TEXT,
                                        amount INTEGER DEFAULT 0, carryover INTEGER DEFAULT 0);
             CREATE TABLE reflect_budgets (id TEXT PRIMARY KEY, month INTEGER, category TEXT,
                                           amount INTEGER DEFAULT 0);

             INSERT INTO accounts (id, name) VALUES ('on', 'Checking');
             INSERT INTO category_groups (id, name) VALUES ('g1', 'Regular Expense');
             INSERT INTO categories (id, name, cat_group, is_income) VALUES
               ('groc', 'Groceries', 'g1', 0),
               ('pay',  'Salary',    'g1', 1);",
        )
        .unwrap();
        c
    }

    fn budgeted(c: &Connection, cat: &str, month: u32, cents: i64, carryover: bool) {
        c.execute(
            "INSERT INTO zero_budgets (id, month, category, amount, carryover)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            rusqlite::params![
                format!("{cat}-{month}"),
                month,
                cat,
                cents,
                carryover as i64
            ],
        )
        .unwrap();
    }

    fn spend(c: &Connection, id: &str, cat: &str, cents: i64, date: u32) {
        c.execute(
            "INSERT INTO transactions (id, acct, category, amount, date) VALUES (?1,'on',?2,?3,?4)",
            rusqlite::params![id, cat, cents, date],
        )
        .unwrap();
    }

    fn august(c: &Connection) -> MonthlyBudget {
        monthly_budget(
            c,
            BudgetMonth::from_int(202608).unwrap(),
            TxScope::spending(),
        )
        .unwrap()
    }

    fn groceries(b: &MonthlyBudget) -> &CategoryBudget {
        b.categories
            .iter()
            .find(|c| c.category == "Groceries")
            .unwrap()
    }

    #[test]
    fn balance_is_budgeted_plus_spent_with_no_history() {
        let c = db();
        budgeted(&c, "groc", 202608, 70_000, false);
        spend(&c, "t1", "groc", -78_376, 20260815);

        let b = august(&c); // bind: `groceries` borrows from it
        let g = groceries(&b);
        assert_eq!(g.budgeted, Money::from_cents(70_000));
        assert_eq!(g.spent, Money::from_cents(-78_376));
        assert_eq!(g.balance, Money::from_cents(-8_376));
        assert_eq!(g.group, "Regular Expense");
    }

    /// A surplus in one month offsets an overspend in the next. Easy to miss,
    /// because the balance then disagrees with `budgeted + spent`.
    #[test]
    fn a_surplus_carries_into_the_next_month() {
        let c = db();
        budgeted(&c, "groc", 202607, 10_000, false);
        spend(&c, "t1", "groc", -4_000, 20260715); // +6,000 left over
        budgeted(&c, "groc", 202608, 10_000, false);
        spend(&c, "t2", "groc", -12_000, 20260815);

        assert_eq!(
            groceries(&august(&c)).balance,
            Money::from_cents(4_000),
            "6,000 carried + 10,000 budgeted - 12,000 spent"
        );
    }

    /// Overspending is absorbed by the month it happened in, not inherited.
    #[test]
    fn an_overspend_does_not_carry_forward() {
        let c = db();
        budgeted(&c, "groc", 202607, 10_000, false);
        spend(&c, "t1", "groc", -15_000, 20260715); // -5,000
        budgeted(&c, "groc", 202608, 10_000, false);

        assert_eq!(
            groceries(&august(&c)).balance,
            Money::from_cents(10_000),
            "July's shortfall must not reduce August"
        );
    }

    /// ...unless the category is explicitly set to carry it.
    #[test]
    fn the_carryover_flag_makes_an_overspend_carry() {
        let c = db();
        budgeted(&c, "groc", 202607, 10_000, true);
        spend(&c, "t1", "groc", -15_000, 20260715);
        budgeted(&c, "groc", 202608, 10_000, true);

        assert_eq!(groceries(&august(&c)).balance, Money::from_cents(5_000));
    }

    #[test]
    fn income_is_reported_separately_and_never_budgeted() {
        let c = db();
        spend(&c, "t1", "pay", 236_406, 20260801);

        let b = august(&c);
        assert!(b.categories.iter().all(|c| c.category != "Salary"));
        assert_eq!(b.income.len(), 1);
        assert_eq!(b.income[0].category, "Salary");
        assert_eq!(b.income[0].received, Money::from_cents(236_406));
    }

    /// FR-4.8: say so rather than quietly reporting zeros.
    #[test]
    fn tracking_budget_data_is_flagged() {
        let c = db();
        assert!(!august(&c).uses_tracking_budget);

        c.execute(
            "INSERT INTO reflect_budgets (id, month, category, amount) VALUES ('r1', 202608, 'groc', 100)",
            [],
        )
        .unwrap();
        assert!(august(&c).uses_tracking_budget);
    }

    #[test]
    fn later_months_do_not_leak_into_the_balance() {
        let c = db();
        budgeted(&c, "groc", 202608, 10_000, false);
        budgeted(&c, "groc", 202609, 99_999, false);
        spend(&c, "t1", "groc", -4_000, 20260915);

        let b = august(&c);
        let g = groceries(&b);
        assert_eq!(g.budgeted, Money::from_cents(10_000));
        assert_eq!(g.spent, Money::ZERO);
        assert_eq!(g.balance, Money::from_cents(10_000));
    }
}
