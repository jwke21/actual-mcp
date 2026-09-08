//! Every FR-4 correctness rule, in one place.
//!
//! These predicates decide whether the numbers are right. Summing every row in
//! `v_transactions` is not the same as summing spending, and on a budget that
//! holds investment accounts the two can differ by more than an order of
//! magnitude. If these rules ever get copy-pasted into individual query modules
//! they will drift, and two tools will quietly disagree about the same month.

/// The `FROM` clause every scoped query must use.
///
/// The aliases are part of the contract. [`TxScope::where_sql`] emits
/// predicates against:
///
/// - `t` — the transaction (`v_transactions`, tombstones already filtered)
/// - `a` — the account it sits on; an inner join is safe, a transaction always
///   has one
/// - `ta` — for a transfer, the account on the *other* side, reached through
///   the transfer payee. Null for anything that is not a transfer.
pub const FROM: &str = "v_transactions t \
     JOIN accounts a ON a.id = t.account \
     LEFT JOIN payees tp ON tp.id = t.payee \
     LEFT JOIN accounts ta ON ta.id = tp.transfer_acct";

/// Which rows a query is allowed to see.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TxScope {
    transfers: bool,
    offbudget: bool,
    closed_accounts: bool,
}

impl TxScope {
    /// Aggregation: money that actually entered or left the budget.
    ///
    /// Excludes transfers between your own accounts and off-budget accounts,
    /// because neither is spending (FR-4.2, FR-4.6).
    pub const fn spending() -> Self {
        Self {
            transfers: false,
            offbudget: false,
            closed_accounts: false,
        }
    }

    /// Listing: show the user their transactions, transfers included so the
    /// account register reconciles. Callers label them using `is_transfer`.
    pub const fn listing() -> Self {
        Self {
            transfers: true,
            offbudget: true,
            closed_accounts: false,
        }
    }

    /// FR-4.7: closed accounts are hidden unless asked for.
    pub const fn with_closed_accounts(mut self, include: bool) -> Self {
        self.closed_accounts = include;
        self
    }

    pub const fn with_transfers(mut self, include: bool) -> Self {
        self.transfers = include;
        self
    }

    pub const fn with_offbudget(mut self, include: bool) -> Self {
        self.offbudget = include;
        self
    }

    pub const fn includes_transfers(&self) -> bool {
        self.transfers
    }

    /// The predicates for this scope, in a stable order.
    pub fn predicates(&self) -> Vec<&'static str> {
        let mut p = Vec::with_capacity(5);

        // Always. Split parents duplicate the amounts of their children, so
        // aggregating over both double-counts (FR-4.5).
        p.push("t.is_parent = 0");

        // Always. Opening balances are bookkeeping, not activity (FR-4.3).
        p.push("t.starting_balance_flag = 0");

        // Tombstones need no predicate: v_transactions is built on
        // v_transactions_internal_alive, which already excludes them (FR-4.1).

        if !self.transfers {
            // Not every transfer is neutral. Moving money between two on-budget
            // accounts changes nothing the budget cares about, but moving it to
            // an *off-budget* account (a brokerage, a retirement fund) is money
            // leaving the budget, and Actual counts it as spending against
            // whatever category it carries.
            //
            // Excluding both kinds understates spending, by exactly the amount
            // contributed to investment and retirement accounts.
            p.push("(t.transfer_id IS NULL OR ta.offbudget = 1)");
        }
        if !self.offbudget {
            p.push("a.offbudget = 0");
        }
        if !self.closed_accounts {
            p.push("a.closed = 0");
        }

        p
    }

    /// The predicates as a single `AND`-joined SQL fragment.
    ///
    /// Never empty, so callers can always write `WHERE {scope} AND ...`.
    pub fn where_sql(&self) -> String {
        self.predicates().join(" AND ")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn has(scope: TxScope, needle: &str) -> bool {
        scope.predicates().contains(&needle)
    }

    const TRANSFERS: &str = "(t.transfer_id IS NULL OR ta.offbudget = 1)";
    const OFFBUDGET: &str = "a.offbudget = 0";
    const CLOSED: &str = "a.closed = 0";
    const PARENTS: &str = "t.is_parent = 0";
    const STARTING: &str = "t.starting_balance_flag = 0";

    /// Spending drops internal transfers but keeps the ones that leave the
    /// budget for an off-budget account.
    #[test]
    fn spending_excludes_transfers_and_offbudget() {
        let s = TxScope::spending();
        assert!(has(s, TRANSFERS));
        assert!(has(s, OFFBUDGET));
    }

    #[test]
    fn listing_keeps_transfers_and_offbudget() {
        let s = TxScope::listing();
        assert!(!has(s, TRANSFERS), "a register must reconcile");
        assert!(!has(s, OFFBUDGET));
    }

    /// Split parents duplicate their children's amounts. No scope may include
    /// them — this is the guard against silently doubling every split.
    #[test]
    fn split_parents_are_excluded_from_every_scope() {
        assert!(has(TxScope::spending(), PARENTS));
        assert!(has(TxScope::listing(), PARENTS));
        assert!(has(
            TxScope::listing()
                .with_closed_accounts(true)
                .with_transfers(true),
            PARENTS
        ));
    }

    #[test]
    fn starting_balances_are_excluded_from_every_scope() {
        assert!(has(TxScope::spending(), STARTING));
        assert!(has(TxScope::listing(), STARTING));
    }

    #[test]
    fn closed_accounts_are_opt_in() {
        assert!(has(TxScope::listing(), CLOSED));
        assert!(!has(TxScope::listing().with_closed_accounts(true), CLOSED));
    }

    #[test]
    fn where_sql_is_never_empty_so_callers_can_always_append() {
        let widest = TxScope::listing()
            .with_transfers(true)
            .with_offbudget(true)
            .with_closed_accounts(true);
        assert!(!widest.where_sql().is_empty());
        assert!(widest.where_sql().contains(" AND "));
    }

    #[test]
    fn predicates_join_with_and() {
        let sql = TxScope::spending().where_sql();
        assert_eq!(
            sql.matches(" AND ").count(),
            4,
            "5 predicates, 4 separators: {sql}"
        );
    }
}
