use std::time::{Duration, SystemTime};

use rmcp::{ServerHandler, handler::server::router::tool::ToolRouter, model::*, tool_handler};

use crate::actual::{client::ActualClient, model::UserFile};
use crate::config::Config;
use crate::error::{StartupError, ToolError};
use crate::mcp::tools::{
    freshness::GetDataFreshness, list_accounts::ListAccounts, list_categories::ListCategories,
    monthly_budget::GetMonthlyBudget, query_transactions::QueryTransactions,
    spending_by_category::SpendingByCategory,
};
use crate::store::{
    freshness::{self, Freshness},
    replica::Replica,
};

pub struct BudgetServer {
    config: Config,
    client: ActualClient,
    replica: Replica,
    /// Holds both ids: `file_id` for downloads, `group_id` for message fetches.
    file: UserFile,
    tool_router: ToolRouter<Self>,
}

impl BudgetServer {
    /// Cold start: authenticate, choose the budget, and ensure a replica exists.
    ///
    /// A warm start reuses the cached replica and never re-downloads the
    /// snapshot (FR-1.3).
    pub async fn connect(config: Config) -> Result<Self, StartupError> {
        let client = ActualClient::new(&config)?;

        let file = client.select_budget(config.sync_id.as_deref()).await?;
        // The group id must come from the file listing: a reset-clock snapshot
        // carries `groupId: null` in its own metadata.
        let group_id = file.group_id.clone().ok_or(StartupError::NoGroupId)?;
        tracing::info!(budget = %file.name, %group_id, "selected budget");

        let replica = match Replica::open(&config.cache_dir, &group_id)? {
            Some(replica) => {
                tracing::info!(path = %replica.path().display(), "reusing cached replica");
                replica
            }
            None => {
                tracing::info!("no cached replica; downloading snapshot");
                let snapshot = client.download_snapshot(&file.file_id).await?;
                Replica::install(&config.cache_dir, &group_id, &snapshot)?
            }
        };

        Ok(Self {
            config,
            client,
            replica,
            file,
            tool_router: Self::tool_router(),
        })
    }

    pub fn tool_router() -> ToolRouter<Self> {
        ToolRouter::new()
            .with_async_tool::<GetDataFreshness>()
            .with_async_tool::<SpendingByCategory>()
            .with_async_tool::<GetMonthlyBudget>()
            .with_async_tool::<ListAccounts>()
            .with_async_tool::<ListCategories>()
            .with_async_tool::<QueryTransactions>()
    }

    pub(crate) fn refresh_interval(&self) -> Duration {
        self.config.ttl
    }

    /// Run a read against the replica on the blocking pool.
    pub(crate) async fn read<T, F>(&self, f: F) -> Result<T, ToolError>
    where
        F: FnOnce(&rusqlite::Connection) -> Result<T, crate::store::error::StoreError>
            + Send
            + 'static,
        T: Send + 'static,
    {
        Ok(self.replica.read(f).await?)
    }

    pub(crate) fn freshness(&self) -> Result<Freshness, ToolError> {
        Ok(self.replica.freshness()?)
    }

    /// Pull anything new before serving a tool call (FR-3.1).
    ///
    /// A refresh failure is logged and swallowed: serving slightly stale data
    /// beats failing the call outright (FR-3.4). The staleness is still
    /// reported through `get_data_freshness`.
    pub(crate) async fn refresh_if_stale(&self) -> Result<(), ToolError> {
        let current = self.replica.freshness()?;
        if !freshness::is_stale(current.last_retrieval, SystemTime::now(), self.config.ttl) {
            return Ok(());
        }

        match self.refresh().await {
            Ok(applied) => tracing::debug!(applied, "replica refreshed"),
            Err(error) => {
                tracing::warn!(?error, "refresh failed; serving cached data")
            }
        }
        Ok(())
    }

    async fn refresh(&self) -> Result<usize, ToolError> {
        let since = self.replica.clock()?;
        let group_id = self.file.group_id.as_deref().unwrap_or_default();

        let messages = self
            .client
            .fetch_messages(&self.file.file_id, group_id, &since)
            .await?;

        // The new cursor is the newest timestamp we actually received. With no
        // messages the clock stands still, but the retrieval time still moves,
        // which is what stops the TTL from re-fetching on every call.
        let new_clock = messages
            .iter()
            .map(|m| m.timestamp.as_str())
            .max()
            .unwrap_or(&since)
            .to_string();

        Ok(self
            .replica
            .apply(&messages, &new_clock, SystemTime::now())?)
    }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for BudgetServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new(
                env!("CARGO_PKG_NAME"),
                env!("CARGO_PKG_VERSION"),
            ))
            .with_instructions(
                "Read-only access to a self-hosted Actual Budget. Amounts are in \
                 dollars; negative values are outflows. Transaction data is only as \
                 current as the last bank import, which the user performs manually — \
                 call get_data_freshness before making claims about recent activity.",
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Tool descriptions are the *only* documentation the model gets: they land
    /// verbatim in `tools/list`, and nothing else tells it how to use this
    /// server. These assertions are the authoring standard for adding a tool.
    #[test]
    fn every_tool_documents_itself() {
        let tools = BudgetServer::tool_router().list_all();
        assert_eq!(tools.len(), 6, "a tool was added or dropped from the router");

        for tool in &tools {
            let name = &tool.name;
            assert!(
                name.chars().all(|c| c.is_ascii_lowercase() || c == '_'),
                "{name} should be snake_case"
            );

            let description = tool
                .description
                .as_deref()
                .unwrap_or_else(|| panic!("{name} has no description"));

            // Short descriptions mean the model is guessing. Every existing
            // tool explains what it returns, what it excludes, or both.
            assert!(
                description.len() >= 120,
                "{name}'s description is too thin to use ({} chars)",
                description.len()
            );

            assert!(tool.input_schema.contains_key("type"), "{name} has no input schema");
            assert!(tool.output_schema.is_some(), "{name} has no output schema");
        }
    }

    /// Anything returning amounts has to say what the numbers mean, or the
    /// model will guess at the sign and the units.
    #[test]
    fn tools_returning_money_state_their_units_and_sign() {
        for tool in BudgetServer::tool_router().list_all() {
            let returns_money = matches!(
                tool.name.as_ref(),
                "spending_by_category" | "get_monthly_budget" | "list_accounts" | "query_transactions"
            );
            if !returns_money {
                continue;
            }
            let d = tool.description.as_deref().unwrap_or_default().to_lowercase();
            assert!(d.contains("dollar"), "{} must state its units", tool.name);
            assert!(
                d.contains("negative"),
                "{} must state the sign convention",
                tool.name
            );
        }
    }

    /// Spending figures are scoped: transfers and off-budget accounts are left
    /// out. A model that does not know that will present them as every dollar
    /// that moved.
    #[test]
    fn spending_tools_disclose_what_they_exclude() {
        for tool in BudgetServer::tool_router().list_all() {
            if !matches!(tool.name.as_ref(), "spending_by_category" | "get_monthly_budget") {
                continue;
            }
            let d = tool.description.as_deref().unwrap_or_default().to_lowercase();
            assert!(
                d.contains("exclud") || d.contains("off-budget"),
                "{} must say what it leaves out",
                tool.name
            );
        }
    }
}
