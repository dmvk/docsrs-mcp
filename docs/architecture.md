# Architecture

## Overview

rust-docs-mcp is an MCP (Model Context Protocol) server that provides AI assistants with access to Rust crate documentation. It fetches rustdoc JSON from docs.rs, parses it into an in-memory index, and exposes query tools over MCP's stdio transport.

## System Diagram

```
┌──────────────┐      stdio       ┌──────────────────────┐
│  MCP Client  │ ◄──────────────► │   RustDocsServer     │
│ (e.g. Claude)│                  │                      │
└──────────────┘                  │  ┌────────────────┐  │
                                  │  │  Tool Router    │  │
                                  │  │  (4 tools)      │  │
                                  │  └───────┬────────┘  │
                                  │          │           │
                                  │  ┌───────▼────────┐  │
                                  │  │  Crate Cache    │  │
                                  │  │  HashMap<K,V>   │  │
                                  │  └───────┬────────┘  │
                                  └──────────┼──────────┘
                                             │ cache miss
                                  ┌──────────▼──────────┐
                                  │     docs::fetcher    │
                                  │  HTTP GET docs.rs    │
                                  │  zstd decompress     │
                                  │  normalize JSON      │
                                  └──────────┬──────────┘
                                             │
                                  ┌──────────▼──────────┐
                                  │     docs::parser     │
                                  │  Phase 1: index items│
                                  │  Phase 2: impl blocks│
                                  └──────────┬──────────┘
                                             │
                                  ┌──────────▼──────────┐
                                  │     docs::index      │
                                  │  CrateIndex          │
                                  │  (items, modules,    │
                                  │   impl_blocks)       │
                                  └──────────┬──────────┘
                                             │
                                  ┌──────────▼──────────┐
                                  │     docs::render     │
                                  │  Markdown output     │
                                  └─────────────────────┘
```

## Module Responsibilities

### `main.rs`
Entry point. Initializes `tracing` (to stderr, since stdout is the MCP transport), discovers and parses `Cargo.lock` from CWD for version auto-resolution, then starts the MCP server on stdio.

### `server.rs`
Implements `ServerHandler` for `RustDocsServer`. Contains:
- 4 tool parameter structs with `JsonSchema` derives for MCP schema generation
- Tool implementations that resolve versions, load/cache crate indices, and render results
- `resolve_version()`: explicit > Cargo.lock > "latest"
- `get_or_load_index()`: double-check locking cache pattern with `Arc<RwLock<HashMap>>`

### `cargo_lock.rs`
`CargoLockIndex` walks up from CWD to find `Cargo.lock`, parses it, and builds a `HashMap<crate_name, version>`. When multiple versions of the same crate exist, keeps the latest.

### `docs/fetcher.rs`
Fetches zstd-compressed rustdoc JSON from `https://docs.rs/crate/{name}/{version}/json`. The critical complexity here is **format version normalization**:

| Format Version | Change | Normalization |
|---------------|--------|---------------|
| 53 → 54 | `attrs` changed from `Vec<String>` to tagged enum | Strip all attrs arrays |
| 55 → 56 | `Crate.target` field added | Inject dummy target for older formats |
| 56 → 57 | `ExternalCrate.path` field added | Strip path from external_crates for 57+ |

The normalizer ensures any format version (53–57+) deserializes correctly with `rustdoc-types` 0.56.

### `docs/parser.rs`
Two-phase conversion of `rustdoc_types::Crate` into `CrateIndex`:
1. **Phase 1**: Iterate all items in `krate.index`, resolve paths via `krate.paths`, build module hierarchy
2. **Phase 2**: Process all `Impl` items, attach methods to their implementing types

Contains extensive type signature rendering (~500 lines): structs, enums, traits, functions, unions, generics, where clauses, and all Rust type forms (references, slices, arrays, function pointers, dyn traits, impl traits, qualified paths).

### `docs/index.rs`
`CrateIndex` stores:
- `items: HashMap<path, IndexedItem>` — all documented items
- `modules: HashMap<path, Vec<child_paths>>` — module hierarchy
- `impl_blocks: HashMap<type_path, Vec<ImplBlock>>` — implementations per type
- `root_items: Vec<path>` — top-level crate items

Provides search (ranked by: exact > prefix > name contains > path contains > doc contains) and Levenshtein-based suggestions for typos.

### `docs/render.rs`
Converts indexed data structures into markdown text for MCP tool responses. Each tool has a corresponding render function.

## Concurrency Model

The server uses `Arc<RwLock<HashMap>>` for caching. Multiple concurrent tool calls can read the cache simultaneously (read lock). On cache miss, a write lock is acquired after fetching, with a re-check to avoid duplicate work if another task populated the cache while fetching.

## Error Handling

All errors flow through `error::Error` (thiserror). Tool methods catch errors and return them as `CallToolResult::error()` text responses rather than failing the MCP connection.
