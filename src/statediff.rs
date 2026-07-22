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

/// The result of diffing two state blobs.
pub enum StateDiff {
    /// Both blobs parsed as terraform state — a structural, resource-aware diff.
    Structured {
        /// `Some((from, to))` when the terraform version changed.
        terraform_version: Option<(String, String)>,
        /// `Some((from, to))` when serial or terraform version changed.
        serial: Option<(u64, u64)>,
        changes: Vec<Change>,
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
            } if changes.is_empty()
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
