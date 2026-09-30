use crate::AppState;
use crate::auth::AuthUser;
use axum::{
    Json,
    extract::{Path, Request, State},
    http::StatusCode,
    response::{IntoResponse, Response},
};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    path::PathBuf,
    sync::Arc,
    time::{Duration, SystemTime},
};

#[derive(Debug, Serialize, Deserialize, Clone)]
#[allow(non_snake_case)]
pub struct LockInfo {
    pub ID: String,
    pub Operation: Option<String>,
    pub Info: Option<String>,
    pub Who: Option<String>,
    pub Version: Option<String>,
    pub Created: Option<String>,
}

/// Default lock lifetime. Long enough for any sane apply, short enough that a
/// CI job that died mid-run doesn't wedge a workspace for the rest of the day.
const DEFAULT_LOCK_TTL: Duration = Duration::from_secs(2 * 60 * 60);

/// Lock lifetime from `TERRARIUM_LOCK_TTL` (seconds). `0` disables expiry;
/// unset or unparsable falls back to [`DEFAULT_LOCK_TTL`].
pub fn lock_ttl_from_env() -> Option<Duration> {
    let Ok(raw) = std::env::var("TERRARIUM_LOCK_TTL") else {
        return Some(DEFAULT_LOCK_TTL);
    };
    match raw.trim().parse::<u64>() {
        Ok(0) => None,
        Ok(secs) => Some(Duration::from_secs(secs)),
        Err(_) => {
            tracing::warn!("Invalid TERRARIUM_LOCK_TTL {raw:?}, using default of {}s", DEFAULT_LOCK_TTL.as_secs());
            Some(DEFAULT_LOCK_TTL)
        }
    }
}

/// A held lock plus the server-side time it was acquired. Expiry is measured
/// from `acquired_at`, never from the client-supplied `Created`, so a client
/// with a skewed clock can neither dodge nor trigger expiry.
#[derive(Clone)]
struct ActiveLock {
    info: LockInfo,
    acquired_at: SystemTime,
}

/// Outcome of [`LockContainer::try_acquire`].
pub enum Acquire {
    /// The workspace was unlocked. Carries the lock as stored.
    Acquired(LockInfo),
    /// The previous lock had outlived the TTL and was replaced. `stale` is the
    /// old lock, whose holder can no longer push with its lock ID.
    TookOver { lock: LockInfo, stale: LockInfo },
    /// The workspace is held by a live lock.
    Held(LockInfo),
}

/// Outcome of [`LockContainer::release`].
pub enum Release {
    Released(LockInfo),
    NotHeld,
    /// Held under a different ID. Carries the current holder.
    Mismatch(LockInfo),
}

pub struct LockContainer {
    locks: Arc<DashMap<String, ActiveLock>>,
    pub persisted: PathBuf,
    ttl: Option<Duration>,
}

impl Clone for LockContainer {
    fn clone(&self) -> Self {
        Self {
            locks: Arc::clone(&self.locks),
            persisted: self.persisted.clone(),
            ttl: self.ttl,
        }
    }
}

impl LockContainer {
    pub fn new(dir: PathBuf, ttl: Option<Duration>) -> Self {
        if !dir.exists() {
            std::fs::create_dir_all(&dir).unwrap();
        }
        Self {
            locks: Arc::new(DashMap::new()),
            persisted: dir,
            ttl,
        }
    }

    pub fn ttl(&self) -> Option<Duration> {
        self.ttl
    }

    fn expired(&self, lock: &ActiveLock) -> bool {
        self.ttl.is_some_and(|ttl| lock.acquired_at.elapsed().is_ok_and(|age| age >= ttl))
    }

    pub fn get(&self, name: &str) -> Option<LockInfo> {
        self.locks.get(name).map(|x| x.info.clone())
    }

    /// Whether the lock on `name` has outlived the TTL and may be taken over.
    pub fn is_expired(&self, name: &str) -> bool {
        self.locks.get(name).is_some_and(|l| self.expired(&l))
    }

    pub fn remove(&self, name: &str) -> Option<LockInfo> {
        self.locks.remove(name).map(|x| x.1.info)
    }

    /// An expired lock still verifies: expiry only lets someone else take the
    /// lock over, it doesn't revoke the holder until that happens.
    pub fn verify_lock(&self, name: &str, lock_id: &str) -> bool {
        self.get(name).is_some_and(|info| info.ID == lock_id)
    }

    pub fn list(&self) -> HashMap<String, LockInfo> {
        self.locks
            .iter()
            .map(|e| (e.key().clone(), e.value().info.clone()))
            .collect()
    }

    /// Number of held locks that have outlived the TTL.
    pub fn expired_count(&self) -> usize {
        self.locks.iter().filter(|e| self.expired(e.value())).count()
    }

