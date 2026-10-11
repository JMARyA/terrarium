//! Structured diff between two terraform state versions.
//!
//! The diff is computed once into a structured [`StateDiff`] and rendered
//! separately by each consumer — the CLI (`remote state diff`) renders it with
//! ANSI colour, the web UI renders it as HTML. Keeping the computation here
//! means both stay in sync.

use rediff::FacetDiff;
use std::collections::HashMap;

use crate::tfstate::{TfResource, TfState};

/// A single resource-level change between two versions.
pub enum Change {
    /// Resource present only in the newer version.
    Added(String),
    /// Resource present only in the older version.
    Removed(String),
    /// Resource present in both, with differing instances.
    Modified {
        addr: String,
        instances: Vec<InstanceChange>,
    },
}

/// A changed instance within a modified resource.
pub struct InstanceChange {
    pub index: usize,
    /// True when the parent resource has more than one instance (count/for_each).
    pub multi: bool,
    /// Formatted attribute diff (one logical change per line).
    pub diff: String,
}

/// A change to a root-module output.
pub struct OutputChange {
    pub name: String,
    pub kind: OutputChangeKind,
    /// Sensitive in either version. Values of sensitive outputs are never
    /// part of a diff, only the fact that they changed.
    pub sensitive: bool,
}

pub enum OutputChangeKind {
    Added,
    Removed,
    /// `diff` is the formatted value diff; `None` for sensitive outputs, or
    /// when only the sensitivity flag changed.
    Modified { diff: Option<String> },
}

/// The result of diffing two state blobs.
pub enum StateDiff {
    /// Both blobs parsed as terraform state — a structural, resource-aware diff.
    Structured {
        /// `Some((from, to))` when the terraform version changed.
        terraform_version: Option<(String, String)>,
        /// `Some((from, to))` when serial or terraform version changed.
        serial: Option<(u64, u64)>,
        changes: Vec<Change>,
        outputs: Vec<OutputChange>,
    },
    /// At least one blob wasn't terraform state but both were valid JSON —
    /// a generic structural JSON diff, pre-formatted.
    Raw(String),
    /// A blob could not be parsed as JSON at all.
    Error(String),
}

impl StateDiff {
    /// True when a structured diff found no differences whatsoever.
    pub fn is_empty(&self) -> bool {
        matches!(
            self,
            StateDiff::Structured {
                terraform_version: None,
                serial: None,
                changes,
                outputs,
            } if changes.is_empty() && outputs.is_empty()
        )
    }
}

/// Diff two state blobs (`from` is the older version, `to` the newer).
pub fn diff_states(from: &[u8], to: &[u8]) -> StateDiff {
    let from_s = String::from_utf8_lossy(from);
    let to_s = String::from_utf8_lossy(to);

    let a_state = facet_json::from_str::<TfState>(&from_s).ok();
    let b_state = facet_json::from_str::<TfState>(&to_s).ok();

    if let (Some(a), Some(b)) = (a_state, b_state) {
        let a_map: HashMap<String, &TfResource> =
            a.resources.iter().map(|r| (r.address(), r)).collect();
        let b_map: HashMap<String, &TfResource> =
            b.resources.iter().map(|r| (r.address(), r)).collect();

        let mut addrs: Vec<String> = a_map.keys().chain(b_map.keys()).cloned().collect();
        addrs.sort();
        addrs.dedup();

        let meta_changed = a.serial != b.serial || a.terraform_version != b.terraform_version;
        let terraform_version = (a.terraform_version != b.terraform_version)
            .then(|| (a.terraform_version.clone(), b.terraform_version.clone()));
        let serial = meta_changed.then_some((a.serial, b.serial));

        let mut changes = Vec::new();
        for addr in &addrs {
            match (a_map.get(addr), b_map.get(addr)) {
                (None, Some(_)) => changes.push(Change::Added(addr.clone())),
                (Some(_), None) => changes.push(Change::Removed(addr.clone())),
                (Some(ra), Some(rb)) if ra.instances != rb.instances => {
                    let multi = ra.instances.len() > 1;
                    let mut instances = Vec::new();
                    for (i, (ia, ib)) in ra.instances.iter().zip(rb.instances.iter()).enumerate() {
                        if ia.attributes != ib.attributes {
                            instances.push(InstanceChange {
                                index: i,
                                multi,
                                diff: format!("{}", ia.attributes.diff(&ib.attributes)),
                            });
                        }
                    }
                    changes.push(Change::Modified {
                        addr: addr.clone(),
                        instances,
                    });
                }
                _ => {}
            }
        }

        StateDiff::Structured {
            terraform_version,
            serial,
            changes,
            outputs: diff_outputs(&a, &b),
        }
    } else {
        let a = match facet_json::from_str::<facet_value::Value>(&from_s) {
            Ok(v) => v,
            Err(e) => return StateDiff::Error(format!("source version is not valid JSON: {e}")),
        };
        let b = match facet_json::from_str::<facet_value::Value>(&to_s) {
            Ok(v) => v,
            Err(e) => return StateDiff::Error(format!("target version is not valid JSON: {e}")),
        };
        StateDiff::Raw(format!("{}", a.diff(&b)))
    }
}

