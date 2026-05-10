//! Background watcher that reloads `db.json` and `usage.json` whenever the
//! files change on disk.
//!
//! Why this exists: the CLI is allowed to write the DB directly while the
//! server is running. Without a watcher, the server's in-memory `ArcSwap`
//! would silently go stale until the next restart. With this watcher, the
//! server's snapshot reflects CLI mutations within ~150ms.
//!
//! Design notes:
//! - We debounce events so a burst of writes (atomic rename produces
//!   create/modify in quick succession) only triggers one reload.
//! - We never block the watcher thread on the reload itself; we hand the work
//!   off to a tokio task.
//! - Reload errors are logged at WARN and the previous snapshot is kept.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use tokio::sync::mpsc;
use tokio::time::sleep;
use tracing::{debug, info, warn};

use crate::db::Db;

const DEBOUNCE_MS: u64 = 100;

/// Spawn a background watcher tied to the lifetime of `db`. Drops silently if
/// the watch cannot be installed (e.g. on platforms without inotify) — the
/// server still works, it just won't auto-reload from CLI writes.
pub fn spawn_watcher(db: Arc<Db>) {
    let data_dir = db.data_dir.clone();
    let db_path = db.db_path.clone();
    let usage_path = db.usage_path.clone();

    tokio::spawn(async move {
        if let Err(e) = run_watcher(db, data_dir, db_path, usage_path).await {
            warn!(error = %e, "db watcher exited; CLI writes will not auto-reload until restart");
        }
    });
}

async fn run_watcher(
    db: Arc<Db>,
    data_dir: PathBuf,
    db_path: PathBuf,
    usage_path: PathBuf,
) -> anyhow::Result<()> {
    // notify is sync; bridge into tokio via an mpsc channel.
    let (tx, mut rx) = mpsc::unbounded_channel::<PathBuf>();

    let db_path_clone = db_path.clone();
    let usage_path_clone = usage_path.clone();

    let mut watcher = RecommendedWatcher::new(
        move |res: Result<Event, notify::Error>| {
            let Ok(event) = res else { return };
            if !matches!(
                event.kind,
                EventKind::Create(_) | EventKind::Modify(_) | EventKind::Remove(_)
            ) {
                return;
            }
            for p in event.paths {
                if p == db_path_clone || p == usage_path_clone {
                    let _ = tx.send(p);
                }
            }
        },
        notify::Config::default(),
    )
    .context("create file watcher")?;

    // Watch the directory rather than the files themselves: editors and
    // atomic-rename writes (our `write_json_atomic`) often replace the inode,
    // which a file-level watch would miss.
    watcher
        .watch(&data_dir, RecursiveMode::NonRecursive)
        .with_context(|| format!("watch data dir at {}", data_dir.display()))?;

    info!(path = %data_dir.display(), "db file watcher active");

    while let Some(path) = rx.recv().await {
        // Drain rapid bursts (atomic rename → CREATE + MODIFY).
        let _ = drain_for(&mut rx, Duration::from_millis(DEBOUNCE_MS)).await;
        debug!(path = %path.display(), "db change detected, reloading");

        if path == db_path {
            match crate::db::reload_app_db(&db_path).await {
                Ok(next) => {
                    db.snapshot.store(Arc::new(next));
                    info!("db.json reloaded into in-memory snapshot");
                }
                Err(e) => warn!(error = %e, "failed to reload db.json"),
            }
        } else if path == usage_path {
            match crate::db::reload_usage_db(&usage_path).await {
                Ok(next) => {
                    db.usage_snapshot.store(Arc::new(next));
                    debug!("usage.json reloaded into in-memory snapshot");
                }
                Err(e) => warn!(error = %e, "failed to reload usage.json"),
            }
        }
    }

    // Keep the watcher alive for the lifetime of the task.
    drop(watcher);
    Ok(())
}

async fn drain_for(rx: &mut mpsc::UnboundedReceiver<PathBuf>, dur: Duration) {
    let deadline = sleep(dur);
    tokio::pin!(deadline);
    loop {
        tokio::select! {
            _ = &mut deadline => break,
            maybe = rx.recv() => {
                if maybe.is_none() { break; }
            }
        }
    }
}
