//! WH3MM user-flow pack discovery.
//!
//! The legacy TypeScript app stores executable node graphs as JSON files under
//! `whmmflows\` inside mod packs. Full graph execution is a larger subsystem;
//! this module owns the first stable core boundary: identifying and
//! summarizing flow files in real pack indexes.

use std::path::Path;

use serde_json::Value;

use crate::pack::{PackReadOptions, read_pack_index, read_packed_file_payload};
use crate::ports::CoreResult;

/// TS flow-file prefix used inside packs.
pub const WHMM_FLOW_FILE_PREFIX: &str = "whmmflows\\";

/// Lossy per-pack summary of WH3MM flow files.
///
/// A malformed flow file is recorded in `read_errors` instead of aborting the
/// whole pack summary. That mirrors the TypeScript launch path, which catches
/// flow errors per file and keeps processing later flows from the same pack.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct WhmmFlowPackSummary {
    /// Successfully parsed flow files, in pack-index order.
    pub files: Vec<WhmmFlowFileSummary>,
    /// Per-flow read/parse errors, in pack-index order.
    pub read_errors: Vec<WhmmFlowFileReadError>,
}

/// Lightweight graph metadata needed by UI badges and future execution gates.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WhmmFlowFileSummary {
    /// Packed file path, usually `whmmflows\name.json`.
    pub name: String,
    /// TS `isGraphEnabled`: whether this graph exposes a user enable toggle.
    pub has_graph_enable_toggle: bool,
    /// TS `graphStartsEnabled`: the graph's default toggle state.
    pub graph_starts_enabled: bool,
    /// Number of serialized graph nodes found in `nodes`.
    pub node_count: usize,
    /// Number of serialized graph connections found in `connections`.
    pub connection_count: usize,
    /// Number of option entries found in `options`.
    pub option_count: usize,
    /// User-facing flow options with stable IDs.
    pub options: Vec<WhmmFlowOptionSummary>,
}

/// One user-configurable flow option.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WhmmFlowOptionSummary {
    /// Stable placeholder ID, used in `{{optionId}}` substitutions.
    pub id: String,
    /// User-facing option name, falling back to `id` when absent.
    pub name: String,
    /// TS option kind such as `textbox`, `range`, or `checkbox`.
    pub kind: String,
    /// Optional user-facing description.
    pub description: Option<String>,
    /// Default value serialized as a UI-friendly string.
    pub default_value: Option<String>,
}

/// Error for one flow file inside an otherwise readable pack.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WhmmFlowFileReadError {
    /// Packed flow file path.
    pub name: String,
    /// Human-readable diagnostic.
    pub message: String,
}

/// Returns true when a packed file is a WH3MM flow JSON candidate.
#[must_use]
pub fn is_whmm_flow_file_name(name: &str) -> bool {
    name.starts_with(WHMM_FLOW_FILE_PREFIX) || name.starts_with("whmmflows/")
}

/// Reads flow-file names from a pack index.
///
/// This intentionally avoids parsing the graph JSON or executing anything.
/// It is safe to use for UI badges/status and for deciding whether the future
/// flow executor has work to do.
///
/// # Errors
///
/// Returns [`crate::ports::CoreError`] when the pack index cannot be read.
pub fn read_whmm_flow_file_names(pack_path: impl AsRef<Path>) -> CoreResult<Vec<String>> {
    let index = read_pack_index(pack_path, &PackReadOptions::default())?;
    Ok(index
        .files
        .iter()
        .filter(|entry| is_whmm_flow_file_name(&entry.name))
        .map(|entry| entry.name.clone())
        .collect())
}

/// Reads and summarizes WH3MM flow files from a pack.
///
/// Only the serialized graph shape is inspected. This does not execute nodes,
/// modify graph data, apply user flow options, or write generated packs.
///
/// # Errors
///
/// Returns [`crate::ports::CoreError`] when the pack index cannot be read.
/// Individual payload/JSON errors are returned in
/// [`WhmmFlowPackSummary::read_errors`].
pub fn read_whmm_flow_pack_summary(pack_path: impl AsRef<Path>) -> CoreResult<WhmmFlowPackSummary> {
    let pack_path = pack_path.as_ref();
    let index = read_pack_index(pack_path, &PackReadOptions::default())?;
    let mut summary = WhmmFlowPackSummary::default();

    for entry in index
        .files
        .iter()
        .filter(|entry| is_whmm_flow_file_name(&entry.name))
    {
        let result = read_packed_file_payload(pack_path, entry)
            .map_err(|error| error.message)
            .and_then(|payload| summarize_flow_payload(&entry.name, &payload));

        match result {
            Ok(file) => summary.files.push(file),
            Err(message) => summary.read_errors.push(WhmmFlowFileReadError {
                name: entry.name.clone(),
                message,
            }),
        }
    }

    Ok(summary)
}