fn diff_outputs(a: &TfState, b: &TfState) -> Vec<OutputChange> {
    let mut names: Vec<&String> = a.outputs.keys().chain(b.outputs.keys()).collect();
    names.sort();
    names.dedup();
    names
        .into_iter()
        .filter_map(|name| {
            let (kind, sensitive) = match (a.outputs.get(name), b.outputs.get(name)) {
                (None, Some(o)) => (OutputChangeKind::Added, o.sensitive),
                (Some(o), None) => (OutputChangeKind::Removed, o.sensitive),
                (Some(x), Some(y)) if x.value != y.value || x.sensitive != y.sensitive => {
                    let sensitive = x.sensitive || y.sensitive;
                    let diff = (!sensitive && x.value != y.value).then(|| format!("{}", x.value.diff(&y.value)));
                    (OutputChangeKind::Modified { diff }, sensitive)
                }
                _ => return None,
            };
            Some(OutputChange { name: name.clone(), kind, sensitive })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state(outputs: &str) -> Vec<u8> {
        format!(r#"{{"version":4,"terraform_version":"1.9.0","serial":1,"lineage":"l","outputs":{{{outputs}}},"resources":[]}}"#).into_bytes()
    }

    #[test]
    fn output_changes_hide_sensitive_values() {
        let a = state(r#""url":{"value":"a.example","type":"string","sensitive":false},"pw":{"value":"hunter2","type":"string","sensitive":true},"gone":{"value":1,"type":"number","sensitive":false}"#);
        let b = state(r#""url":{"value":"b.example","type":"string","sensitive":false},"pw":{"value":"hunter3","type":"string","sensitive":true},"new":{"value":2,"type":"number","sensitive":false}"#);
        let StateDiff::Structured { outputs, changes, .. } = diff_states(&a, &b) else { panic!("expected structured diff") };
        assert!(changes.is_empty());
        let by = |n: &str| outputs.iter().find(|o| o.name == n).unwrap();
        assert!(matches!(by("new").kind, OutputChangeKind::Added));
        assert!(matches!(by("gone").kind, OutputChangeKind::Removed));
        let OutputChangeKind::Modified { diff: Some(d) } = &by("url").kind else { panic!("url should carry a diff") };
        assert!(d.contains("b.example"));
        let pw = by("pw");
        assert!(pw.sensitive && matches!(pw.kind, OutputChangeKind::Modified { diff: None }));
    }

    #[test]
    fn unchanged_outputs_are_not_reported() {
        let a = state(r#""url":{"value":"a","type":"string","sensitive":false}"#);
        assert!(diff_states(&a, &a).is_empty());
    }
}
