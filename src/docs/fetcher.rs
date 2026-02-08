use crate::error::Error;

/// Fetch the rustdoc JSON for a crate from docs.rs.
///
/// The URL pattern is: `https://docs.rs/crate/{name}/{version}/json`
/// docs.rs serves this as zstd-compressed JSON.
///
/// docs.rs serves varying rustdoc JSON format versions (53-56) across crates.
/// We use `rustdoc-types` 0.56 and normalize older/newer formats before
/// deserializing so that all versions parse successfully.
pub async fn fetch_rustdoc_json(
    client: &reqwest::Client,
    crate_name: &str,
    version: &str,
) -> Result<rustdoc_types::Crate, Error> {
    let url = format!("https://docs.rs/crate/{crate_name}/{version}/json");
    tracing::info!("Fetching rustdoc JSON from {url}");

    let response = client.get(&url).send().await?;

    let status = response.status();
    if status == reqwest::StatusCode::NOT_FOUND {
        return Err(Error::JsonNotAvailable {
            crate_name: crate_name.to_string(),
            version: version.to_string(),
        });
    }

    let response = response.error_for_status()?;
    let bytes = response.bytes().await?;

    // docs.rs serves rustdoc JSON as zstd-compressed
    let decompressed = zstd::stream::decode_all(bytes.as_ref()).map_err(Error::Zstd)?;

    // Parse into a generic JSON value first so we can normalize across format versions
    let mut value: serde_json::Value = serde_json::from_slice(&decompressed)?;

    let format_version = value
        .get("format_version")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    tracing::info!("Rustdoc JSON for {crate_name} v{version} has format_version {format_version}");

    normalize_for_v56(&mut value, format_version);

    let krate: rustdoc_types::Crate = serde_json::from_value(value)?;
    tracing::info!(
        "Parsed rustdoc JSON for {crate_name} v{version}: {} items",
        krate.index.len()
    );
    Ok(krate)
}

/// Normalize a rustdoc JSON value so it deserializes with `rustdoc-types` 0.56
/// (format version 56).
///
/// Format differences we handle:
/// - **53 -> 54**: `Item.attrs` changed from `Vec<String>` to `Vec<Attribute>` (tagged enum).
///   We don't use attrs, so we empty all attrs arrays.
/// - **55 -> 56**: `Crate.target: Target` added; `Attribute::MacroExport` variant added.
///   We inject a dummy target for older formats.
/// - **56 -> 57**: `ExternalCrate.path: PathBuf` added. We strip it since 0.56 doesn't expect it.
fn normalize_for_v56(value: &mut serde_json::Value, format_version: u64) {
    // For all versions: empty attrs arrays (format changed 53->54, we don't use them)
    strip_attrs(value);

    // For format < 56: inject a dummy target (Crate.target was added in format 56)
    if format_version < 56 {
        inject_dummy_target(value);
    }

    // For format 57+: strip ExternalCrate.path (doesn't exist in 0.56)
    if format_version >= 57 {
        strip_external_crate_paths(value);
    }
}

/// Recursively replace all `"attrs"` arrays with `[]`.
///
/// The `attrs` field changed from `Vec<String>` (format <= 53) to `Vec<Attribute>`
/// (format >= 54). Since we never use attrs, emptying them avoids deserialization
/// errors regardless of format version.
fn strip_attrs(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::Object(map) => {
            if let Some(attrs) = map.get_mut("attrs") {
                if attrs.is_array() {
                    *attrs = serde_json::Value::Array(Vec::new());
                }
            }
            for v in map.values_mut() {
                strip_attrs(v);
            }
        }
        serde_json::Value::Array(arr) => {
            for v in arr.iter_mut() {
                strip_attrs(v);
            }
        }
        _ => {}
    }
}

/// Inject a dummy `target` field into the root if not present.
///
/// Format version 56 added `Crate.target: Target` which older formats lack.
/// We inject a minimal placeholder so deserialization succeeds.
fn inject_dummy_target(value: &mut serde_json::Value) {
    if let serde_json::Value::Object(map) = value {
        if !map.contains_key("target") {
            map.insert(
                "target".to_string(),
                serde_json::json!({
                    "triple": "unknown",
                    "target_features": []
                }),
            );
        }
    }
}

