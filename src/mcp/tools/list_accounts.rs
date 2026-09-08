use std::{borrow::Cow, sync::Arc};

use rmcp::{
    handler::server::router::tool::{AsyncTool, ToolBase},
    model::JsonObject,
    schemars,
};
use serde::{Deserialize, Serialize};

use crate::{domain::Money, error::ToolError, mcp::server::BudgetServer, queries::accounts};

pub struct ListAccounts;

#[derive(Debug, Default, Deserialize, schemars::JsonSchema)]
pub struct Input {
    /// Include accounts the user has closed. Defaults to false.
    #[serde(default)]
    pub include_closed: bool,
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct Output {
    /// On-budget accounts first, then off-budget, each in the user's own order.
    pub accounts: Vec<accounts::AccountSummary>,
    /// Total across on-budget accounts: the money the budget actually governs.
    pub on_budget_total: Money,
    /// Total across off-budget accounts, such as retirement and brokerage.
    pub off_budget_total: Money,
    /// Everything added together.
    pub net_worth: Money,
}

impl ToolBase for ListAccounts {
    type Parameter = Input;
    type Output = Output;
    type Error = ToolError;

    fn name() -> Cow<'static, str> {
        "list_accounts".into()
    }

    fn description() -> Option<Cow<'static, str>> {
        Some(
            "Lists the user's accounts with their current balances in dollars. A \
             negative balance means money owed, as on a credit card. On-budget \
             accounts fund the envelope budget; off-budget accounts (retirement, \
             brokerage) are tracked for net worth but are never budgeted and are \
             excluded from spending figures. Balances include every transaction in \
             the account, transfers and opening balances included."
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

impl AsyncTool<BudgetServer> for ListAccounts {
    async fn invoke(
        service: &BudgetServer,
        param: Self::Parameter,
    ) -> Result<Self::Output, Self::Error> {
        service.refresh_if_stale().await?;

        let include_closed = param.include_closed;
        let accounts = service
            .read(move |conn| Ok(accounts::list(conn, include_closed)?))
            .await?;

        let on_budget_total = accounts
            .iter()
            .filter(|a| a.on_budget)
            .map(|a| a.balance)
            .sum();
        let off_budget_total: Money = accounts
            .iter()
            .filter(|a| !a.on_budget)
            .map(|a| a.balance)
            .sum();

        Ok(Output {
            accounts,
            on_budget_total,
            off_budget_total,
            net_worth: on_budget_total + off_budget_total,
        })
    }
}
