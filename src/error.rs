use thiserror::Error;

#[derive(Debug, Error)]
#[allow(dead_code)]
pub enum Error {
    #[error("HTTP request failed: {0}")]
    Http(#[from] reqwest::Error),

    #[error("JSON parsing failed: {0}")]
    Json(#[from] serde_json::Error),

    #[error("Zstd decompression failed: {0}")]
    Zstd(#[from] std::io::Error),

    #[error("Cargo.lock parsing failed: {0}")]
    CargoLock(#[from] cargo_lock::Error),

    #[error(
        "Rustdoc JSON not available for {crate_name} v{version}. This crate may have been published before docs.rs started generating JSON. See: https://docs.rs/{crate_name}/{version}"
    )]
    JsonNotAvailable { crate_name: String, version: String },

    #[error("Crate not found: {0}")]
    CrateNotFound(String),

    #[error("Item not found: {item_path} in {crate_name}")]
    ItemNotFound {
        crate_name: String,
        item_path: String,
    },

    #[error("{0}")]
    Other(String),
}
