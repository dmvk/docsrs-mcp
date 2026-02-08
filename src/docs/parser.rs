use rustdoc_types::{
    Crate, Enum, Function, GenericArg, GenericArgs, GenericBound, GenericParamDef,
    GenericParamDefKind, Id, Impl, Item, ItemEnum, Path as RustdocPath, Struct, StructKind, Trait,
    Type, Union, Variant, VariantKind,
};
use std::collections::HashMap;

use super::index::{
    CrateIndex, FieldInfo, ImplBlock, IndexedItem, ItemDetail, ItemKind, MethodInfo, VariantInfo,
};

/// Convert a `rustdoc_types::Crate` into a `CrateIndex`.
///
/// Uses a flat iteration approach inspired by cargo-doc-md:
/// - Path resolution uses `crate_data.paths` directly (Id → ItemSummary with path: Vec<String>)
/// - Items are discovered by iterating ALL entries in `crate_data.index`
/// - Module membership is determined by dropping the last path component
pub fn parse_crate(krate: &Crate, crate_name: &str, version: &str) -> CrateIndex {
    let ctx = ParseContext { krate };

    let mut index = CrateIndex {
        crate_name: crate_name.to_string(),
        version: version.to_string(),
        items: HashMap::new(),
        modules: HashMap::new(),
        impl_blocks: HashMap::new(),
        root_items: Vec::new(),
    };

    // Build a path map from Id → fully qualified path string using krate.paths
    let mut path_map: HashMap<Id, String> = HashMap::new();
    for (id, summary) in &krate.paths {
        if !summary.path.is_empty() {
            path_map.insert(id.clone(), summary.path.join("::"));
        }
    }

    // Phase 1: Iterate ALL items in krate.index and index named, non-impl items.
    // For each item, look up its path in krate.paths. If not in paths, skip it
    // (it's likely a sub-item like a struct field or variant, handled via parent).
    for (id, item) in &krate.index {
        // Skip impl blocks (handled in phase 2)
        if matches!(&item.inner, ItemEnum::Impl(_)) {
            continue;
        }

        // Skip unnamed items
        let Some(name) = &item.name else {
            continue;
        };

        // Skip sub-items that are children of other items (fields, variants, etc.)
        if matches!(&item.inner, ItemEnum::StructField(_) | ItemEnum::Variant(_)) {
            continue;
        }

        // Look up the item's path via krate.paths
        let item_path = match path_map.get(id) {
            Some(p) => p.clone(),
            None => {
                // Item not in paths table — this can happen for re-exports or
                // items only reachable through the index. Skip.
                tracing::trace!("Item {name:?} ({id:?}) not found in krate.paths, skipping");
                continue;
            }
        };

        // Determine parent module by dropping the last path component
        let parent_module = match item_path.rsplit_once("::") {
            Some((parent, _)) => parent.to_string(),
            None => crate_name.to_string(),
        };

        if let Some(indexed) = ctx.index_item(item, name, &item_path, &parent_module) {
            let kind = indexed.kind.clone();

            // Track in parent module
            if parent_module == crate_name {
                index.root_items.push(item_path.clone());
            } else {
                index
                    .modules
                    .entry(parent_module.to_string())
                    .or_default()
                    .push(item_path.clone());
            }

            // If this is a module, ensure it has an entry in the modules map
            if kind == ItemKind::Module {
                index.modules.entry(item_path.clone()).or_default();
            }

            index.items.insert(item_path, indexed);
        }
    }

    // Phase 2: Process all impl blocks
    for item in krate.index.values() {
        if let ItemEnum::Impl(impl_) = &item.inner {
            ctx.process_impl(impl_, &path_map, &mut index);
        }
    }

    tracing::info!(
        "Indexed {} items, {} modules, {} impl block groups for {crate_name}",
        index.items.len(),
        index.modules.len(),
        index.impl_blocks.len(),
    );

    index
}

struct ParseContext<'a> {
    krate: &'a Crate,
}

