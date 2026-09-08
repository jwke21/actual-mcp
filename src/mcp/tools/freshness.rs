use std::{borrow::Cow, sync::Arc};

use rmcp::{
    handler::server::router::tool::{AsyncTool, ToolBase},
    model::JsonObject,
    schemars,
};
use serde::{Deserialize, Serialize};

use crate::{error::ToolError, mcp::server::BudgetServer, store::freshness::iso_date};

pub struct GetDataFreshness;

#[derive(Debug, Default, Deserialize, schemars::JsonSchema)]
pub struct Input {}

#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct Output {
    /// Date of the most recent transaction, ISO 8601. This is the best
    /// available proxy for when transactions were last imported from banks —
    /// an import the user runs manually in the Actual app.
    pub newest_transaction_date: Option<String>,

    /// How long ago this server last pulled changes from the Actual server.
    /// Absent if it has not managed to pull at all yet.
    pub last_retrieval_age_seconds: Option<u64>,

    /// True when the cached copy is older than the refresh interval. Data is
    /// still served when stale; this says how much to trust it.
    pub is_stale: bool,

    /// How often this server refreshes from Actual.
    pub refresh_interval_seconds: u64,
}

impl ToolBase for GetDataFreshness {
    type Parameter = Input;
    type Output = Output;
    type Error = ToolError;

    fn name() -> Cow<'static, str> {
        "get_data_freshness".into()
    }

    fn description() -> Option<Cow<'static, str>> {
        Some(
            "Reports how current the budget data is. Two different things can be \
             out of date: this server's copy of the budget (refreshed \
             automatically, see last_retrieval_age_seconds) and the budget itself, \
             which only gains new transactions when the user runs a bank import in \
             Actual (see newest_transaction_date). Check this before stating that \
             recent activity is complete."
                .into(),
        )
    }

    /// No parameters.
    fn input_schema() -> Option<Arc<JsonObject>> {
        None
    }
}

impl AsyncTool<BudgetServer> for GetDataFreshness {
    async fn invoke(
        service: &BudgetServer,
        _param: Self::Parameter,
    ) -> Result<Self::Output, Self::Error> {
        service.refresh_if_stale().await?;

        let now = std::time::SystemTime::now();
        let ttl = service.refresh_interval();
        let freshness = service.freshness()?;

        Ok(Output {
            newest_transaction_date: freshness.newest_transaction.map(iso_date),
            last_retrieval_age_seconds: freshness
                .last_retrieval
                .map(|_| crate::store::freshness::age(freshness.last_retrieval, now).as_secs()),
            is_stale: crate::store::freshness::is_stale(freshness.last_retrieval, now, ttl),
            refresh_interval_seconds: ttl.as_secs(),
        })
    }
}
