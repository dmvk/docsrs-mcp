use std::collections::HashMap;
use std::sync::Arc;

use rmcp::handler::server::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::*;
use rmcp::schemars;
use rmcp::schemars::JsonSchema;
use rmcp::{ServerHandler, tool, tool_handler, tool_router};
use serde::Deserialize;
use tokio::sync::RwLock;

use crate::cargo_lock::CargoLockIndex;
use crate::docs::fetcher::fetch_rustdoc_json;
use crate::docs::index::CrateIndex;
use crate::docs::parser::parse_crate;
use crate::docs::render;

type CrateCache = Arc<RwLock<HashMap<(String, String), Arc<CrateIndex>>>>;

#[derive(Clone)]
pub struct RustDocsServer {
    cargo_lock: Option<Arc<CargoLockIndex>>,
    http_client: reqwest::Client,
    cache: CrateCache,
    tool_router: ToolRouter<Self>,
}

// ========== Tool parameter structs ==========

#[derive(Debug, Deserialize, JsonSchema)]
struct LookupCrateItemsParams {
    /// The crate name (e.g. "serde", "tokio")
    crate_name: String,
    /// Specific version. Auto-detected from Cargo.lock if omitted, falls back to "latest".
    #[serde(default)]
    version: Option<String>,
    /// Module path to list items from (e.g. "tokio::sync"). Lists root items if omitted.
    #[serde(default)]
    module_path: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct LookupItemParams {
    /// The crate name (e.g. "serde", "tokio")
    crate_name: String,
    /// Fully qualified path to the item (e.g. "Serialize", "sync::Mutex")
    item_path: String,
    /// Specific version. Auto-detected from Cargo.lock if omitted, falls back to "latest".
    #[serde(default)]
    version: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct SearchCrateParams {
    /// The crate name to search in
    crate_name: String,
    /// Search query (matches against item names and doc text)
    query: String,
    /// Specific version. Auto-detected from Cargo.lock if omitted, falls back to "latest".
    #[serde(default)]
    version: Option<String>,
    /// Maximum number of results (default: 20)
    #[serde(default)]
    limit: Option<usize>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct LookupImplBlockParams {
    /// The crate name
    crate_name: String,
    /// Path to the type or trait (e.g. "HashMap", "sync::Mutex")
    item_path: String,
    /// Specific version. Auto-detected from Cargo.lock if omitted, falls back to "latest".
    #[serde(default)]
    version: Option<String>,
}

// ========== Server implementation ==========

#[tool_router]
impl RustDocsServer {
    pub fn new(cargo_lock: Option<CargoLockIndex>) -> Self {
        Self {
            cargo_lock: cargo_lock.map(Arc::new),
            http_client: reqwest::Client::builder()
                .user_agent("rust-docs-mcp/0.1.0")
                .build()
                .expect("failed to build HTTP client"),
            cache: Arc::new(RwLock::new(HashMap::new())),
            tool_router: Self::tool_router(),
        }
    }

    #[tool(
        name = "lookup_crate_items",
        description = "List items (modules, structs, enums, traits, functions) in a Rust crate or module. Use this to explore the structure of a crate."
    )]
    async fn lookup_crate_items(
        &self,
        Parameters(params): Parameters<LookupCrateItemsParams>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        let version = self.resolve_version(&params.crate_name, params.version.as_deref());
        match self.get_or_load_index(&params.crate_name, &version).await {
            Ok(index) => {
                let module = params.module_path.as_deref().map(|p| {
                    if p.contains("::") {
                        p.to_string()
                    } else {
                        format!("{}::{p}", index.crate_name)
                    }
                });
                let text = render::render_crate_items(&index, module.as_deref());
                Ok(CallToolResult::success(vec![Content::text(text)]))
            }
            Err(e) => Ok(CallToolResult::error(vec![Content::text(e.to_string())])),
        }
    }

    #[tool(
        name = "lookup_item",
        description = "Get detailed documentation for a specific Rust item (struct, enum, trait, function, etc.) including its signature, fields, methods, and doc comments."
    )]
    async fn lookup_item(
        &self,
        Parameters(params): Parameters<LookupItemParams>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        let version = self.resolve_version(&params.crate_name, params.version.as_deref());
        match self.get_or_load_index(&params.crate_name, &version).await {
            Ok(index) => {
                let text = if let Some(item) = index.get_item(&params.item_path) {
                    render::render_item(item)
                } else {
                    render::render_not_found(&index, &params.item_path)
                };
                Ok(CallToolResult::success(vec![Content::text(text)]))
            }
            Err(e) => Ok(CallToolResult::error(vec![Content::text(e.to_string())])),
        }
    }

    #[tool(
        name = "search_crate",
        description = "Search within a Rust crate for items matching a query. Searches item names and documentation text. Returns ranked results."
    )]
    async fn search_crate(
        &self,
        Parameters(params): Parameters<SearchCrateParams>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        let version = self.resolve_version(&params.crate_name, params.version.as_deref());
        let limit = params.limit.unwrap_or(20).min(50);
        match self.get_or_load_index(&params.crate_name, &version).await {
            Ok(index) => {
                let results = index.search(&params.query, limit);
                let text = render::render_search_results(&index, &params.query, &results);
                Ok(CallToolResult::success(vec![Content::text(text)]))
            }
            Err(e) => Ok(CallToolResult::error(vec![Content::text(e.to_string())])),
        }
    }

    #[tool(
        name = "lookup_impl_block",
        description = "Look up trait implementations for a type, or implementors of a trait. Shows method signatures and documentation."
    )]
    async fn lookup_impl_block(
        &self,
        Parameters(params): Parameters<LookupImplBlockParams>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        let version = self.resolve_version(&params.crate_name, params.version.as_deref());
        match self.get_or_load_index(&params.crate_name, &version).await {
            Ok(index) => {
                let impls = index.get_impl_blocks(&params.item_path);
                let text = render::render_impls(&params.item_path, &impls);
                Ok(CallToolResult::success(vec![Content::text(text)]))
            }
            Err(e) => Ok(CallToolResult::error(vec![Content::text(e.to_string())])),
        }
    }
}