    /// Every lock ever acquired for a workspace, newest first.
    ///
    /// Persisted `.lock` files are written on acquire and never deleted on
    /// release, so they double as an audit trail of who locked the state, for
    /// which operation, and when.
    pub fn history(&self, name: &str) -> Vec<LockInfo> {
        let dir = self.persisted.join(name);
        let mut entries: Vec<LockInfo> = std::fs::read_dir(&dir)
            .map(|rd| {
                rd.filter_map(|e| e.ok())
                    .filter(|e| e.path().extension().is_some_and(|x| x == "lock"))
                    .filter_map(|e| std::fs::read_to_string(e.path()).ok())
                    .filter_map(|s| serde_json::from_str::<LockInfo>(&s).ok())
                    .collect()
            })
            .unwrap_or_default();
        entries.sort_by(|a, b| b.Created.cmp(&a.Created));
        entries
    }

    /// Acquire the lock on `name`, taking over an expired one.
    ///
    /// The check and the insert happen under a single map entry, so two
    /// concurrent callers can never both acquire the same workspace.
    pub fn try_acquire(&self, name: &str, mut info: LockInfo) -> Acquire {
        use dashmap::mapref::entry::Entry;

        if info.Created.is_none() {
            info.Created = Some(chrono::Utc::now().to_rfc3339());
        }
        let fresh = ActiveLock { info: info.clone(), acquired_at: SystemTime::now() };
        let outcome = match self.locks.entry(name.to_string()) {
            Entry::Vacant(v) => {
                v.insert(fresh);
                Acquire::Acquired(info.clone())
            }
            Entry::Occupied(mut o) if self.expired(o.get()) => Acquire::TookOver {
                lock: info.clone(),
                stale: std::mem::replace(o.get_mut(), fresh).info,
            },
            Entry::Occupied(o) => Acquire::Held(o.get().info.clone()),
        };
        if !matches!(outcome, Acquire::Held(_)) {
            self.record(name, &info);
        }
        outcome
    }

    /// Release the lock on `name`.
    ///
    /// With `expected_id`, only the lock with that ID is released: after a
    /// takeover, the stale holder finishing late must not release the lock of
    /// whoever took over. Without one it is a force-unlock.
    pub fn release(&self, name: &str, expected_id: Option<&str>) -> Release {
        let Some(id) = expected_id else {
            return self.remove(name).map_or(Release::NotHeld, Release::Released);
        };
        match self.locks.remove_if(name, |_, l| l.info.ID == id) {
            Some((_, l)) => Release::Released(l.info),
            None => self.get(name).map_or(Release::NotHeld, Release::Mismatch),
        }
    }

    /// Append an acquired lock to the audit trail. The file name is generated
    /// server-side: `Created` is client input and must not pick a path. A
    /// failed write is logged, not fatal — the lock itself lives in memory.
    fn record(&self, name: &str, info: &LockInfo) {
        // The UUID suffix keeps names unique on clocks with coarse resolution.
        let file = format!(
            "{}-{}.lock",
            chrono::Utc::now().format("%Y%m%dT%H%M%S%.9fZ"),
            uuid::Uuid::new_v4().simple()
        );
        let result = serde_json::to_vec(info)
            .map_err(std::io::Error::other)
            .and_then(|json| crate::state::atomic_write(&self.persisted.join(name).join(file), &json));
        if let Err(e) = result {
            tracing::error!("💥 Failed to record lock history for {name}: {e}");
        }
    }
}

/// List all active locks
pub async fn list_locks(
    State(app): State<AppState>,
    _auth: AuthUser,
) -> Json<HashMap<String, LockInfo>> {
    Json(app.locks.list())
}

fn validate_name(name: &str) -> Result<(), StatusCode> {
    if name.is_empty() || name.starts_with('/') || name.ends_with('/') || name.contains('\\') {
        return Err(StatusCode::BAD_REQUEST);
    }
    for component in name.split('/') {
        if component.is_empty() || component == ".." || component == "." {
            return Err(StatusCode::BAD_REQUEST);
        }
    }
    Ok(())
}

