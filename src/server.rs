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
use crate::docs::cache::DiskCache;
use crate::docs::fetcher::{decode_raw_bytes, fetch_raw_bytes};
use crate::docs::index::CrateIndex;
use crate::docs::parser::parse_crate;
use crate::docs::render;

type CrateCache = Arc<RwLock<HashMap<(String, String), Arc<CrateIndex>>>>;

#[derive(Clone)]
pub struct RustDocsServer {
    cargo_lock: Option<Arc<CargoLockIndex>>,
    http_client: reqwest::Client,
    cache: CrateCache,
    disk_cache: Option<Arc<DiskCache>>,
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
    pub fn new(cargo_lock: Option<CargoLockIndex>, use_disk_cache: bool) -> Self {
        let disk_cache = if use_disk_cache {
            DiskCache::new().map(Arc::new)
        } else {
            None
        };

        match &disk_cache {
            Some(_) => tracing::info!("Disk cache enabled"),
            None if use_disk_cache => {
                tracing::warn!("Could not determine cache directory, disk cache disabled");
            }
            None => tracing::info!("Disk cache disabled"),
        }

        Self {
            cargo_lock: cargo_lock.map(Arc::new),
            http_client: reqwest::Client::builder()
                .user_agent("rust-docs-mcp/0.1.0")
                .build()
                .expect("failed to build HTTP client"),
            cache: Arc::new(RwLock::new(HashMap::new())),
            disk_cache,
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
        if let Some(ref lock) = self.cargo_lock
            && let Some(v) = lock.get_version(crate_name)
        {
            tracing::debug!("Resolved {crate_name} version from Cargo.lock: {v}");
            return v.to_string();
        }
        "latest".to_string()
    }

    /// Get a cached CrateIndex or fetch/parse/cache a new one.
    ///
    /// Cache layers (checked in order):
    /// 1. In-memory `CrateCache` (fast path)
    /// 2. On-disk cache of raw zstd bytes (skipped for "latest")
    /// 3. HTTP fetch from docs.rs (writes to disk cache for pinned versions)
    async fn get_or_load_index(
        &self,
        crate_name: &str,
        version: &str,
    ) -> Result<Arc<CrateIndex>, crate::error::Error> {
        let key = (crate_name.to_string(), version.to_string());

        // Fast path: in-memory cache read lock
        {
            let cache = self.cache.read().await;
            if let Some(index) = cache.get(&key) {
                return Ok(Arc::clone(index));
            }
        }

        // Disk cache is only used for pinned (non-"latest") versions
        let disk = self.disk_cache.as_ref().filter(|_| version != "latest");
        let krate = self.fetch_crate(disk, crate_name, version).await?;

        // Normalize crate name (hyphens -> underscores in rustdoc)
        let normalized_name = crate_name.replace('-', "_");
        let index = Arc::new(parse_crate(&krate, &normalized_name, version));

        // Double-check locking: someone else may have populated while we fetched
        let mut cache = self.cache.write().await;
        cache.entry(key).or_insert_with(|| Arc::clone(&index));

        Ok(index)
    }

    /// Fetch and decode rustdoc JSON, using the disk cache when available.
    ///
    /// On disk cache hit, decodes directly. On miss or corruption, fetches from
    /// docs.rs and writes through to the disk cache for future use.
    async fn fetch_crate(
        &self,
        disk: Option<&Arc<DiskCache>>,
        crate_name: &str,
        version: &str,
    ) -> Result<rustdoc_types::Crate, crate::error::Error> {
        if let Some(disk) = disk
            && let Some(bytes) = disk.read(crate_name, version).await
        {
            match decode_raw_bytes(&bytes, crate_name, version) {
                Ok(krate) => return Ok(krate),
                Err(e) => {
                    tracing::warn!(
                        "Corrupted cache entry for {crate_name} v{version}, \
                         removing and fetching from network: {e}"
                    );
                    disk.remove(crate_name, version).await;
                }
            }
        }

        tracing::info!("Loading {crate_name} v{version} from docs.rs...");
        let bytes = fetch_raw_bytes(&self.http_client, crate_name, version).await?;

        if let Some(disk) = disk {
            disk.write(crate_name, version, &bytes).await;
        }

        decode_raw_bytes(&bytes, crate_name, version)
    }
}