impl<'a> ParseContext<'a> {
    /// Convert a single rustdoc Item into an IndexedItem.
    fn index_item(
        &self,
        item: &Item,
        name: &str,
        item_path: &str,
        parent_module: &str,
    ) -> Option<IndexedItem> {
        let (kind, signature, detail) = match &item.inner {
            ItemEnum::Module(_) => (
                ItemKind::Module,
                format!("mod {name}"),
                ItemDetail::default(),
            ),
            ItemEnum::Struct(s) => {
                let sig = self.render_struct_signature(name, s, item);
                let detail = self.struct_detail(s);
                (ItemKind::Struct, sig, detail)
            }
            ItemEnum::Enum(e) => {
                let sig = self.render_enum_signature(name, e, item);
                let detail = self.enum_detail(e);
                (ItemKind::Enum, sig, detail)
            }
            ItemEnum::Trait(t) => {
                let sig = self.render_trait_signature(name, t, item);
                let detail = self.trait_detail(t);
                (ItemKind::Trait, sig, detail)
            }
            ItemEnum::Function(f) => {
                let sig = self.render_function_signature(name, f, item);
                (ItemKind::Function, sig, ItemDetail::default())
            }
            ItemEnum::TypeAlias(ta) => {
                let sig = format!(
                    "pub type {name}{} = {}",
                    render_generics_from_item(item),
                    render_type(&ta.type_)
                );
                (ItemKind::TypeAlias, sig, ItemDetail::default())
            }
            ItemEnum::Constant { type_, const_: _ } => {
                let sig = format!("pub const {name}: {}", render_type(type_));
                (ItemKind::Constant, sig, ItemDetail::default())
            }
            ItemEnum::Static(s) => {
                let sig = format!(
                    "pub static {}{name}: {}",
                    if s.is_mutable { "mut " } else { "" },
                    render_type(&s.type_)
                );
                (ItemKind::Static, sig, ItemDetail::default())
            }
            ItemEnum::Macro(mac) => {
                let sig = if mac.is_empty() {
                    format!("macro_rules! {name}")
                } else {
                    mac.clone()
                };
                (ItemKind::Macro, sig, ItemDetail::default())
            }
            ItemEnum::Union(u) => {
                let sig = self.render_union_signature(name, u, item);
                let detail = self.union_detail(u);
                (ItemKind::Union, sig, detail)
            }
            // Skip items we don't index (imports, extern crates, fields, variants, etc.)
            other => {
                tracing::trace!("Skipping {name} ({:?})", std::mem::discriminant(other));
                return None;
            }
        };

        let doc = item.docs.clone().unwrap_or_default();
        let short_doc = first_sentence(&doc);

        Some(IndexedItem {
            path: item_path.to_string(),
            name: name.to_string(),
            kind,
            signature,
            short_doc,
            doc,
            detail,
            parent_module: parent_module.to_string(),
        })
    }

    // ========== Signature rendering ==========

    fn render_struct_signature(&self, name: &str, s: &Struct, item: &Item) -> String {
        let generics = render_generics_from_item(item);
        match &s.kind {
            StructKind::Unit => format!("pub struct {name}{generics};"),
            StructKind::Tuple(fields) => {
                let fields_str: Vec<String> = fields
                    .iter()
                    .map(|f| match f {
                        Some(id) => self
                            .krate
                            .index
                            .get(id)
                            .and_then(|item| match &item.inner {
                                ItemEnum::StructField(ty) => Some(render_type(ty)),
                                _ => None,
                            })
                            .unwrap_or_else(|| "_".to_string()),
                        None => "_".to_string(),
                    })
                    .collect();
                format!("pub struct {name}{generics}({});", fields_str.join(", "))
            }
            StructKind::Plain {
                fields,
                has_stripped_fields,
            } => {
                if fields.is_empty() {
                    if *has_stripped_fields {
                        format!("pub struct {name}{generics} {{ /* private fields */ }}")
                    } else {
                        format!("pub struct {name}{generics} {{}}")
                    }
                } else {
                    let fields_str = self.render_fields(fields);
                    let private = if *has_stripped_fields {
                        "\n    // ... private fields\n"
                    } else {
                        ""
                    };
                    format!("pub struct {name}{generics} {{\n{fields_str}{private}}}",)
                }
            }
        }
    }

    fn render_enum_signature(&self, name: &str, e: &Enum, item: &Item) -> String {
        let generics = render_generics_from_item(item);
        if e.variants.is_empty() {
            return format!("pub enum {name}{generics} {{}}");
        }

        let variants: Vec<String> = e
            .variants
            .iter()
            .filter_map(|id| {
                let item = self.krate.index.get(id)?;
                let vname = item.name.as_ref()?;
                match &item.inner {
                    ItemEnum::Variant(v) => Some(self.render_variant(vname, v)),
                    _ => None,
                }
            })
            .collect();

        let has_stripped = e.has_stripped_variants;
        let stripped_line = if has_stripped {
            "\n    // ... other variants"
        } else {
            ""
        };
        format!(
            "pub enum {name}{generics} {{\n{}{stripped_line}\n}}",
            variants.join("\n")
        )
    }

