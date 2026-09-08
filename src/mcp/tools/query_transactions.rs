use std::{borrow::Cow, sync::Arc};

use rmcp::{
    handler::server::router::tool::{AsyncTool, ToolBase},
    model::JsonObject,
    schemars,
};
use serde::{Deserialize, Serialize};

use crate::{
    domain::{BudgetDate, Money},
    error::ToolError,
    mcp::server::BudgetServer,
    queries::{
        resolve::{self, Resolution},
        scope::TxScope,
        transactions::{self, TxFilters},
    },
};

pub struct QueryTransactions;

#[derive(Debug, Default, Deserialize, schemars::JsonSchema)]
pub struct Input {
    /// Inclusive start date, ISO 8601. Example: 2026-08-01
    pub from: Option<String>,
    /// Inclusive end date, ISO 8601. Example: 2026-08-31
    pub to: Option<String>,
    /// Account name. Partial names are fine; call list_accounts for exact ones.
    pub account: Option<String>,
    /// Category name. Partial names are fine; call list_categories for exact ones.
    pub category: Option<String>,
    /// Payee name. Partial names are fine.
    pub payee: Option<String>,
    /// Smallest amount in dollars, signed. Use -100 to find outflows of $100 or
    /// less, since outflows are negative.
    pub min_amount: Option<f64>,
    /// Largest amount in dollars, signed.
    pub max_amount: Option<f64>,
    /// Matches the notes or the payee name, case-insensitively.
    pub notes_contains: Option<String>,
    /// Include transfers between the user's own accounts. Defaults to true;
    /// they are labelled with `is_transfer` so they can be told apart.
    pub include_transfers: Option<bool>,
    /// Maximum rows to return. Defaults to 100, capped at 1000.
    pub limit: Option<u32>,
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct Output {
    /// Newest first.
    pub transactions: Vec<transactions::TransactionRow>,
    /// How many matched in total, which may exceed the rows returned.
    pub total_matching: usize,
    /// True when `total_matching` is larger than the rows returned. Narrow the
    /// date range or raise `limit` to see the rest.
    pub truncated: bool,
    /// Present when a name could not be matched to exactly one thing. The
    /// transaction list is empty in that case; the text says what to do.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

impl ToolBase for QueryTransactions {
    type Parameter = Input;
    type Output = Output;
    type Error = ToolError;

    fn name() -> Cow<'static, str> {
        "query_transactions".into()
    }

    fn description() -> Option<Cow<'static, str>> {
        Some(
            "Lists individual transactions, newest first, with any combination of \
             filters. Amounts are in dollars and negative means money out. Accounts, \
             categories and payees are given by name, not id; a name that matches \
             several things comes back as a note listing the candidates instead of a \
             guess. Transfers between the user's own accounts are included by default \
             and flagged with is_transfer, so a register reconciles — but they are not \
             spending. Use spending_by_category for totals rather than summing these \
             rows."
                .into(),
        )
    }

    fn input_schema() -> Option<Arc<JsonObject>> {
        Some(
            rmcp::handler::server::common::schema_for_input::<
                rmcp::handler::server::wrapper::Parameters<Input>,
            >()
            .expect("Input schema"),
        )
    }
}

impl AsyncTool<BudgetServer> for QueryTransactions {
    async fn invoke(
        service: &BudgetServer,
        param: Self::Parameter,
    ) -> Result<Self::Output, Self::Error> {
        service.refresh_if_stale().await?;

        let from = parse_opt_date(param.from.as_deref(), "from")?;
        let to = parse_opt_date(param.to.as_deref(), "to")?;
        if let (Some(f), Some(t)) = (from, to)
            && f > t
        {
            return Err(ToolError::BadArgument(format!(
                "`from` ({}) is after `to` ({})",
                f.to_iso(),
                t.to_iso()
            )));
        }

        // Names have to become ids before anything can be queried, and an
        // unresolvable name is reported rather than guessed at (FR-5.3).
        let (want_account, want_category, want_payee) = (
            param.account.clone(),
            param.category.clone(),
            param.payee.clone(),
        );

        let resolved = service
            .read(move |conn| {
                let account = match &want_account {
                    Some(q) => Some((q.clone(), resolve::resolve(&resolve::accounts(conn)?, q))),
                    None => None,
                };
                let category = match &want_category {
                    Some(q) => Some((q.clone(), resolve::resolve(&resolve::categories(conn)?, q))),
                    None => None,
                };
                let payee = match &want_payee {
                    Some(q) => Some((q.clone(), resolve::resolve(&resolve::payees(conn)?, q))),
                    None => None,
                };
                Ok((account, category, payee))
            })
            .await?;

        let mut notes = Vec::new();
        let mut take = |kind: &str, found: Option<(String, Resolution)>| match found {
            None => None,
            Some((_, Resolution::One(named))) => Some(named.id),
            Some((query, other)) => {
                notes.push(other.describe(kind, &query));
                None
            }
        };
        let account_id = take("account", resolved.0);
        let category_id = take("category", resolved.1);
        let payee_id = take("payee", resolved.2);

        // One unusable name makes the whole result untrustworthy, so say so
        // instead of quietly answering a different question.
        if !notes.is_empty() {
            return Ok(Output {
                transactions: Vec::new(),
                total_matching: 0,
                truncated: false,
                note: Some(notes.join(" ")),
            });
        }

        let filters = TxFilters {
            from,
            to,
            account_id,
            category_id,
            payee_id,
            min_amount: param.min_amount.map(to_money),
            max_amount: param.max_amount.map(to_money),
            notes_contains: param.notes_contains.clone(),
        };

        let scope = TxScope::listing().with_transfers(param.include_transfers.unwrap_or(true));
        let limit = param
            .limit
            .map(|l| l as usize)
            .unwrap_or(transactions::DEFAULT_LIMIT)
            .clamp(1, transactions::MAX_LIMIT);

        let page = service
            .read(move |conn| Ok(transactions::list(conn, &filters, scope, limit)?))
            .await?;

        let truncated = page.total_matching > page.rows.len();
        Ok(Output {
            transactions: page.rows,
            total_matching: page.total_matching,
            truncated,
            note: truncated.then(|| {
                format!(
                    "Showing the {} most recent of {} matching transactions.",
                    limit, page.total_matching
                )
            }),
        })
    }
}

fn parse_opt_date(raw: Option<&str>, field: &str) -> Result<Option<BudgetDate>, ToolError> {
    raw.map(|r| {
        BudgetDate::parse_iso(r.trim()).ok_or_else(|| {
            ToolError::BadArgument(format!(
                "`{field}` must be an ISO 8601 date like 2026-08-01, got {r:?}"
            ))
        })
    })
    .transpose()
}

fn to_money(dollars: f64) -> Money {
    Money::from_cents((dollars * 100.0).round() as i64)
}
