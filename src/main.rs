mod cargo_lock;
mod docs;
mod error;
mod server;

use rmcp::ServiceExt;
use rmcp::transport::stdio;

use crate::cargo_lock::CargoLockIndex;
use crate::docs::cache::DiskCache;
use crate::server::RustDocsServer;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Initialize tracing to stderr (stdout is used for MCP stdio transport)
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_writer(std::io::stderr)
        .init();

    // Parse CLI flags
    let args: Vec<String> = std::env::args().collect();
    let no_cache = args.iter().any(|a| a == "--no-cache");
    let clear_cache = args.iter().any(|a| a == "--clear-cache");

    if clear_cache {
        DiskCache::clear().await;
    }

    // Find and parse Cargo.lock from CWD
    let cwd = std::env::current_dir()?;
    let cargo_lock = CargoLockIndex::find_and_parse(&cwd);
    if cargo_lock.is_some() {
        tracing::info!("Cargo.lock loaded, will auto-resolve crate versions");
    } else {
        tracing::info!("No Cargo.lock found, will use explicit versions or 'latest'");
    }

    let server = RustDocsServer::new(cargo_lock, !no_cache);

    let service = server.serve(stdio()).await.inspect_err(|e| {
        tracing::error!("Failed to start MCP server: {e}");
    })?;

    service.waiting().await?;

    Ok(())
}
