use super::index::{CrateIndex, ImplBlock, IndexedItem, ItemKind, SearchResult};

/// Render a module listing (for `lookup_crate_items`).
pub fn render_crate_items(index: &CrateIndex, module_path: Option<&str>) -> String {
    let items = index.get_module_items(module_path);

    let header = match module_path {
        Some(path) => format!("## {path}\n"),
        None => format!("## {} v{}\n", index.crate_name, index.version),
    };

    if items.is_empty() {
        let suggestion = match module_path {
            Some(path) => {
                let suggestions = index.suggest_similar(path, 5);
                if suggestions.is_empty() {
                    String::new()
                } else {
                    format!("\nDid you mean one of: {}?", suggestions.join(", "))
                }
            }
            None => String::new(),
        };
        return format!("{header}\nNo items found.{suggestion}");
    }

    let mut sections: Vec<String> = Vec::new();
    let mut current_kind: Option<ItemKind> = None;

    for item in &items {
        if current_kind.as_ref() != Some(&item.kind) {
            current_kind = Some(item.kind.clone());
            sections.push(format!("\n### {}s\n", kind_label(&item.kind)));
        }

        let doc_suffix = if item.short_doc.is_empty() {
            String::new()
        } else {
            format!(" — {}", item.short_doc)
        };

        sections.push(format!("- `{}`{doc_suffix}", item.name));
    }

    format!("{header}{}", sections.join("\n"))
}

/// Render detailed info for a single item (for `lookup_item`).
pub fn render_item(item: &IndexedItem) -> String {
    let mut parts = Vec::new();

    // Header
    parts.push(format!("## {}\n", item.path));

    // Signature
    parts.push(format!("```rust\n{}\n```\n", item.signature));

    // Documentation
    if !item.doc.is_empty() {
        parts.push(item.doc.clone());
        parts.push(String::new());
    }

    // Kind-specific details
    match item.kind {
        ItemKind::Struct | ItemKind::Union => {
            if !item.detail.fields.is_empty() {
                parts.push("### Fields\n".to_string());
                for f in &item.detail.fields {
                    let doc = if f.doc.is_empty() {
                        String::new()
                    } else {
                        format!(" — {}", first_line(&f.doc))
                    };
                    parts.push(format!("- `{}`: `{}`{doc}", f.name, f.type_str));
                }
                parts.push(String::new());
            }
        }
        ItemKind::Enum => {
            if !item.detail.variants.is_empty() {
                parts.push("### Variants\n".to_string());
                for v in &item.detail.variants {
                    let doc = if v.doc.is_empty() {
                        String::new()
                    } else {
                        format!("\n  {}", first_line(&v.doc))
                    };
                    parts.push(format!("- `{}`{doc}", v.name));
                }
                parts.push(String::new());
            }
        }
        ItemKind::Trait => {
            let required: Vec<_> = item.detail.methods.iter().filter(|m| m.is_required).collect();
            let provided: Vec<_> = item.detail.methods.iter().filter(|m| !m.is_required).collect();

            if !required.is_empty() {
                parts.push("### Required Methods\n".to_string());
                for m in &required {
                    parts.push(format!("- `{}`", m.signature));
                    if !m.doc.is_empty() {
                        parts.push(format!("  {}\n", first_line(&m.doc)));
                    }
                }
                parts.push(String::new());
            }

            if !provided.is_empty() {
                parts.push("### Provided Methods\n".to_string());
                for m in &provided {
                    parts.push(format!("- `{}`", m.signature));
                    if !m.doc.is_empty() {
                        parts.push(format!("  {}\n", first_line(&m.doc)));
                    }
                }
                parts.push(String::new());
            }
        }
        _ => {}
    }

    parts.join("\n")
}

/// Render search results (for `search_crate`).
pub fn render_search_results(
    index: &CrateIndex,
    query: &str,
    results: &[SearchResult],
) -> String {
    if results.is_empty() {
        let suggestions = index.suggest_similar(query, 5);
        let suggestion_text = if suggestions.is_empty() {
            String::new()
        } else {
            format!("\n\nDid you mean: {}?", suggestions.join(", "))
        };
        return format!(
            "No results found for \"{query}\" in {} v{}.{suggestion_text}",
            index.crate_name, index.version
        );
    }

    let mut parts = Vec::new();
    parts.push(format!(
        "## Search results for \"{query}\" in {} v{}\n",
        index.crate_name, index.version
    ));

    for result in results {
        let item = &result.item;
        let doc_suffix = if item.short_doc.is_empty() {
            String::new()
        } else {
            format!(" — {}", item.short_doc)
        };
        parts.push(format!(
            "- [{kind}] `{path}`{doc_suffix}",
            kind = item.kind,
            path = item.path,
        ));
    }

    parts.join("\n")
}

/// Render impl blocks for a type (for `lookup_impl_block`).
pub fn render_impls(
    item_path: &str,
    impls: &[&ImplBlock],
) -> String {
    if impls.is_empty() {
        return format!("No implementations found for `{item_path}`.");
    }

    let mut parts = Vec::new();
    parts.push(format!("## Implementations for `{item_path}`\n"));

    // Separate inherent impls from trait impls
    let inherent: Vec<_> = impls.iter().filter(|i| i.trait_name.is_none()).collect();
    let trait_impls: Vec<_> = impls.iter().filter(|i| i.trait_name.is_some()).collect();

    if !inherent.is_empty() {
        parts.push("### Inherent Methods\n".to_string());
        for block in &inherent {
            for m in &block.methods {
                let doc = if m.doc.is_empty() {
                    String::new()
                } else {
                    format!("\n  {}", first_line(&m.doc))
                };
                parts.push(format!("- `{}`{doc}", m.signature));
            }
        }
        parts.push(String::new());
    }

    if !trait_impls.is_empty() {
        parts.push("### Trait Implementations\n".to_string());
        for block in &trait_impls {
            parts.push(format!("#### {}\n", block.header));
            if block.methods.is_empty() {
                parts.push("  _(auto-derived, no custom methods)_\n".to_string());
            } else {
                for m in &block.methods {
                    parts.push(format!("- `{}`", m.signature));
                    if !m.doc.is_empty() {
                        parts.push(format!("  {}\n", first_line(&m.doc)));
                    }
                }
            }
            parts.push(String::new());
        }
    }

    parts.join("\n")
}

/// Render a "not found" message with suggestions.
pub fn render_not_found(index: &CrateIndex, item_path: &str) -> String {
    let suggestions = index.suggest_similar(item_path, 5);
    let suggestion_text = if suggestions.is_empty() {
        String::new()
    } else {
        format!(
            "\n\nDid you mean one of:\n{}",
            suggestions
                .iter()
                .map(|s| format!("- `{s}`"))
                .collect::<Vec<_>>()
                .join("\n")
        )
    };
    format!(
        "Item `{item_path}` not found in {} v{}.{suggestion_text}",
        index.crate_name, index.version
    )
}

fn kind_label(kind: &ItemKind) -> &'static str {
    match kind {
        ItemKind::Module => "Module",
        ItemKind::Struct => "Struct",
        ItemKind::Enum => "Enum",
        ItemKind::Trait => "Trait",
        ItemKind::Function => "Function",
        ItemKind::TypeAlias => "Type Alias",
        ItemKind::Constant => "Constant",
        ItemKind::Static => "Static",
        ItemKind::Macro => "Macro",
        ItemKind::Union => "Union",
    }
}

fn first_line(s: &str) -> &str {
    s.lines().next().unwrap_or("")
}