fn summarize_flow_payload(name: &str, payload: &[u8]) -> Result<WhmmFlowFileSummary, String> {
    let value = serde_json::from_slice::<Value>(payload)
        .map_err(|error| format!("failed to parse flow JSON: {error}"))?;
    let object = value
        .as_object()
        .ok_or_else(|| "flow JSON root is not an object".to_string())?;

    let options = object
        .get("options")
        .and_then(Value::as_array)
        .map_or_else(Vec::new, |values| {
            values.iter().filter_map(flow_option_summary).collect()
        });

    Ok(WhmmFlowFileSummary {
        name: name.to_string(),
        has_graph_enable_toggle: object
            .get("isGraphEnabled")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        graph_starts_enabled: object
            .get("graphStartsEnabled")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        node_count: array_len(object.get("nodes")),
        connection_count: array_len(object.get("connections")),
        option_count: array_len(object.get("options")),
        options,
    })
}

fn array_len(value: Option<&Value>) -> usize {
    value.and_then(Value::as_array).map_or(0, Vec::len)
}

fn flow_option_summary(value: &Value) -> Option<WhmmFlowOptionSummary> {
    let object = value.as_object()?;
    let id = object.get("id")?.as_str()?.trim();
    if id.is_empty() {
        return None;
    }

    Some(WhmmFlowOptionSummary {
        id: id.to_string(),
        name: object
            .get("name")
            .and_then(Value::as_str)
            .filter(|name| !name.trim().is_empty())
            .unwrap_or(id)
            .to_string(),
        kind: object
            .get("type")
            .and_then(Value::as_str)
            .filter(|kind| !kind.trim().is_empty())
            .unwrap_or("unknown")
            .to_string(),
        description: object
            .get("description")
            .and_then(Value::as_str)
            .filter(|description| !description.trim().is_empty())
            .map(str::to_string),
        default_value: object.get("value").map(flow_option_value_label),
    })
}

