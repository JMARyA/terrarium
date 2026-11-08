//! Ctrl+C handling for commands that wrap `tofu`.
//!
//! The terminal delivers SIGINT to the whole foreground process group, so
//! `tofu` sees every Ctrl+C on its own: the first makes it stop gracefully and
//! release the state lock, the second makes it exit immediately. `terra` must
//! therefore *not* die on SIGINT while `tofu` runs — it owns the pipe `tofu`
//! writes its JSON stream to, and a closed pipe kills `tofu` (SIGPIPE) halfway
//! through its shutdown, before it has unlocked the state.
//!
//! While no lock-holding `tofu` runs (the confirmation prompt, the policy
//! check), Ctrl+C aborts `terra` directly after removing its temp files.

use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use chrono::{DateTime, Utc};
use colored::Colorize as _;

static INTERRUPTS: AtomicUsize = AtomicUsize::new(0);
static TOFU_RUNNING: AtomicBool = AtomicBool::new(false);
static TEMP_FILES: Mutex<Vec<PathBuf>> = Mutex::new(Vec::new());
static STARTED: Mutex<Option<DateTime<Utc>>> = Mutex::new(None);

/// Take over SIGINT for the rest of the process. Must run inside the
/// (multi-threaded) Tokio runtime.
pub fn install() {
    *STARTED.lock().unwrap_or_else(|p| p.into_inner()) = Some(Utc::now());
    tokio::spawn(async {
        while tokio::signal::ctrl_c().await.is_ok() {
            let n = INTERRUPTS.fetch_add(1, Ordering::SeqCst) + 1;
            // A third press aborts even while settling, in case the server
            // hangs; the lock then expires by TTL.
            if !TOFU_RUNNING.load(Ordering::SeqCst) || n >= 3 {
                abort();
            }
            if n == 1 {
                eprintln!(
                    "\n{} waiting for OpenTofu to stop and release the state lock…\n{}",
                    "Interrupt received:".bold().yellow(),
                    "Press Ctrl+C again to stop immediately (the lock may stay held).".dimmed()
                );
            } else {
                eprintln!("\n{}", "Stopping immediately…".bold().yellow());
            }
        }
    });
}

/// Whether Ctrl+C was pressed at any point.
pub fn interrupted() -> bool {
    INTERRUPTS.load(Ordering::SeqCst) > 0
}

/// Remove `path` if `terra` is aborted before it gets to clean up itself.
pub fn track_temp(path: PathBuf) {
    TEMP_FILES.lock().unwrap_or_else(|p| p.into_inner()).push(path);
}

/// Call right after spawning a `tofu` that may take the state lock. From now
/// until [`settle`], Ctrl+C is left to `tofu` and `terra` keeps draining its
/// output.
pub fn tofu_started() {
    // Each `tofu` counts its own presses, as it does itself.
    INTERRUPTS.store(0, Ordering::SeqCst);
    TOFU_RUNNING.store(true, Ordering::SeqCst);
}

fn abort() -> ! {
    for path in TEMP_FILES.lock().unwrap_or_else(|p| p.into_inner()).drain(..) {
        let _ = std::fs::remove_file(path);
    }
    eprintln!("\n{}", "Cancelled.".yellow());
    std::process::exit(130);
}

/// Call once the `tofu` child has exited. After an interrupted run, releases
/// the state lock if `tofu` was stopped before it could. Blocks; call from
/// synchronous code inside the runtime.
///
/// Ctrl+C stays deferred until this returns: `tofu` exits the instant it sees
/// a second interrupt, often before `terra` has handled that same signal.
pub fn settle() {
    if interrupted() {
        tokio::task::block_in_place(|| tokio::runtime::Handle::current().block_on(release_abandoned_lock()));
    }
    TOFU_RUNNING.store(false, Ordering::SeqCst);
}

/// Only a lock that is provably ours is released: held under this machine's
/// `user@host` (how OpenTofu fills `Who`) and created after `terra` started.
/// The release names the lock ID, so a lock someone else took in the meantime
/// is refused by the server rather than broken.
async fn release_abandoned_lock() {
    let Some(config) = crate::config::load_quiet() else { return };
    let dir = std::env::current_dir().unwrap_or_default();
    let Some(workspace) = crate::policy_client::detect_workspace(&dir, &config.url) else { return };
    let client = crate::client::TerrariumClient::new(config.url, config.username, config.password);

    let Ok(locks) = client.list_locks().await else { return };
    let Some(lock) = locks.get(&workspace) else { return }; // tofu released it

    let started = *STARTED.lock().unwrap_or_else(|p| p.into_inner());
    let created = lock
        .Created
        .as_deref()
        .and_then(|c| DateTime::parse_from_rfc3339(c).ok())
        .map(|c| c.with_timezone(&Utc));
    let ours = local_who().is_some_and(|who| lock.Who.as_deref() == Some(who.as_str()))
        && matches!((created, started), (Some(c), Some(s)) if c >= s);

    if !ours {
        return;
    }
    match client.release_lock(&workspace, lock).await {
        Ok(()) => eprintln!("{} {}", "Released state lock".green(), workspace.bold()),
        Err(e) => eprintln!(
            "{} could not release the state lock on {workspace}: {e}\n  Release it with: terra force-unlock {}",
            "warning:".bold().yellow(),
            lock.ID
        ),
    }
}

/// `user@host` as OpenTofu records it in a lock's `Who`.
fn local_who() -> Option<String> {
    let user = std::env::var("USER").or_else(|_| std::env::var("LOGNAME")).ok()?;
    let host = std::fs::read_to_string("/proc/sys/kernel/hostname")
        .or_else(|_| std::fs::read_to_string("/etc/hostname"))
        .ok()
        .or_else(|| std::env::var("HOSTNAME").ok())?;
    Some(format!("{user}@{}", host.trim()))
}