/// Acquire `name` for `user`, shared by the POST and LOCK routes.
///
/// On conflict the current holder's lock info is returned as the body, which
/// Terraform and OpenTofu print ("Lock Info: ID … Who …"), so a blocked user
/// can see who holds the lock without asking around.
async fn acquire(app: &AppState, name: &str, info: LockInfo, user: &str) -> Response {
    let operation = crate::observability::lock_operation(info.Operation.as_ref());
    if app.state.is_archived(name) {
        tracing::info!("📦 State {name} is archived, rejecting lock");
        metrics::counter!("terrarium_lock_acquires_total", "workspace" => name.to_string(), "result" => "forbidden", "operation" => operation).increment(1);
        return StatusCode::FORBIDDEN.into_response();
    }

    let lock = match app.locks.try_acquire(name, info) {
        Acquire::Held(holder) => {
            tracing::info!("🔒 Already existing lock for {name}");
            metrics::counter!("terrarium_lock_acquires_total", "workspace" => name.to_string(), "result" => "conflict", "operation" => operation).increment(1);
            metrics::counter!("terrarium_lock_conflicts_total", "workspace" => name.to_string()).increment(1);
            return (StatusCode::CONFLICT, Json(holder)).into_response();
        }
        Acquire::Acquired(lock) => lock,
        Acquire::TookOver { lock, stale } => {
            tracing::warn!(
                "⏰ Lock {} on {name} (held by {}) outlived the TTL, taken over by {user}",
                stale.ID,
                stale.Who.as_deref().unwrap_or("unknown"),
            );
            metrics::counter!("terrarium_lock_expirations_total", "workspace" => name.to_string()).increment(1);
            crate::observability::observe_lock_age(name, &stale);
            app.webhooks.fire("lock.expire", name, None, user).await;
            lock
        }
    };

    tracing::info!("🔒 Acquired lock for {name}: {lock:#?}");
    metrics::counter!("terrarium_lock_acquires_total", "workspace" => name.to_string(), "result" => "ok", "operation" => operation).increment(1);
    app.webhooks.fire("lock.acquire", name, None, user).await;
    Json(lock).into_response()
}

/// Create a lock on state
pub async fn lock(
    AuthUser(user): AuthUser,
    State(app): State<AppState>,
    Path(name): Path<String>,
    Json(info): Json<LockInfo>,
) -> Response {
    if validate_name(&name).is_err() {
        return StatusCode::BAD_REQUEST.into_response();
    }
    tracing::info!("🔒 Trying to lock {name}");
    acquire(&app, &name, info, &user.username).await
}

/// Fallback for the non-standard LOCK and UNLOCK HTTP methods that the
/// Terraform HTTP backend sends by default, so no `lock_method`/`unlock_method`
/// overrides are needed in backend configs.
pub async fn lock_method_compat(
    AuthUser(user): AuthUser,
    State(app): State<AppState>,
    Path(name): Path<String>,
    req: Request,
) -> impl IntoResponse {
    if validate_name(&name).is_err() {
        return StatusCode::BAD_REQUEST.into_response();
    }

    match req.method().as_str() {
        "LOCK" => {
            tracing::info!("🔒 Trying to lock {name}");
            let bytes = match axum::body::to_bytes(req.into_body(), usize::MAX).await {
                Ok(b) => b,
                Err(_) => return StatusCode::BAD_REQUEST.into_response(),
            };
            let info: LockInfo = match serde_json::from_slice(&bytes) {
                Ok(i) => i,
                Err(_) => return StatusCode::BAD_REQUEST.into_response(),
            };
            acquire(&app, &name, info, &user.username).await
        }
        "UNLOCK" => {
            let body = axum::body::to_bytes(req.into_body(), usize::MAX).await.unwrap_or_default();
            release(&app, &name, &body, &user.username).await
        }
        _ => StatusCode::METHOD_NOT_ALLOWED.into_response(),
    }
}

/// Release `name`, shared by the DELETE and UNLOCK routes.
///
/// Terraform sends the lock it holds as the body; when that carries an ID,
/// only a matching lock is released (409 with the current holder otherwise).
/// An empty body is a force-unlock, as `terra remote state unlock` sends.
async fn release(app: &AppState, name: &str, body: &[u8], user: &str) -> Response {
    tracing::info!("🔓 Unlocking {name}");
    let expected_id = serde_json::from_slice::<LockInfo>(body).ok().map(|i| i.ID);
    match app.locks.release(name, expected_id.as_deref()) {
        Release::Released(info) => {
            tracing::info!("🔓 Unlocked {name}");
            metrics::counter!("terrarium_lock_releases_total", "workspace" => name.to_string(), "result" => "ok").increment(1);
            crate::observability::observe_lock_age(name, &info);
            app.webhooks.fire("lock.release", name, None, user).await;
            Json(info).into_response()
        }
        Release::Mismatch(holder) => {
            tracing::warn!("🔓 Refusing to unlock {name}: lock is now held under ID {}", holder.ID);
            metrics::counter!("terrarium_lock_releases_total", "workspace" => name.to_string(), "result" => "mismatch").increment(1);
            (StatusCode::CONFLICT, Json(holder)).into_response()
        }
        Release::NotHeld => {
            metrics::counter!("terrarium_lock_releases_total", "workspace" => name.to_string(), "result" => "not_found").increment(1);
            StatusCode::NOT_FOUND.into_response()
        }
    }
}

