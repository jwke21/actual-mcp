//! The category list, as the budget organises it.

use rusqlite::Connection;
use schemars::JsonSchema;
use serde::Serialize;

#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
pub struct CategoryInfo {
    pub name: String,
    /// The group it sits under, e.g. "Regular Expense".
    pub group: String,
    /// Income categories are never budgeted; they record money coming in.
    pub is_income: bool,
    /// Hidden categories are kept out of Actual's budget view, usually because
    /// they are no longer used. Historical transactions still reference them.
    pub hidden: bool,
}

/// List categories in budget order: groups as the user arranged them, and
/// categories in their order within each group.
pub fn list(conn: &Connection, include_hidden: bool) -> Result<Vec<CategoryInfo>, rusqlite::Error> {
    let mut stmt = conn.prepare(
        "SELECT c.name, COALESCE(g.name, ''), c.is_income, c.hidden
         FROM categories c
         LEFT JOIN category_groups g ON g.id = c.cat_group AND g.tombstone = 0
         WHERE c.tombstone = 0 AND (?1 OR (c.hidden = 0 AND COALESCE(g.hidden, 0) = 0))
         ORDER BY g.sort_order, g.name, c.sort_order, c.name",
    )?;

    let rows = stmt.query_map([include_hidden], |r| {
        Ok(CategoryInfo {
            name: r.get(0)?,
            group: r.get(1)?,
            is_income: r.get::<_, i64>(2)? != 0,
            hidden: r.get::<_, i64>(3)? != 0,
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
            "CREATE TABLE category_groups (id TEXT PRIMARY KEY, name TEXT, is_income INTEGER DEFAULT 0,
                                           hidden INTEGER DEFAULT 0, sort_order REAL,
                                           tombstone INTEGER DEFAULT 0);
             CREATE TABLE categories (id TEXT PRIMARY KEY, name TEXT, cat_group TEXT,
                                      is_income INTEGER DEFAULT 0, hidden INTEGER DEFAULT 0,
                                      sort_order REAL, tombstone INTEGER DEFAULT 0);
             INSERT INTO category_groups (id, name, is_income, sort_order) VALUES
               ('g1', 'Regular Expense', 0, 1.0), ('g2', 'Income', 1, 2.0);",
        )
        .unwrap();
        c
    }

    fn category(c: &Connection, id: &str, name: &str, group: &str, hidden: bool, order: f64) {
        c.execute(
            "INSERT INTO categories (id, name, cat_group, hidden, sort_order, is_income)
             VALUES (?1, ?2, ?3, ?4, ?5, (SELECT is_income FROM category_groups WHERE id = ?3))",
            rusqlite::params![id, name, group, hidden as i64, order],
        )
        .unwrap();
    }

    #[test]
    fn categories_carry_their_group_and_income_flag() {
        let c = db();
        category(&c, "c1", "Groceries", "g1", false, 1.0);
        category(&c, "c2", "Salary", "g2", false, 1.0);

        let rows = list(&c, false).unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].name, "Groceries");
        assert_eq!(rows[0].group, "Regular Expense");
        assert!(!rows[0].is_income);
        assert!(rows[1].is_income);
    }

    #[test]
    fn ordering_follows_the_budget_not_the_alphabet() {
        let c = db();
        category(&c, "c1", "Zebra", "g1", false, 1.0);
        category(&c, "c2", "Apple", "g1", false, 2.0);

        let names: Vec<_> = list(&c, false)
            .unwrap()
            .into_iter()
            .map(|c| c.name)
            .collect();
        assert_eq!(names, ["Zebra", "Apple"]);
    }

    #[test]
    fn hidden_categories_are_opt_in() {
        let c = db();
        category(&c, "c1", "Groceries", "g1", false, 1.0);
        category(&c, "c2", "Old Commuting", "g1", true, 2.0);

        assert_eq!(list(&c, false).unwrap().len(), 1);
        let all = list(&c, true).unwrap();
        assert_eq!(all.len(), 2);
        assert!(all.iter().any(|c| c.hidden));
    }

    /// Hiding a group hides everything in it, even categories not hidden
    /// themselves.
    #[test]
    fn a_hidden_group_hides_its_categories() {
        let c = db();
        c.execute(
            "INSERT INTO category_groups (id, name, hidden, sort_order) VALUES ('g3','Archive',1,3.0)",
            [],
        )
        .unwrap();
        category(&c, "c1", "Groceries", "g1", false, 1.0);
        category(&c, "c2", "Old Thing", "g3", false, 1.0);

        assert_eq!(list(&c, false).unwrap().len(), 1);
        assert_eq!(list(&c, true).unwrap().len(), 2);
    }

    #[test]
    fn deleted_categories_never_appear() {
        let c = db();
        category(&c, "c1", "Groceries", "g1", false, 1.0);
        c.execute("UPDATE categories SET tombstone = 1 WHERE id = 'c1'", [])
            .unwrap();
        assert!(list(&c, true).unwrap().is_empty());
    }
}
