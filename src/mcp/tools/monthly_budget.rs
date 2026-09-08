use std::{borrow::Cow, sync::Arc};

use rmcp::{
    handler::server::router::tool::{AsyncTool, ToolBase},
    model::JsonObject,
    schemars,
};
use serde::{Deserialize, Serialize};

use crate::{
    domain::{BudgetMonth, Money},
    error::ToolError,
    mcp::server::BudgetServer,
    queries::{budget, scope::TxScope},
};

pub struct GetMonthlyBudget;

#[derive(Debug, Default, Deserialize, schemars::JsonSchema)]
pub struct Input {
    /// The budget month, ISO form. Example: 2026-08
    pub month: String,
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct Output {
    pub month: BudgetMonth,
    /// Expense categories, grouped as they appear in the budget.
    pub categories: Vec<budget::CategoryBudget>,
    /// Income received in the month. Income categories are not budgeted.
    pub income: Vec<budget::IncomeTotal>,
    pub total_budgeted: Money,
    pub total_spent: Money,
    /// Includes surpluses rolled forward from earlier months, so it is not
    /// simply `total_budgeted + total_spent`.
    pub total_balance: Money,
    pub total_income: Money,
    /// Present only when something makes these figures unreliable.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub warning: Option<String>,
    pub excludes: String,
}

const EXCLUDES: &str = "Spending excludes transfers between the user's own on-budget \
                        accounts, activity inside off-budget accounts, closed accounts, \
                        and opening balances. Money moved from an on-budget account into \
                        an off-budget one IS counted, because it leaves the budget.";

impl ToolBase for GetMonthlyBudget {
    type Parameter = Input;
    type Output = Output;
    type Error = ToolError;

    fn name() -> Cow<'static, str> {
        "get_monthly_budget".into()
    }

    fn description() -> Option<Cow<'static, str>> {
        Some(
            "For one month, what was budgeted per category, what was spent, and what \
             remains. Amounts are in dollars; spending is negative. This is an envelope \
             budget, so a category's balance carries any surplus forward from previous \
             months and is therefore not just budgeted plus spent. Overspending does not \
             carry forward. Income categories are reported separately, as received. \
             Spending excludes transfers between the user's own on-budget accounts and \
             activity inside off-budget accounts such as retirement and brokerage."
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

impl AsyncTool<BudgetServer> for GetMonthlyBudget {
    async fn invoke(
        service: &BudgetServer,
        param: Self::Parameter,
    ) -> Result<Self::Output, Self::Error> {
        service.refresh_if_stale().await?;

        let month = BudgetMonth::parse_iso(param.month.trim()).ok_or_else(|| {
            ToolError::BadArgument(format!(
                "`month` must be an ISO month like 2026-08, got {:?}",
                param.month
            ))
        })?;

        let result = service
            .read(move |conn| Ok(budget::monthly_budget(conn, month, TxScope::spending())?))
            .await?;

        let total_budgeted = result.categories.iter().map(|c| c.budgeted).sum();
        let total_spent = result.categories.iter().map(|c| c.spent).sum();
        let total_balance = result.categories.iter().map(|c| c.balance).sum();
        let total_income = result.income.iter().map(|i| i.received).sum();

        Ok(Output {
            month,
            categories: result.categories,
            income: result.income,
            total_budgeted,
            total_spent,
            total_balance,
            total_income,
            warning: result.uses_tracking_budget.then(|| {
                "This budget file contains tracking-budget data, which this server does \
                 not read. The figures below come from the envelope budget only and may \
                 be incomplete."
                    .to_string()
            }),
            excludes: EXCLUDES.to_string(),
        })
    }
}