/// Unlock a state
pub async fn unlock(
    AuthUser(user): AuthUser,
    State(app): State<AppState>,
    Path(name): Path<String>,
    body: axum::body::Bytes,
) -> Response {
    if validate_name(&name).is_err() {
        return StatusCode::BAD_REQUEST.into_response();
    }
    release(&app, &name, &body, &user.username).await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn container(ttl: Option<Duration>) -> (LockContainer, PathBuf) {
        let dir = std::env::temp_dir().join(format!("terrarium-lock-test-{}", uuid::Uuid::new_v4()));
        (LockContainer::new(dir.clone(), ttl), dir)
    }

    fn info(id: &str) -> LockInfo {
        LockInfo { ID: id.into(), Operation: None, Info: None, Who: Some(format!("{id}@ci")), Version: None, Created: None }
    }

    /// Pretend the current lock on `name` was acquired `age` ago.
    fn age(c: &LockContainer, name: &str, age: Duration) {
        c.locks.get_mut(name).unwrap().acquired_at = SystemTime::now() - age;
    }

    #[test]
    fn live_lock_blocks_and_expired_lock_is_taken_over() {
        let (c, dir) = container(Some(Duration::from_secs(60)));
        assert!(matches!(c.try_acquire("ws", info("a")), Acquire::Acquired(_)));
        assert!(matches!(c.try_acquire("ws", info("b")), Acquire::Held(h) if h.ID == "a"));
        assert!(!c.is_expired("ws"));

        age(&c, "ws", Duration::from_secs(61));
        assert!(c.is_expired("ws"));
        assert_eq!(c.expired_count(), 1);
        // Expired but not yet taken over: the holder can still push.
        assert!(c.verify_lock("ws", "a"));

        match c.try_acquire("ws", info("b")) {
            Acquire::TookOver { lock, stale } => assert_eq!((lock.ID.as_str(), stale.ID.as_str()), ("b", "a")),
            _ => panic!("expected takeover"),
        }
        assert!(!c.verify_lock("ws", "a"), "stale holder must not push after takeover");
        assert!(c.verify_lock("ws", "b"));
        assert_eq!(c.history("ws").len(), 2, "both acquisitions are in the audit trail");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn expiry_can_be_disabled() {
        let (c, dir) = container(None);
        c.try_acquire("ws", info("a"));
        age(&c, "ws", Duration::from_secs(365 * 24 * 3600));
        assert!(!c.is_expired("ws"));
        assert!(matches!(c.try_acquire("ws", info("b")), Acquire::Held(_)));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn stale_holder_cannot_release_the_new_lock() {
        let (c, dir) = container(Some(Duration::from_secs(60)));
        c.try_acquire("ws", info("a"));
        age(&c, "ws", Duration::from_secs(120));
        c.try_acquire("ws", info("b"));

        assert!(matches!(c.release("ws", Some("a")), Release::Mismatch(h) if h.ID == "b"));
        assert!(c.verify_lock("ws", "b"), "lock b must survive a's late unlock");
        assert!(matches!(c.release("ws", Some("b")), Release::Released(r) if r.ID == "b"));
        assert!(matches!(c.release("ws", Some("b")), Release::NotHeld));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn release_without_id_is_a_force_unlock() {
        let (c, dir) = container(None);
        c.try_acquire("ws", info("a"));
        assert!(matches!(c.release("ws", None), Release::Released(r) if r.ID == "a"));
        assert!(c.get("ws").is_none());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn missing_created_is_filled_and_history_filename_is_server_chosen() {
        let (c, dir) = container(None);
        let mut hostile = info("a");
        hostile.Created = Some("../../escape".into());
        c.try_acquire("ws", hostile);
        let files: Vec<_> = std::fs::read_dir(dir.join("ws")).unwrap().filter_map(|e| e.ok()).map(|e| e.file_name()).collect();
        assert_eq!(files.len(), 1);
        assert!(!dir.join("escape.lock").exists() && !dir.parent().unwrap().join("escape.lock").exists());

        let (c2, dir2) = container(None);
        match c2.try_acquire("ws", info("b")) {
            Acquire::Acquired(lock) => assert!(lock.Created.is_some()),
            _ => panic!(),
        }
        let _ = std::fs::remove_dir_all(dir);
        let _ = std::fs::remove_dir_all(dir2);
    }

    #[test]
    fn concurrent_acquires_have_exactly_one_winner() {
        let (c, dir) = container(None);
        let winners: usize = std::thread::scope(|s| {
            let handles: Vec<_> = (0..16)
                .map(|i| {
                    let c = c.clone();
                    s.spawn(move || matches!(c.try_acquire("ws", info(&format!("t{i}"))), Acquire::Acquired(_)) as usize)
                })
                .collect();
            handles.into_iter().map(|h| h.join().unwrap()).sum()
        });
        assert_eq!(winners, 1);
        let _ = std::fs::remove_dir_all(dir);
    }
}
