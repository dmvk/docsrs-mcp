# rust-docs-mcp

[![CI](https://github.com/dmvk/rust-docs-mcp/actions/workflows/ci.yml/badge.svg)](https://github.com/dmvk/rust-docs-mcp/actions/workflows/ci.yml)
[![License: Apache-2.0](https://img.shields.io/badge/License-Apache_2.0-blue.svg)](LICENSE)
[![Rust: 1.93+](https://img.shields.io/badge/Rust-1.93%2B-orange.svg)](https://www.rust-lang.org)

An MCP server that gives AI assistants access to Rust crate documentation from [docs.rs](https://docs.rs). Point it at any crate and get full API docs, type signatures, trait impls, and search — all through the [Model Context Protocol](https://modelcontextprotocol.io).

## Features

- **Browse crate structure** — list modules, types, functions, and other items in any crate
- **Read detailed docs** — get full documentation, type signatures, struct fields, and enum variants
- **Search across items** — full-text search over item names and doc comments with fuzzy matching
- **Explore implementations** — look up trait implementations and inherent methods for any type
- **Automatic version resolution** — detects versions from your project's `Cargo.lock`, or falls back to latest
- **Wide format compatibility** — handles rustdoc JSON format versions 53–57+ via automatic normalization

## Tools

| Tool | Description |
|------|-------------|
| `lookup_crate_items` | List items in a crate or module — use this to explore crate structure |
| `lookup_item` | Get detailed docs for a specific item including signature, fields, and methods |
| `search_crate` | Full-text search across item names and documentation |
| `lookup_impl_block` | Look up trait implementations and inherent methods for a type |

All tools accept `crate_name` (required) and `version` (optional, auto-resolved).

## Installation

Build from source:

```bash
git clone https://github.com/dmvk/rust-docs-mcp.git
cd rust-docs-mcp
cargo build --release
```

The binary will be at `target/release/rust-docs-mcp`.

## Configuration

### Claude Desktop

Add to your Claude Desktop config (`~/Library/Application Support/Claude/claude_desktop_config.json` on macOS):

```json
{
  "mcpServers": {
    "rust-docs": {
      "command": "/path/to/rust-docs-mcp"
    }
  }
}
```

### Claude Code

```bash
claude mcp add rust-docs /path/to/rust-docs-mcp
```

### Generic MCP clients

The server uses **stdio transport** — launch the binary and communicate over stdin/stdout. Logs go to stderr.

### Environment variables

| Variable | Description |
|----------|-------------|
| `RUST_LOG` | Log level filter (e.g. `debug`, `info`, `trace`). Default: `info` |

## Version resolution

When you call a tool without specifying a version, the server resolves it automatically:

1. **Explicit version** — if you pass `version`, that's used as-is
2. **Cargo.lock** — the server looks for `Cargo.lock` in the working directory (and parent directories) and uses the version found there
3. **Latest** — if no version is found, fetches the latest version from docs.rs

This means if you run the server from your project directory, it automatically uses the same crate versions your project depends on.

## Usage examples

> "What types does the `serde` crate export?"

Calls `lookup_crate_items` with `crate_name: "serde"` to list top-level items.

> "Show me the docs for `tokio::sync::Mutex`"

Calls `lookup_item` with `crate_name: "tokio"` and `item_path: "sync::Mutex"`.

> "Search `reqwest` for anything related to cookies"

Calls `search_crate` with `crate_name: "reqwest"` and `query: "cookies"`.

> "What traits does `Vec` implement?"

Calls `lookup_impl_block` with `crate_name: "std"` and `item_path: "vec::Vec"`.

## Architecture

```
docs.rs ──HTTP──► fetcher ──► parser ──► index ──► render ──► MCP response
                  (zstd)      (items)    (search)  (markdown)
```

See [docs/architecture.md](docs/architecture.md) for a detailed breakdown.

## Contributing

1. Fork the repository
2. Create a feature branch
3. Make sure your changes pass:
   ```bash
   cargo fmt --all --check
   cargo clippy --all-targets
   cargo test
   ```
4. Open a pull request

## License

Apache-2.0 — see [LICENSE](LICENSE) for details.
