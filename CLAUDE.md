# docsrs-mcp

MCP server that fetches and serves Rust crate documentation from docs.rs. Exposes 4 tools for exploring crate APIs via the Model Context Protocol.

## Quick Reference

```bash
cargo build                    # Build the project
cargo test                     # Run unit tests
cargo clippy                   # Lint
cargo run                      # Run MCP server (uses stdio transport, logs to stderr)
cargo run -- --no-cache        # Run without disk cache
cargo run -- --clear-cache     # Clear disk cache, then run
RUST_LOG=debug cargo run       # Run with debug logging
```

## Architecture

```
main.rs           Entry point: loads Cargo.lock, starts MCP stdio server
server.rs         MCP tool handler (4 tools), in-memory crate cache (Arc<RwLock<HashMap>>)
cargo_lock.rs     Parses Cargo.lock for automatic version resolution
docs/
  cache.rs        On-disk cache for raw zstd bytes (DiskCache, path sanitization)
  fetcher.rs      Fetches zstd-compressed rustdoc JSON from docs.rs, normalizes format versions
  parser.rs       Converts rustdoc_types::Crate into CrateIndex (two-phase: items, then impls)
  index.rs        In-memory search index: CrateIndex, IndexedItem, ImplBlock, Levenshtein search
  render.rs       Renders indexed items to markdown for tool responses
error.rs          Error types (thiserror)
```

**Data flow:** tool call → in-memory cache → disk cache → HTTP fetch (docs.rs) → zstd decompress → normalize JSON → parse to CrateIndex → render to markdown

**Version resolution order:** explicit param > Cargo.lock > "latest"

## MCP Tools

| Tool | Purpose |
|------|---------|
| `lookup_crate_items` | List items in a crate or module (explore structure) |
| `lookup_item` | Get detailed docs for a specific item (signature, fields, methods) |
| `search_crate` | Full-text search across item names and docs |
| `lookup_impl_block` | Look up trait implementations and inherent methods |

All tools accept `crate_name` (required) and `version` (optional, auto-resolved).

## Conventions

- Rust 2024 edition
- Error handling: `thiserror` derive macros, `Error` enum in `error.rs`
- Logging: `tracing` crate — `info` for major operations, `debug` for version resolution, `trace` for skipped items
- Async: `tokio` runtime, `reqwest` for HTTP
- Tool definitions: `#[tool]` macro from `rmcp` on `RustDocsServer` impl methods
- Adding a new tool: create params struct with `Deserialize + JsonSchema`, add `#[tool]` method to `RustDocsServer`

## Workflow

- Use Plan mode for multi-file changes or new tools
- Run `cargo build` after changes to verify compilation
- Run `cargo test` to validate normalization logic
- Run `cargo fmt` and `cargo clippy --all-targets` before committing

## Gotchas

- **Rustdoc JSON format versions**: docs.rs serves formats v53–v57+ depending on when a crate was built. `fetcher.rs::normalize_for_v56()` patches older/newer JSON to match `rustdoc-types` 0.56. When updating `rustdoc-types`, this normalization must be revisited.
- **Crate name normalization**: Rust crate names use hyphens (`my-crate`) but rustdoc paths use underscores (`my_crate`). `server.rs::get_or_load_index()` does `replace('-', "_")`.
- **Cache key**: `(crate_name, version)` tuple. In-memory cache has no TTL or eviction. Disk cache stores raw zstd bytes at `{platform_cache_dir}/docsrs-mcp/{crate}/{version}.json.zst`. `"latest"` versions skip the disk cache (only pinned versions are cached on disk). Path sanitization in `cache_path()` prevents directory traversal.
- **Double-check locking**: `get_or_load_index` uses read lock fast path, then write lock slow path with re-check to avoid duplicate fetches under concurrency.

## Learnings

<!-- Add lessons learned from development and PR reviews here -->

## Deep Dive (read on demand)

- [Architecture details](docs/architecture.md)