fn flow_option_value_label(value: &Value) -> String {
    match value {
        Value::String(value) => value.clone(),
        Value::Bool(value) => value.to_string(),
        Value::Number(value) => value.to_string(),
        Value::Null => "null".to_string(),
        Value::Array(_) | Value::Object(_) => value.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    use crate::pack::{PackFileWrite, build_pfh5_pack_bytes};

    use super::{
        WhmmFlowFileSummary, WhmmFlowOptionSummary, is_whmm_flow_file_name,
        read_whmm_flow_file_names, read_whmm_flow_pack_summary,
    };

    static TEST_FILE_COUNTER: AtomicU64 = AtomicU64::new(0);

    #[test]
    fn identifies_whmm_flow_file_names() {
        assert!(is_whmm_flow_file_name("whmmflows\\flow.json"));
        assert!(is_whmm_flow_file_name("whmmflows/flow.json"));
        assert!(!is_whmm_flow_file_name("script\\flow.json"));
        assert!(!is_whmm_flow_file_name("WHMMFLOWS\\flow.json"));
    }

    #[test]
    fn reads_flow_file_names_from_pack_index() {
        let path = temp_pack_path("flow-index");
        fs::write(
            &path,
            build_pfh5_pack_bytes(&[
                PackFileWrite {
                    name: "whmmflows\\campaign.json".to_string(),
                    payload: br#"{"nodes":[]}"#.to_vec(),
                },
                PackFileWrite {
                    name: "script\\ignored.lua".to_string(),
                    payload: b"-- ignored".to_vec(),
                },
                PackFileWrite {
                    name: "whmmflows\\battle.json".to_string(),
                    payload: br#"{"nodes":[]}"#.to_vec(),
                },
            ])
            .unwrap(),
        )
        .unwrap();

        assert_eq!(
            read_whmm_flow_file_names(&path).unwrap(),
            [
                "whmmflows\\campaign.json".to_string(),
                "whmmflows\\battle.json".to_string()
            ]
        );

        fs::remove_file(path).ok();
    }

    #[test]
    fn summarizes_flow_graphs_from_pack() {
        let path = temp_pack_path("flow-summary");
        fs::write(
            &path,
            build_pfh5_pack_bytes(&[
                PackFileWrite {
                    name: "whmmflows\\campaign.json".to_string(),
                    payload: br#"{
                        "version": "1",
                        "timestamp": 123,
                        "nodes": [{ "id": "n1" }, { "id": "n2" }],
                        "connections": [{ "id": "c1", "sourceId": "n1", "targetId": "n2" }],
                        "options": [
                            {
                                "id": "radius",
                                "name": "Radius",
                                "description": "How far to search",
                                "type": "range",
                                "value": 3,
                                "min": 0,
                                "max": 10,
                                "step": 1
                            },
                            {
                                "id": "enabled",
                                "name": "Enabled",
                                "type": "checkbox",
                                "value": true
                            }
                        ],
                        "metadata": { "nodeCount": 99, "connectionCount": 99 },
                        "isGraphEnabled": true,
                        "graphStartsEnabled": false
                    }"#
                    .to_vec(),
                },
                PackFileWrite {
                    name: "script\\ignored.lua".to_string(),
                    payload: b"-- ignored".to_vec(),
                },
            ])
            .unwrap(),
        )
        .unwrap();

        let summary = read_whmm_flow_pack_summary(&path).unwrap();

        assert_eq!(summary.read_errors, Vec::new());
        assert_eq!(
            summary.files,
            vec![WhmmFlowFileSummary {
                name: "whmmflows\\campaign.json".to_string(),
                has_graph_enable_toggle: true,
                graph_starts_enabled: false,
                node_count: 2,
                connection_count: 1,
                option_count: 2,
                options: vec![
                    WhmmFlowOptionSummary {
                        id: "radius".to_string(),
                        name: "Radius".to_string(),
                        kind: "range".to_string(),
                        description: Some("How far to search".to_string()),
                        default_value: Some("3".to_string()),
                    },
                    WhmmFlowOptionSummary {
                        id: "enabled".to_string(),
                        name: "Enabled".to_string(),
                        kind: "checkbox".to_string(),
                        description: None,
                        default_value: Some("true".to_string()),
                    },
                ],
            }]
        );

        fs::remove_file(path).ok();
    }

    #[test]
    fn records_flow_parse_errors_without_dropping_valid_summaries() {
        let path = temp_pack_path("flow-parse-errors");
        fs::write(
            &path,
            build_pfh5_pack_bytes(&[
                PackFileWrite {
                    name: "whmmflows\\valid.json".to_string(),
                    payload: br#"{"nodes":[],"connections":[],"options":[]}"#.to_vec(),
                },
                PackFileWrite {
                    name: "whmmflows\\bad.json".to_string(),
                    payload: b"{not-json".to_vec(),
                },
            ])
            .unwrap(),
        )
        .unwrap();

        let summary = read_whmm_flow_pack_summary(&path).unwrap();

        assert_eq!(
            summary.files,
            vec![WhmmFlowFileSummary {
                name: "whmmflows\\valid.json".to_string(),
                has_graph_enable_toggle: false,
                graph_starts_enabled: false,
                node_count: 0,
                connection_count: 0,
                option_count: 0,
                options: Vec::new(),
            }]
        );
        assert_eq!(summary.read_errors.len(), 1);
        assert_eq!(summary.read_errors[0].name, "whmmflows\\bad.json");
        assert!(
            summary.read_errors[0]
                .message
                .starts_with("failed to parse flow JSON:")
        );

        fs::remove_file(path).ok();
    }

    fn temp_pack_path(test_name: &str) -> PathBuf {
        let counter = TEST_FILE_COUNTER.fetch_add(1, Ordering::Relaxed);
        let mut path = std::env::temp_dir();
        path.push(format!(
            "wh3mm-core-flows-{test_name}-{}-{counter}.pack",
            std::process::id()
        ));
        path
    }
}