    fn render_variant(&self, name: &str, variant: &Variant) -> String {
        match &variant.kind {
            VariantKind::Plain => format!("    {name},"),
            VariantKind::Tuple(fields) => {
                let fields_str: Vec<String> = fields
                    .iter()
                    .map(|f| match f {
                        Some(id) => self
                            .krate
                            .index
                            .get(id)
                            .and_then(|item| match &item.inner {
                                ItemEnum::StructField(ty) => Some(render_type(ty)),
                                _ => None,
                            })
                            .unwrap_or_else(|| "_".to_string()),
                        None => "_".to_string(),
                    })
                    .collect();
                format!("    {name}({}),", fields_str.join(", "))
            }
            VariantKind::Struct {
                fields,
                has_stripped_fields,
            } => {
                let fields_str = self.render_fields(fields);
                let private = if *has_stripped_fields {
                    "        // ... private fields\n"
                } else {
                    ""
                };
                format!("    {name} {{\n{fields_str}{private}    }},")
            }
        }
    }

    fn render_trait_signature(&self, name: &str, t: &Trait, item: &Item) -> String {
        let generics = render_generics_from_item(item);
        let bounds = if t.bounds.is_empty() {
            String::new()
        } else {
            let bounds_str: Vec<String> = t.bounds.iter().map(render_generic_bound).collect();
            format!(": {}", bounds_str.join(" + "))
        };

        let methods = self.collect_trait_methods(t);
        if methods.is_empty() {
            format!("pub trait {name}{generics}{bounds} {{}}")
        } else {
            let method_sigs: Vec<String> = methods
                .iter()
                .map(|m| format!("    {};", m.signature))
                .collect();
            format!(
                "pub trait {name}{generics}{bounds} {{\n{}\n}}",
                method_sigs.join("\n")
            )
        }
    }

    fn render_function_signature(&self, name: &str, func: &Function, _item: &Item) -> String {
        let header = &func.header;
        let mut parts = Vec::new();
        parts.push("pub".to_string());
        if header.is_const {
            parts.push("const".to_string());
        }
        if header.is_async {
            parts.push("async".to_string());
        }
        if header.is_unsafe {
            parts.push("unsafe".to_string());
        }
        parts.push("fn".to_string());

        let generics = render_generics(&func.generics.params);

        let params: Vec<String> = func
            .sig
            .inputs
            .iter()
            .map(|(pname, ty)| format!("{pname}: {}", render_type(ty)))
            .collect();

        let ret = func
            .sig
            .output
            .as_ref()
            .map(|ty| format!(" -> {}", render_type(ty)))
            .unwrap_or_default();

        let where_clause = render_where_clause(&func.generics.where_predicates);

        format!(
            "{} {name}{generics}({}){ret}{where_clause}",
            parts.join(" "),
            params.join(", ")
        )
    }

    fn render_union_signature(&self, name: &str, _u: &Union, item: &Item) -> String {
        let generics = render_generics_from_item(item);
        format!("pub union {name}{generics} {{ ... }}")
    }

    fn render_fields(&self, fields: &[Id]) -> String {
        fields
            .iter()
            .filter_map(|id| {
                let item = self.krate.index.get(id)?;
                let name = item.name.as_ref()?;
                match &item.inner {
                    ItemEnum::StructField(ty) => {
                        Some(format!("    pub {name}: {},\n", render_type(ty)))
                    }
                    _ => None,
                }
            })
            .collect()
    }

    // ========== Detail extraction ==========

    fn struct_detail(&self, s: &Struct) -> ItemDetail {
        let fields = match &s.kind {
            StructKind::Plain { fields, .. } => self.extract_fields(fields),
            StructKind::Tuple(fields) => fields
                .iter()
                .enumerate()
                .filter_map(|(i, f)| {
                    let id = f.as_ref()?;
                    let item = self.krate.index.get(id)?;
                    match &item.inner {
                        ItemEnum::StructField(ty) => Some(FieldInfo {
                            name: i.to_string(),
                            type_str: render_type(ty),
                            doc: item.docs.clone().unwrap_or_default(),
                        }),
                        _ => None,
                    }
                })
                .collect(),
            StructKind::Unit => Vec::new(),
        };
        ItemDetail {
            fields,
            ..Default::default()
        }
    }

