//! Read models the pages share: version metadata, workspace health, and the
//! activity feed. All derived from what's on disk — terrarium keeps no
//! separate event log.

use chrono::{DateTime, Duration, Utc};

use crate::AppState;
use crate::lock::LockInfo;

pub struct VersionInfo {
    pub v: u32,
    pub size: u64,
    /// When the version was pushed.
    pub at: Option<DateTime<Utc>>,
    /// Who pushed it; unknown for versions that predate version metadata.
    pub user: Option<String>,
    pub restored_from: Option<u32>,
}

/// Every version of `name`, oldest first. Uses the recorded metadata, and
/// the file's size and mtime for versions written before it existed.
pub fn versions(app: &AppState, name: &str) -> Vec<VersionInfo> {
    let dir = app.state.versions_dir.join(name);
    app.state
        .list_versions(name)
        .into_iter()
        .map(|v| match app.state.version_meta(name, v) {
            Some(m) => VersionInfo {
                v,
                size: m.size,
                at: parse_time(Some(&m.pushed_at)),
                user: Some(m.user),
                restored_from: m.restored_from,
            },
            None => {
                let meta = std::fs::metadata(dir.join(v.to_string())).ok();
                VersionInfo {
                    v,
                    size: meta.as_ref().map(|m| m.len()).unwrap_or(0),
                    at: meta.and_then(|m| m.modified().ok()).map(DateTime::<Utc>::from),
                    user: None,
                    restored_from: None,
                }
            }
        })
        .collect()
}

pub fn parse_time(s: Option<&str>) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(s?).ok().map(|t| t.with_timezone(&Utc))
}

// ── Health ──────────────────────────────────────────────────────────────

/// A workspace with no push for this long has "gone quiet" — often retired
/// infrastructure nobody archived.
pub const QUIET_AFTER_DAYS: i64 = 90;

/// When `name` was last pushed to.
pub fn last_push(app: &AppState, name: &str) -> Option<DateTime<Utc>> {
    let v = *app.state.list_versions(name).last()?;
    match app.state.version_meta(name, v) {
        Some(m) => parse_time(Some(&m.pushed_at)),
        None => std::fs::metadata(app.state.versions_dir.join(name).join(v.to_string()))
            .and_then(|m| m.modified())
            .ok()
            .map(DateTime::<Utc>::from),
    }
}

/// Days since the last push, when that's past [`QUIET_AFTER_DAYS`].
pub fn quiet_days(app: &AppState, name: &str) -> Option<i64> {
    let days = (Utc::now() - last_push(app, name)?).num_days();
    (days >= QUIET_AFTER_DAYS).then_some(days)
}

/// The one-glance status of a workspace, most urgent first.
pub enum Health {
    Archived,
    ExpiredLock,
    Locked,
    Deny(usize),
    Unchecked,
    Warn(usize),
    Quiet(i64),
    Ok,
}

impl Health {
    pub fn of(app: &AppState, name: &str, archived: bool) -> Self {
        if archived {
            return Health::Archived;
        }
        if app.locks.get(name).is_some() {
            return if app.locks.is_expired(name) { Health::ExpiredLock } else { Health::Locked };
        }
        match app.violations.get(name) {
            Some(r) if r.error.is_some() => Health::Unchecked,
            Some(r) if r.deny_count() > 0 => Health::Deny(r.deny_count()),
            Some(r) if r.warn_count() > 0 => Health::Warn(r.warn_count()),
            _ => quiet_days(app, name).map_or(Health::Ok, Health::Quiet),
        }
    }

    pub fn dot(&self) -> &'static str {
        match self {
            Health::Archived => "archived",
            Health::ExpiredLock => "expired",
            Health::Locked => "locked",
            Health::Deny(_) => "deny",
            Health::Unchecked | Health::Warn(_) => "warn",
            Health::Quiet(_) => "quiet",
            Health::Ok => "ok",
        }
    }

    pub fn label(&self) -> String {
        match self {
            Health::Archived => "archived".into(),
            Health::ExpiredLock => "lock expired".into(),
            Health::Locked => "locked".into(),
            Health::Deny(n) => format!("{n} violation{}", plural(*n)),
            Health::Unchecked => "policy not checked".into(),
            Health::Warn(n) => format!("{n} warning{}", plural(*n)),
            Health::Quiet(d) => format!("quiet for {d} days"),
            Health::Ok => "idle".into(),
        }
    }
}

pub fn plural(n: usize) -> &'static str {
    if n == 1 { "" } else { "s" }
}

// ── Activity ────────────────────────────────────────────────────────────

pub enum EventKind {
    Push { version: u32, size: u64, prev: Option<u64>, user: Option<String>, restored_from: Option<u32> },
    Lock { who: String, operation: Option<String> },
}

pub struct Event {
    pub workspace: String,
    pub at: DateTime<Utc>,
    pub kind: EventKind,
}

/// Pushes and lock acquisitions for one workspace, newest first.
///
/// Releases aren't recorded anywhere, so they don't appear.
pub fn events_for(app: &AppState, name: &str, limit: usize) -> Vec<Event> {
    let mut events = Vec::new();
    let versions = versions(app, name);
    let start = versions.len().saturating_sub(limit);
    for (i, v) in versions.iter().enumerate().skip(start) {
        let Some(at) = v.at else { continue };
        let prev = i.checked_sub(1).map(|p| versions[p].size);
        events.push(Event {
            workspace: name.to_string(),
            at,
            kind: EventKind::Push { version: v.v, size: v.size, prev, user: v.user.clone(), restored_from: v.restored_from },
        });
    }
    for info in app.locks.history(name).into_iter().take(limit) {
        events.push(lock_event(name, info));
    }
    events.sort_by(|a, b| b.at.cmp(&a.at));
    events.truncate(limit);
    events
}

fn lock_event(name: &str, info: LockInfo) -> Event {
    Event {
        workspace: name.to_string(),
        at: parse_time(info.Created.as_deref()).unwrap_or(DateTime::<Utc>::UNIX_EPOCH),
        kind: EventKind::Lock {
            who: info.Who.unwrap_or_else(|| "unknown".into()),
            operation: info.Operation,
        },
    }
}

/// The most recent events across every workspace, newest first.
pub fn all_events(app: &AppState, names: &[String], limit: usize) -> Vec<Event> {
    let mut events: Vec<Event> = names.iter().flat_map(|n| events_for(app, n, limit)).collect();
    events.sort_by(|a, b| b.at.cmp(&a.at));
    events.truncate(limit);
    events
}

/// Pushes per day for the last `days` days, oldest first — today is last.
pub fn pushes_per_day(app: &AppState, names: &[String], days: usize) -> Vec<u32> {
    let mut buckets = vec![0u32; days];
    let today = Utc::now().date_naive();
    for name in names {
        for v in versions(app, name) {
            let Some(at) = v.at else { continue };
            let age = (today - at.date_naive()).num_days();
            if (0..days as i64).contains(&age) {
                buckets[days - 1 - age as usize] += 1;
            }
        }
    }
    buckets
}

/// Number of pushes in the last 24 hours.
pub fn pushes_since(app: &AppState, names: &[String], window: Duration) -> usize {
    let cutoff = Utc::now() - window;
    names
        .iter()
        .flat_map(|n| versions(app, n))
        .filter(|v| v.at.is_some_and(|t| t >= cutoff))
        .count()
}