#[tool_handler]
impl ServerHandler for RustDocsServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo {
            instructions: Some(
                "Rust documentation server. Fetches and serves crate documentation from docs.rs. \
                 Use lookup_crate_items to explore crate structure, lookup_item for detailed docs, \
                 search_crate to find items, and lookup_impl_block for implementations."
                    .into(),
            ),
            capabilities: ServerCapabilities::builder().enable_tools().build(),
            ..Default::default()
        }
    }
}

impl RustDocsServer {
    /// Resolve the version to use: explicit > Cargo.lock > "latest"
    fn resolve_version(&self, crate_name: &str, explicit: Option<&str>) -> String {
        if let Some(v) = explicit {
            return v.to_string();
        }
        if let Some(ref lock) = self.cargo_lock {
            if let Some(v) = lock.get_version(crate_name) {
                tracing::debug!("Resolved {crate_name} version from Cargo.lock: {v}");
                return v.to_string();
            }
        }
        "latest".to_string()
    }

    /// Get a cached CrateIndex or fetch/parse/cache a new one.
    async fn get_or_load_index(
        &self,
        crate_name: &str,
        version: &str,
    ) -> Result<Arc<CrateIndex>, crate::error::Error> {
        let key = (crate_name.to_string(), version.to_string());

        // Fast path: read lock
        {
            let cache = self.cache.read().await;
            if let Some(index) = cache.get(&key) {
                return Ok(Arc::clone(index));
            }
        }

        // Slow path: fetch, parse, then write lock
        tracing::info!("Loading {crate_name} v{version} from docs.rs...");
        let krate = fetch_rustdoc_json(&self.http_client, crate_name, version).await?;

        // Normalize crate name (hyphens -> underscores in rustdoc)
        let normalized_name = crate_name.replace('-', "_");
        let index = Arc::new(parse_crate(&krate, &normalized_name, version));

        // Double-check locking: someone else may have populated while we fetched
        let mut cache = self.cache.write().await;
        cache.entry(key).or_insert_with(|| Arc::clone(&index));

        Ok(index)
    }
}