    fn enum_detail(&self, e: &Enum) -> ItemDetail {
        let variants: Vec<VariantInfo> = e
            .variants
            .iter()
            .filter_map(|id| {
                let item = self.krate.index.get(id)?;
                let name = item.name.as_ref()?;
                match &item.inner {
                    ItemEnum::Variant(v) => Some(VariantInfo {
                        name: name.clone(),
                        signature: self.render_variant(name, v),
                        doc: item.docs.clone().unwrap_or_default(),
                    }),
                    _ => None,
                }
            })
            .collect();
        ItemDetail {
            variants,
            ..Default::default()
        }
    }

    fn trait_detail(&self, t: &Trait) -> ItemDetail {
        let methods = self.collect_trait_methods(t);
        ItemDetail {
            methods,
            ..Default::default()
        }
    }

    fn union_detail(&self, u: &Union) -> ItemDetail {
        let fields = self.extract_fields(&u.fields);
        ItemDetail {
            fields,
            ..Default::default()
        }
    }

    fn extract_fields(&self, field_ids: &[Id]) -> Vec<FieldInfo> {
        field_ids
            .iter()
            .filter_map(|id| {
                let item = self.krate.index.get(id)?;
                let name = item.name.as_ref()?;
                match &item.inner {
                    ItemEnum::StructField(ty) => Some(FieldInfo {
                        name: name.clone(),
                        type_str: render_type(ty),
                        doc: item.docs.clone().unwrap_or_default(),
                    }),
                    _ => None,
                }
            })
            .collect()
    }

    fn collect_trait_methods(&self, t: &Trait) -> Vec<MethodInfo> {
        t.items
            .iter()
            .filter_map(|id| {
                let item = self.krate.index.get(id)?;
                let name = item.name.as_ref()?;
                match &item.inner {
                    ItemEnum::Function(f) => {
                        let sig = self.render_function_signature(name, f, item);
                        let is_required = !f.has_body;
                        Some(MethodInfo {
                            name: name.clone(),
                            signature: sig,
                            doc: item.docs.clone().unwrap_or_default(),
                            is_required,
                        })
                    }
                    _ => None,
                }
            })
            .collect()
    }

    /// Process an impl block and attach it to the implementing type.
    fn process_impl(&self, impl_: &Impl, path_map: &HashMap<Id, String>, index: &mut CrateIndex) {
        let type_path = match &impl_.for_ {
            Type::ResolvedPath(path) => resolve_path(path, path_map),
            _ => return,
        };

        let Some(type_path) = type_path else {
            return;
        };

        let trait_name = impl_
            .trait_
            .as_ref()
            .and_then(|p| resolve_path(p, path_map))
            .map(|p| p.rsplit("::").next().unwrap_or(&p).to_string());

        let header = if let Some(ref tn) = trait_name {
            format!(
                "impl {tn} for {}",
                type_path.rsplit("::").next().unwrap_or(&type_path)
            )
        } else {
            format!(
                "impl {}",
                type_path.rsplit("::").next().unwrap_or(&type_path)
            )
        };

        let methods: Vec<MethodInfo> = impl_
            .items
            .iter()
            .filter_map(|id| {
                let item = self.krate.index.get(id)?;
                let name = item.name.as_ref()?;
                match &item.inner {
                    ItemEnum::Function(f) => {
                        let sig = self.render_function_signature(name, f, item);
                        Some(MethodInfo {
                            name: name.clone(),
                            signature: sig,
                            doc: item.docs.clone().unwrap_or_default(),
                            is_required: false,
                        })
                    }
                    _ => None,
                }
            })
            .collect();

        // Skip empty auto-trait impls
        if methods.is_empty() && trait_name.is_some() {
            let tn = trait_name.as_deref().unwrap_or("");
            let boring = ["Send", "Sync", "Unpin", "UnwindSafe", "RefUnwindSafe"];
            if boring.contains(&tn) {
                return;
            }
        }

        let block = ImplBlock {
            header,
            trait_name,
            methods,
        };

        index.impl_blocks.entry(type_path).or_default().push(block);
    }
}

/// Resolve a rustdoc Path to a fully qualified string using the path map.
fn resolve_path(path: &RustdocPath, path_map: &HashMap<Id, String>) -> Option<String> {
    path_map
        .get(&path.id)
        .cloned()
        .or_else(|| Some(path.path.clone()))
}

