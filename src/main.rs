use actual_mcp::{config::Config, mcp::server::BudgetServer};
use rmcp::ServiceExt;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // stdout is the JSON-RPC channel; everything else goes to stderr.
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .with_writer(std::io::stderr)
        .with_ansi(false)
        .init();

    let config = Config::from_env()?;
    let server = BudgetServer::connect(config).await?;

    let transport = (tokio::io::stdin(), tokio::io::stdout());
    let running = server.serve(transport).await?;
    running.waiting().await?;
    Ok(())
}
