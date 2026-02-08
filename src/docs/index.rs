use std::collections::HashMap;

/// In-memory indexed representation of a crate's documentation.
/// All signatures are pre-rendered to strings during parsing, so the
/// original rustdoc_types::Crate is dropped after index construction.
pub struct CrateIndex {
    pub crate_name: String,
    pub version: String,
    /// All indexed items, keyed by their fully qualified path (e.g. "serde::Serialize").
    pub items: HashMap<String, IndexedItem>,
    /// Module hierarchy: module path → list of child item paths.
    pub modules: HashMap<String, Vec<String>>,
    /// Impl blocks: type path → list of impl blocks.
    pub impl_blocks: HashMap<String, Vec<ImplBlock>>,
    /// Root module items (items at the crate root).
    pub root_items: Vec<String>,
}

/// A single documented item in the crate.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct IndexedItem {
    /// Fully qualified path (e.g. "serde::Serialize").
    pub path: String,
    /// The item's simple name (e.g. "Serialize").
    pub name: String,
    /// What kind of item this is.
    pub kind: ItemKind,
    /// The rendered signature (e.g. `pub trait Serialize { ... }`).
    pub signature: String,
    /// The short one-line doc summary.
    pub short_doc: String,
    /// Full documentation text.
    pub doc: String,
    /// Kind-specific detail (struct fields, enum variants, trait methods, etc.)
    pub detail: ItemDetail,
    /// The parent module path (empty string for root items).
    pub parent_module: String,
}

/// The kind of a documented item.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ItemKind {
    Module,
    Struct,
    Enum,
    Trait,
    Function,
    TypeAlias,
    Constant,
    Static,
    Macro,
    Union,
}

impl std::fmt::Display for ItemKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ItemKind::Module => write!(f, "mod"),
            ItemKind::Struct => write!(f, "struct"),
            ItemKind::Enum => write!(f, "enum"),
            ItemKind::Trait => write!(f, "trait"),
            ItemKind::Function => write!(f, "fn"),
            ItemKind::TypeAlias => write!(f, "type"),
            ItemKind::Constant => write!(f, "const"),
            ItemKind::Static => write!(f, "static"),
            ItemKind::Macro => write!(f, "macro"),
            ItemKind::Union => write!(f, "union"),
        }
    }
}

/// Kind-specific detail for an item.
#[derive(Debug, Clone, Default)]
#[allow(dead_code)]
pub struct ItemDetail {
    /// For structs: list of fields as rendered strings.
    pub fields: Vec<FieldInfo>,
    /// For enums: list of variants.
    pub variants: Vec<VariantInfo>,
    /// For traits: list of required/provided methods.
    pub methods: Vec<MethodInfo>,
    /// For structs/enums: whether it derives common traits.
    pub derives: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct FieldInfo {
    pub name: String,
    pub type_str: String,
    pub doc: String,
}

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct VariantInfo {
    pub name: String,
    pub signature: String,
    pub doc: String,
}

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct MethodInfo {
    pub name: String,
    pub signature: String,
    pub doc: String,
    pub is_required: bool,
}

/// An impl block associated with a type.
#[derive(Debug, Clone)]
pub struct ImplBlock {
    /// e.g. "impl Serialize for MyStruct" or "impl MyStruct"
    pub header: String,
    /// Trait being implemented, if any.
    pub trait_name: Option<String>,
    /// Methods in this impl block.
    pub methods: Vec<MethodInfo>,
}

/// Result of a search query.
pub struct SearchResult {
    pub item: IndexedItem,
    pub score: SearchScore,
}

/// How well a search result matches.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum SearchScore {
    /// Query exactly matches the item name.
    Exact = 4,
    /// Item name starts with the query.
    Prefix = 3,
    /// Item name contains the query.
    NameContains = 2,
    /// The item path contains the query.
    PathContains = 1,
    /// The doc text contains the query.
    DocContains = 0,
}