// ========== Type rendering (free functions) ==========

pub fn render_type(ty: &Type) -> String {
    match ty {
        Type::ResolvedPath(path) => {
            let mut s = path.path.clone();
            if let Some(args) = &path.args {
                s.push_str(&render_generic_args(args));
            }
            s
        }
        Type::DynTrait(dyn_trait) => {
            let traits: Vec<String> = dyn_trait
                .traits
                .iter()
                .map(|poly| {
                    let mut s = poly.trait_.path.clone();
                    if let Some(args) = &poly.trait_.args {
                        s.push_str(&render_generic_args(args));
                    }
                    s
                })
                .collect();
            format!("dyn {}", traits.join(" + "))
        }
        Type::Generic(name) => name.clone(),
        Type::Primitive(name) => name.clone(),
        Type::FunctionPointer(fp) => {
            let params: Vec<String> = fp
                .sig
                .inputs
                .iter()
                .map(|(_, ty)| render_type(ty))
                .collect();
            let ret = fp
                .sig
                .output
                .as_ref()
                .map(|ty| format!(" -> {}", render_type(ty)))
                .unwrap_or_default();
            format!("fn({}){ret}", params.join(", "))
        }
        Type::Tuple(types) => {
            let inner: Vec<String> = types.iter().map(render_type).collect();
            format!("({})", inner.join(", "))
        }
        Type::Slice(ty) => format!("[{}]", render_type(ty)),
        Type::Array { type_, len } => format!("[{}; {len}]", render_type(type_)),
        Type::Pat { type_, .. } => render_type(type_),
        Type::ImplTrait(bounds) => {
            let b: Vec<String> = bounds.iter().map(render_generic_bound).collect();
            format!("impl {}", b.join(" + "))
        }
        Type::Infer => "_".to_string(),
        Type::RawPointer { is_mutable, type_ } => {
            if *is_mutable {
                format!("*mut {}", render_type(type_))
            } else {
                format!("*const {}", render_type(type_))
            }
        }
        Type::BorrowedRef {
            lifetime,
            is_mutable,
            type_,
        } => {
            let lt = lifetime
                .as_ref()
                .map(|l| format!("{l} "))
                .unwrap_or_default();
            let mutability = if *is_mutable { "mut " } else { "" };
            format!("&{lt}{mutability}{}", render_type(type_))
        }
        Type::QualifiedPath {
            name,
            self_type,
            trait_,
            ..
        } => {
            let self_ty = render_type(self_type);
            match trait_ {
                Some(trait_path) => format!("<{self_ty} as {}>::{name}", trait_path.path),
                None => format!("<{self_ty}>::{name}"),
            }
        }
    }
}

fn render_generic_args(args: &GenericArgs) -> String {
    match args {
        GenericArgs::AngleBracketed { args, constraints } => {
            if args.is_empty() && constraints.is_empty() {
                return String::new();
            }
            let mut parts: Vec<String> = args.iter().map(render_generic_arg).collect();
            for c in constraints {
                let binding = match &c.binding {
                    rustdoc_types::AssocItemConstraintKind::Equality(term) => match term {
                        rustdoc_types::Term::Type(ty) => {
                            format!("{} = {}", c.name, render_type(ty))
                        }
                        rustdoc_types::Term::Constant(c2) => {
                            format!("{} = {}", c.name, c2.value.as_deref().unwrap_or(&c2.expr))
                        }
                    },
                    rustdoc_types::AssocItemConstraintKind::Constraint(bounds) => {
                        let b: Vec<String> = bounds.iter().map(render_generic_bound).collect();
                        format!("{}: {}", c.name, b.join(" + "))
                    }
                };
                parts.push(binding);
            }
            format!("<{}>", parts.join(", "))
        }
        GenericArgs::Parenthesized { inputs, output } => {
            let params: Vec<String> = inputs.iter().map(render_type).collect();
            let ret = output
                .as_ref()
                .map(|ty| format!(" -> {}", render_type(ty)))
                .unwrap_or_default();
            format!("({}){ret}", params.join(", "))
        }
        GenericArgs::ReturnTypeNotation => "(..)".to_string(),
    }
}

fn render_generic_arg(arg: &GenericArg) -> String {
    match arg {
        GenericArg::Lifetime(lt) => lt.clone(),
        GenericArg::Type(ty) => render_type(ty),
        GenericArg::Const(c) => c.value.as_deref().unwrap_or(&c.expr).to_string(),
        GenericArg::Infer => "_".to_string(),
    }
}

