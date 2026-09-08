use std::{borrow::Cow, sync::Arc};

use rmcp::{
    handler::server::router::tool::{AsyncTool, ToolBase},
    model::JsonObject,
    schemars,
};
use serde::{Deserialize, Serialize};

use crate::{error::ToolError, mcp::server::BudgetServer, queries::categories};

pub struct ListCategories;

#[derive(Debug, Default, Deserialize, schemars::JsonSchema)]
pub struct Input {
    /// Include categories the user has hidden, usually ones no longer in use.
    /// Historical transactions may still reference them. Defaults to false.
    #[serde(default)]
    pub include_hidden: bool,
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct Output {
    /// In budget order: groups as the user arranged them, categories in their
    /// order within each group.
    pub categories: Vec<categories::CategoryInfo>,
}

impl ToolBase for ListCategories {
    type Parameter = Input;
    type Output = Output;
    type Error = ToolError;

    fn name() -> Cow<'static, str> {
        "list_categories".into()
    }

    fn description() -> Option<Cow<'static, str>> {
        Some(
            "Lists the budget's categories and the groups they belong to. Useful for \
             discovering the exact category names that other tools accept. Income \
             categories record money coming in and are never budgeted against."
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

impl AsyncTool<BudgetServer> for ListCategories {
    async fn invoke(
        service: &BudgetServer,
        param: Self::Parameter,
    ) -> Result<Self::Output, Self::Error> {
        service.refresh_if_stale().await?;

        let include_hidden = param.include_hidden;
        let categories = service
            .read(move |conn| Ok(categories::list(conn, include_hidden)?))
            .await?;

        Ok(Output { categories })
    }
}