impl CrateIndex {
    /// Search within the crate for items matching the query.
    pub fn search(&self, query: &str, limit: usize) -> Vec<SearchResult> {
        let query_lower = query.to_lowercase();
        let mut results: Vec<SearchResult> = self
            .items
            .values()
            .filter_map(|item| {
                let name_lower = item.name.to_lowercase();
                let path_lower = item.path.to_lowercase();
                let doc_lower = item.doc.to_lowercase();

                let score = if name_lower == query_lower {
                    SearchScore::Exact
                } else if name_lower.starts_with(&query_lower) {
                    SearchScore::Prefix
                } else if name_lower.contains(&query_lower) {
                    SearchScore::NameContains
                } else if path_lower.contains(&query_lower) {
                    SearchScore::PathContains
                } else if doc_lower.contains(&query_lower) {
                    SearchScore::DocContains
                } else {
                    return None;
                };

                Some(SearchResult {
                    item: item.clone(),
                    score,
                })
            })
            .collect();

        // Sort by score (highest first), then alphabetically by path
        results.sort_by(|a, b| {
            b.score
                .cmp(&a.score)
                .then_with(|| a.item.path.cmp(&b.item.path))
        });

        results.truncate(limit);
        results
    }

    /// Find items in a module (or root if module_path is None).
    pub fn get_module_items(&self, module_path: Option<&str>) -> Vec<&IndexedItem> {
        let children = match module_path {
            Some(path) => self.modules.get(path),
            None => Some(&self.root_items),
        };

        let Some(children) = children else {
            return Vec::new();
        };

        let mut items: Vec<&IndexedItem> = children
            .iter()
            .filter_map(|path| self.items.get(path))
            .collect();

        // Sort: modules first, then by kind, then by name
        items.sort_by(|a, b| {
            let a_is_mod = a.kind == ItemKind::Module;
            let b_is_mod = b.kind == ItemKind::Module;
            b_is_mod
                .cmp(&a_is_mod)
                .then_with(|| a.kind.to_string().cmp(&b.kind.to_string()))
                .then_with(|| a.name.cmp(&b.name))
        });

        items
    }

    /// Look up a specific item by path.
    pub fn get_item(&self, item_path: &str) -> Option<&IndexedItem> {
        // Try exact match first
        if let Some(item) = self.items.get(item_path) {
            return Some(item);
        }
        // Try with crate name prefix
        let full_path = format!("{}::{}", self.crate_name, item_path);
        self.items.get(&full_path)
    }

    /// Get impl blocks for a type.
    pub fn get_impl_blocks(&self, item_path: &str) -> Vec<&ImplBlock> {
        let mut result = Vec::new();
        if let Some(impls) = self.impl_blocks.get(item_path) {
            result.extend(impls.iter());
        }
        // Also try with crate name prefix
        let full_path = format!("{}::{}", self.crate_name, item_path);
        if let Some(impls) = self.impl_blocks.get(&full_path) {
            result.extend(impls.iter());
        }
        result
    }

    /// Suggest similar item paths using Levenshtein distance.
    pub fn suggest_similar(&self, query: &str, max_suggestions: usize) -> Vec<String> {
        let query_lower = query.to_lowercase();
        let mut scored: Vec<(String, usize)> = self
            .items
            .keys()
            .map(|path| {
                // Compare against both the full path and just the item name
                let name = path.rsplit("::").next().unwrap_or(path);
                let name_lower = name.to_lowercase();
                let path_lower = path.to_lowercase();
                let d1 = levenshtein(&query_lower, &name_lower);
                let d2 = levenshtein(&query_lower, &path_lower);
                (path.clone(), d1.min(d2))
            })
            .collect();

        scored.sort_by_key(|(_, d)| *d);
        scored.truncate(max_suggestions);

        // Only suggest if distance is reasonable (< half the query length + 3)
        let threshold = query.len() / 2 + 3;
        scored
            .into_iter()
            .filter(|(_, d)| *d <= threshold)
            .map(|(path, _)| path)
            .collect()
    }
}

/// Simple Levenshtein distance implementation.
fn levenshtein(a: &str, b: &str) -> usize {
    let a_len = a.len();
    let b_len = b.len();

    if a_len == 0 {
        return b_len;
    }
    if b_len == 0 {
        return a_len;
    }

    let mut prev: Vec<usize> = (0..=b_len).collect();
    let mut curr = vec![0; b_len + 1];

    for (i, a_ch) in a.chars().enumerate() {
        curr[0] = i + 1;
        for (j, b_ch) in b.chars().enumerate() {
            let cost = if a_ch == b_ch { 0 } else { 1 };
            curr[j + 1] = (prev[j + 1] + 1).min(curr[j] + 1).min(prev[j] + cost);
        }
        std::mem::swap(&mut prev, &mut curr);
    }

    prev[b_len]
}