fn render_generic_bound(bound: &GenericBound) -> String {
    match bound {
        GenericBound::TraitBound {
            trait_, modifier, ..
        } => {
            let prefix = match modifier {
                rustdoc_types::TraitBoundModifier::None => "",
                rustdoc_types::TraitBoundModifier::Maybe => "?",
                rustdoc_types::TraitBoundModifier::MaybeConst => "~const ",
            };
            let mut s = format!("{prefix}{}", trait_.path);
            if let Some(args) = &trait_.args {
                s.push_str(&render_generic_args(args));
            }
            s
        }
        GenericBound::Outlives(lt) => lt.clone(),
        GenericBound::Use(_) => "use<..>".to_string(),
    }
}

fn render_generics(params: &[GenericParamDef]) -> String {
    let rendered: Vec<String> = params
        .iter()
        .filter_map(|p| match &p.kind {
            GenericParamDefKind::Lifetime { outlives } => {
                let mut s = p.name.clone();
                if !outlives.is_empty() {
                    s.push_str(&format!(": {}", outlives.join(" + ")));
                }
                Some(s)
            }
            GenericParamDefKind::Type {
                bounds, default, ..
            } => {
                let mut s = p.name.clone();
                if !bounds.is_empty() {
                    let b: Vec<String> = bounds.iter().map(render_generic_bound).collect();
                    s.push_str(&format!(": {}", b.join(" + ")));
                }
                if let Some(def) = default {
                    s.push_str(&format!(" = {}", render_type(def)));
                }
                Some(s)
            }
            GenericParamDefKind::Const { type_, default } => {
                let mut s = format!("const {}: {}", p.name, render_type(type_));
                if let Some(def) = default {
                    s.push_str(&format!(" = {def}"));
                }
                Some(s)
            }
        })
        .collect();

    if rendered.is_empty() {
        String::new()
    } else {
        format!("<{}>", rendered.join(", "))
    }
}

fn render_generics_from_item(item: &Item) -> String {
    let generics = match &item.inner {
        ItemEnum::Struct(s) => Some(&s.generics),
        ItemEnum::Enum(e) => Some(&e.generics),
        ItemEnum::Trait(t) => Some(&t.generics),
        ItemEnum::Union(u) => Some(&u.generics),
        ItemEnum::TypeAlias(ta) => Some(&ta.generics),
        ItemEnum::Function(f) => Some(&f.generics),
        _ => None,
    };

    generics
        .map(|g| render_generics(&g.params))
        .unwrap_or_default()
}

fn render_where_clause(predicates: &[rustdoc_types::WherePredicate]) -> String {
    if predicates.is_empty() {
        return String::new();
    }

    let clauses: Vec<String> = predicates
        .iter()
        .map(|pred| match pred {
            rustdoc_types::WherePredicate::BoundPredicate { type_, bounds, .. } => {
                let bounds_str: Vec<String> = bounds.iter().map(render_generic_bound).collect();
                format!("{}: {}", render_type(type_), bounds_str.join(" + "))
            }
            rustdoc_types::WherePredicate::LifetimePredicate { lifetime, outlives } => {
                format!("{lifetime}: {}", outlives.join(" + "))
            }
            rustdoc_types::WherePredicate::EqPredicate { lhs, rhs } => match rhs {
                rustdoc_types::Term::Type(ty) => {
                    format!("{} = {}", render_type(lhs), render_type(ty))
                }
                rustdoc_types::Term::Constant(c) => {
                    format!(
                        "{} = {}",
                        render_type(lhs),
                        c.value.as_deref().unwrap_or(&c.expr)
                    )
                }
            },
        })
        .collect();

    format!("\nwhere\n    {}", clauses.join(",\n    "))
}

/// Extract the first sentence from a documentation string.
fn first_sentence(doc: &str) -> String {
    let trimmed = doc.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    // Take everything up to the first period followed by whitespace/newline, or the first newline
    let mut end = trimmed.len();
    for (i, ch) in trimmed.char_indices() {
        if ch == '\n' {
            end = i;
            break;
        }
        if ch == '.' {
            // Check if next char is whitespace or end of string
            let next = trimmed[i + 1..].chars().next();
            if next.is_none() || next == Some(' ') || next == Some('\n') {
                end = i + 1;
                break;
            }
        }
    }
    trimmed[..end].trim().to_string()
}
