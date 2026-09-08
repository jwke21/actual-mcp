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
    queries::{scope::TxScope, transactions},
};

/// What `spending()` leaves out, stated in the response so the model can
/// qualify the figure instead of presenting it as every dollar that moved.
const EXCLUDES: &str = "Excludes transfers between the user's own on-budget accounts, \
                        activity inside off-budget accounts such as retirement and \
                        brokerage, closed accounts, and opening balances. Money moved \
                        from an on-budget account into an off-budget one IS counted as \
                        spending, because it leaves the budget.";

pub struct SpendingByCategory;

#[derive(Debug, Default, Deserialize, schemars::JsonSchema)]
pub struct Input {
    /// Inclusive start date, ISO 8601. Example: 2026-08-01
    pub from: String,
    /// Inclusive end date, ISO 8601. Example: 2026-08-31
    pub to: String,
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct Output {
    /// Echoed back so the figures cannot be attributed to the wrong period.
    pub from: BudgetDate,
    pub to: BudgetDate,
    /// Largest outflow first.
    pub categories: Vec<transactions::CategoryTotal>,
    /// Net total across all non-income categories. Negative means money left
    /// the budget. Refunds are already netted against the category they
    /// belong to.
    pub total_spending: Money,
    /// Net total across all income categories.
    pub total_income: Money,
    /// `total_income + total_spending`.
    pub net: Money,
    pub excludes: String,
}

impl ToolBase for SpendingByCategory {
    type Parameter = Input;
    type Output = Output;
    type Error = ToolError;

    fn name() -> Cow<'static, str> {
        "spending_by_category".into()
    }

    fn description() -> Option<Cow<'static, str>> {
        Some(
            "Totals spending and income per category over an inclusive date range. \
             Amounts are in dollars: negative is money out, positive is money in. \
             Transfers between the user's own accounts and off-budget accounts \
             (retirement, brokerage) are excluded, because neither is spending. \
             Transactions with no category appear as \"Uncategorised\" rather than \
             being dropped, so the categories always sum to the totals."
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

impl AsyncTool<BudgetServer> for SpendingByCategory {
    async fn invoke(
        service: &BudgetServer,
        param: Self::Parameter,
    ) -> Result<Self::Output, Self::Error> {
        service.refresh_if_stale().await?;

        let from = parse_date(&param.from, "from")?;
        let to = parse_date(&param.to, "to")?;
        if from > to {
            return Err(ToolError::BadArgument(format!(
                "`from` ({}) is after `to` ({})",
                from.to_iso(),
                to.to_iso()
            )));
        }

        let categories = service
            .read(move |conn| {
                Ok(transactions::spending_by_category(
                    conn,
                    from,
                    to,
                    TxScope::spending(),
                )?)
            })
            .await?;

        // Split on the is_income flag, not on sign: see split_totals.
        let (total_spending, total_income) = transactions::split_totals(&categories);

        Ok(Output {
            from,
            to,
            categories,
            total_spending,
            total_income,
            net: total_income + total_spending,
            excludes: EXCLUDES.to_string(),
        })
    }
}

fn parse_date(raw: &str, field: &str) -> Result<BudgetDate, ToolError> {
    BudgetDate::parse_iso(raw).ok_or_else(|| {
        ToolError::BadArgument(format!(
            "`{field}` must be an ISO 8601 date like 2026-08-01, got {raw:?}"
        ))
    })
}