/// Remove the `"path"` key from each entry in `"external_crates"`.
///
/// Format version 57 added `ExternalCrate.path: PathBuf` which doesn't exist
/// in rustdoc-types 0.56. Stripping it allows 57+ JSON to deserialize.
fn strip_external_crate_paths(value: &mut serde_json::Value) {
    if let Some(external_crates) = value.get_mut("external_crates") {
        if let serde_json::Value::Object(crates_map) = external_crates {
            for crate_value in crates_map.values_mut() {
                if let serde_json::Value::Object(crate_obj) = crate_value {
                    crate_obj.remove("path");
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // ========== strip_attrs tests ==========

    #[test]
    fn strip_attrs_empties_top_level_array() {
        let mut value = json!({
            "attrs": ["#[derive(Debug)]", "#[allow(unused)]"]
        });
        strip_attrs(&mut value);
        assert_eq!(value["attrs"], json!([]));
    }

    #[test]
    fn strip_attrs_empties_nested_in_items() {
        // Simulates the real rustdoc JSON structure: items inside the index have attrs
        let mut value = json!({
            "index": {
                "0:3": {
                    "name": "MyStruct",
                    "attrs": ["#[derive(Debug)]"],
                    "inner": { "kind": "struct" }
                },
                "0:5": {
                    "name": "my_fn",
                    "attrs": ["#[inline]"],
                    "inner": { "kind": "function" }
                }
            }
        });
        strip_attrs(&mut value);
        assert_eq!(value["index"]["0:3"]["attrs"], json!([]));
        assert_eq!(value["index"]["0:5"]["attrs"], json!([]));
    }

    #[test]
    fn strip_attrs_handles_format_54_tagged_enum_attrs() {
        // Format 54+ uses tagged enum attrs like {"Attribute": "derive"}
        // These are still arrays, so strip_attrs should empty them
        let mut value = json!({
            "attrs": [
                {"Derive": "Debug"},
                {"Other": {"value": "#[serde(rename)]"}}
            ]
        });
        strip_attrs(&mut value);
        assert_eq!(value["attrs"], json!([]));
    }

    #[test]
    fn strip_attrs_leaves_non_array_attrs_alone() {
        // If "attrs" is not an array (hypothetical), don't touch it
        let mut value = json!({ "attrs": "not-an-array" });
        strip_attrs(&mut value);
        assert_eq!(value["attrs"], json!("not-an-array"));
    }

    #[test]
    fn strip_attrs_recurses_into_arrays() {
        // Items can appear inside arrays (e.g., in some JSON structures)
        let mut value = json!([
            { "attrs": ["a"] },
            { "attrs": ["b", "c"] }
        ]);
        strip_attrs(&mut value);
        assert_eq!(value[0]["attrs"], json!([]));
        assert_eq!(value[1]["attrs"], json!([]));
    }

    #[test]
    fn strip_attrs_no_attrs_key_is_noop() {
        let mut value = json!({"name": "foo", "inner": {}});
        let original = value.clone();
        strip_attrs(&mut value);
        assert_eq!(value, original);
    }

    // ========== strip_external_crate_paths tests ==========

    #[test]
    fn strip_external_crate_paths_removes_path_field() {
        let mut value = json!({
            "external_crates": {
                "0": { "name": "std", "path": "/rustc/src/std" },
                "1": { "name": "core", "path": "/rustc/src/core" }
            }
        });
        strip_external_crate_paths(&mut value);

        let crate_0 = &value["external_crates"]["0"];
        assert_eq!(crate_0["name"], json!("std"));
        assert!(crate_0.get("path").is_none());

        let crate_1 = &value["external_crates"]["1"];
        assert_eq!(crate_1["name"], json!("core"));
        assert!(crate_1.get("path").is_none());
    }

    #[test]
    fn strip_external_crate_paths_preserves_other_fields() {
        let mut value = json!({
            "external_crates": {
                "0": { "name": "serde", "html_root_url": "https://docs.rs/serde", "path": "/some/path" }
            }
        });
        strip_external_crate_paths(&mut value);

        let crate_0 = &value["external_crates"]["0"];
        assert_eq!(crate_0["name"], json!("serde"));
        assert_eq!(crate_0["html_root_url"], json!("https://docs.rs/serde"));
        assert!(crate_0.get("path").is_none());
    }

    #[test]
    fn strip_external_crate_paths_noop_without_external_crates() {
        let mut value = json!({"index": {}, "paths": {}});
        let original = value.clone();
        strip_external_crate_paths(&mut value);
        assert_eq!(value, original);
    }

    #[test]
    fn strip_external_crate_paths_noop_when_no_path_fields() {
        let mut value = json!({
            "external_crates": {
                "0": { "name": "std" }
            }
        });
        let original = value.clone();
        strip_external_crate_paths(&mut value);
        assert_eq!(value, original);
    }

    // ========== inject_dummy_target tests ==========

    #[test]
    fn inject_dummy_target_adds_when_missing() {
        let mut value = json!({"root": 0});
        inject_dummy_target(&mut value);
        assert!(value.get("target").is_some());
        assert_eq!(value["target"]["triple"], json!("unknown"));
        assert_eq!(value["target"]["target_features"], json!([]));
    }

    #[test]
    fn inject_dummy_target_noop_when_present() {
        let mut value = json!({
            "target": { "triple": "x86_64-unknown-linux-gnu", "target_features": [] }
        });
        let original = value.clone();
        inject_dummy_target(&mut value);
        assert_eq!(value, original);
    }

    // ========== normalize_for_v56 integration tests ==========

    #[test]
    fn normalize_v53_strips_attrs_and_injects_target() {
        let mut value = json!({
            "format_version": 53,
            "index": {
                "0": { "attrs": ["#[derive(Debug)]"], "name": "Foo" }
            },
            "external_crates": {
                "1": { "name": "std" }
            }
        });
        normalize_for_v56(&mut value, 53);

        // attrs should be emptied
        assert_eq!(value["index"]["0"]["attrs"], json!([]));
        // external_crates should be untouched (no path to strip, and version < 57)
        assert_eq!(value["external_crates"]["1"]["name"], json!("std"));
        // target should be injected
        assert_eq!(value["target"]["triple"], json!("unknown"));
    }

    #[test]
    fn normalize_v56_strips_attrs_only() {
        let mut value = json!({
            "format_version": 56,
            "index": {
                "0:1": { "attrs": [{"Other": "#[cfg(test)]"}], "name": "Bar" }
            },
            "external_crates": {
                "1": { "name": "core" }
            }
        });
        normalize_for_v56(&mut value, 56);

        assert_eq!(value["index"]["0:1"]["attrs"], json!([]));
        assert_eq!(value["external_crates"]["1"]["name"], json!("core"));
    }

    #[test]
    fn normalize_v57_strips_attrs_and_external_crate_paths() {
        let mut value = json!({
            "format_version": 57,
            "index": {
                "0:1": { "attrs": [{"MacroExport": null}], "name": "Baz" }
            },
            "external_crates": {
                "1": { "name": "std", "path": "/rustc/library/std" },
                "2": { "name": "alloc", "path": "/rustc/library/alloc" }
            }
        });
        normalize_for_v56(&mut value, 57);

        // attrs emptied
        assert_eq!(value["index"]["0:1"]["attrs"], json!([]));
        // paths stripped from external_crates
        assert!(value["external_crates"]["1"].get("path").is_none());
        assert!(value["external_crates"]["2"].get("path").is_none());
        // names preserved
        assert_eq!(value["external_crates"]["1"]["name"], json!("std"));
        assert_eq!(value["external_crates"]["2"]["name"], json!("alloc"));
    }

    #[test]
    fn normalize_v58_also_strips_external_crate_paths() {
        // Future format versions should also get path stripping
        let mut value = json!({
            "format_version": 58,
            "external_crates": {
                "0": { "name": "foo", "path": "/some/path" }
            }
        });
        normalize_for_v56(&mut value, 58);
        assert!(value["external_crates"]["0"].get("path").is_none());
    }

    // ========== Deserialization roundtrip tests ==========

    /// Build a minimal but valid rustdoc JSON value that rustdoc-types 0.56 can parse.
    /// Uses Id(u32) format: map keys are plain numbers like "0", "1".
    ///
    /// When `format_version < 56`, the `target` field is omitted to simulate
    /// older format JSON (the normalizer should inject a dummy target).
    fn minimal_rustdoc_json(format_version: u64) -> serde_json::Value {
        let mut value = json!({
            "root": 0,
            "crate_version": "1.0.0",
            "includes_private": false,
            "index": {
                "0": {
                    "id": 0,
                    "crate_id": 0,
                    "name": "test_crate",
                    "span": null,
                    "visibility": "public",
                    "docs": "A test crate",
                    "links": {},
                    "attrs": [],
                    "deprecation": null,
                    "inner": {
                        "module": {
                            "is_crate": true,
                            "items": [1],
                            "is_stripped": false
                        }
                    }
                },
                "1": {
                    "id": 1,
                    "crate_id": 0,
                    "name": "MyStruct",
                    "span": null,
                    "visibility": "public",
                    "docs": "A test struct",
                    "links": {},
                    "attrs": [],
                    "deprecation": null,
                    "inner": {
                        "struct": {
                            "kind": "unit",
                            "generics": {
                                "params": [],
                                "where_predicates": []
                            },
                            "impls": []
                        }
                    }
                }
            },
            "paths": {
                "0": {
                    "crate_id": 0,
                    "path": ["test_crate"],
                    "kind": "module"
                },
                "1": {
                    "crate_id": 0,
                    "path": ["test_crate", "MyStruct"],
                    "kind": "struct"
                }
            },
            "external_crates": {
                "2": { "name": "std", "html_root_url": null }
            },
            "format_version": format_version
        });

        // Format 56+ includes the target field; older versions don't
        if format_version >= 56 {
            value.as_object_mut().unwrap().insert(
                "target".to_string(),
                json!({
                    "triple": "x86_64-unknown-linux-gnu",
                    "target_features": []
                }),
            );
        }

        value
    }

    #[test]
    fn roundtrip_v56_deserializes_successfully() {
        let mut value = minimal_rustdoc_json(56);
        normalize_for_v56(&mut value, 56);
        let krate: rustdoc_types::Crate =
            serde_json::from_value(value).expect("v56 JSON should deserialize after normalization");
        assert_eq!(krate.index.len(), 2);
    }

    #[test]
    fn roundtrip_v53_with_string_attrs_deserializes() {
        // Format 53 used plain string attrs -- the tagged enum Attribute in 0.56
        // would fail to deserialize these, but strip_attrs empties them first
        let mut value = minimal_rustdoc_json(53);
        value["index"]["1"]["attrs"] = json!(["#[derive(Debug)]", "#[allow(unused)]"]);

        normalize_for_v56(&mut value, 53);
        let krate: rustdoc_types::Crate = serde_json::from_value(value)
            .expect("v53 JSON with string attrs should deserialize after normalization");
        assert_eq!(krate.index.len(), 2);
    }

    #[test]
    fn roundtrip_v57_with_external_crate_path_deserializes() {
        // Format 57 adds ExternalCrate.path which doesn't exist in 0.56
        let mut value = minimal_rustdoc_json(57);
        value["external_crates"]["2"]
            .as_object_mut()
            .unwrap()
            .insert("path".to_string(), json!("/rustc/library/std"));

        normalize_for_v56(&mut value, 57);
        let krate: rustdoc_types::Crate = serde_json::from_value(value)
            .expect("v57 JSON with ExternalCrate.path should deserialize after normalization");
        assert_eq!(krate.index.len(), 2);
        // Verify external crate is preserved (minus the path field)
        assert!(krate.external_crates.values().any(|c| c.name == "std"));
    }

    #[test]
    fn roundtrip_v53_without_normalization_fails() {
        // v53 JSON with string attrs and no target field should fail without normalization
        let mut value = minimal_rustdoc_json(53);
        value["index"]["1"]["attrs"] = json!(["#[derive(Debug)]"]);

        let result: Result<rustdoc_types::Crate, _> = serde_json::from_value(value);
        assert!(
            result.is_err(),
            "v53 JSON should fail without normalization (missing target, string attrs)"
        );
    }

    #[test]
    fn roundtrip_v57_extra_fields_ignored_by_serde() {
        // serde's default behavior ignores unknown fields, so ExternalCrate.path
        // doesn't cause a deserialization error. Our strip_external_crate_paths
        // is defensive in case deny_unknown_fields is ever added.
        let mut value = minimal_rustdoc_json(57);
        value["external_crates"]["2"]
            .as_object_mut()
            .unwrap()
            .insert("path".to_string(), json!("/rustc/library/std"));

        let result: Result<rustdoc_types::Crate, _> = serde_json::from_value(value);
        assert!(result.is_ok(), "serde ignores unknown fields by default");
    }
}
