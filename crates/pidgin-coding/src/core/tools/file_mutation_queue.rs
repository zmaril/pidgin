//! Serialize file mutations targeting the same file; parallelize across files.
//!
//! Ported from pi's `core/tools/file-mutation-queue.ts`. pi chains promises per
//! resolved path so two edits to the same file run one-at-a-time (FIFO) while
//! edits to different files proceed concurrently. The key is the file's
//! `realpath`, falling back to the absolute-resolved path when the file does
//! not exist yet (`ENOENT`/`ENOTDIR`).
//!
//! Rust analog: a global map from key path to a per-key [`tokio::sync::Mutex`]
//! (which is FIFO-fair). [`with_file_mutation_queue`] canonicalizes the path,
//! acquires that key's mutex, runs the caller's future, and releases it. Empty
//! entries are pruned once no task references them, mirroring pi deleting the
//! queue entry when it drains.

use std::collections::HashMap;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex};

use tokio::sync::Mutex as AsyncMutex;

/// The per-key mutex registry. The outer `std::sync::Mutex` guards the map for
/// the brief insert/lookup/prune critical sections; the inner
/// `tokio::sync::Mutex` is the actual per-file serialization lock held across
/// the awaited mutation.
static FILE_MUTATION_QUEUES: LazyLock<Mutex<HashMap<PathBuf, Arc<AsyncMutex<()>>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Resolve the queue key for `file_path`, mirroring pi's `getMutationQueueKey`:
/// `realpath` of the absolute path, falling back to the absolute path itself
/// when it does not exist yet.
fn mutation_queue_key(file_path: &Path) -> PathBuf {
    match std::fs::canonicalize(file_path) {
        Ok(resolved) => resolved,
        Err(_) => absolute_resolve(file_path),
    }
}

/// Absolute-resolve without touching the filesystem (pi's `resolve()` fallback).
fn absolute_resolve(file_path: &Path) -> PathBuf {
    std::path::absolute(file_path).unwrap_or_else(|_| file_path.to_path_buf())
}

/// Run `f` while holding the per-file mutation lock for `file_path`.
///
/// Operations on the same (resolved) path run serially and in acquisition
/// order; operations on distinct paths run concurrently. Returns whatever the
/// future resolves to.
pub async fn with_file_mutation_queue<F, T>(file_path: &Path, f: F) -> T
where
    F: Future<Output = T>,
{
    let key = mutation_queue_key(file_path);

    let lock = {
        let mut map = FILE_MUTATION_QUEUES.lock().unwrap();
        map.entry(key.clone())
            .or_insert_with(|| Arc::new(AsyncMutex::new(())))
            .clone()
    };

    let guard = lock.lock().await;
    let result = f.await;
    drop(guard);

    // Prune the entry if nobody else is waiting on it. Under the map lock, no
    // other task can be mid-`or_insert_with`, so the strong count is stable:
    // `2` means only the map's copy and our local `lock` remain (no waiters).
    {
        let mut map = FILE_MUTATION_QUEUES.lock().unwrap();
        if let Some(existing) = map.get(&key) {
            if Arc::strong_count(existing) <= 2 {
                map.remove(&key);
            }
        }
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::tools::test_support::TempDir;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    #[tokio::test]
    async fn same_path_serializes() {
        let dir = TempDir::new("fmq-serial");
        let path = dir.write("f.txt", "");

        // Tracks concurrent occupancy; if the two ops overlapped, `max` > 1.
        let active = Arc::new(AtomicUsize::new(0));
        let max_seen = Arc::new(AtomicUsize::new(0));

        let mk = |p: PathBuf, active: Arc<AtomicUsize>, max_seen: Arc<AtomicUsize>| async move {
            with_file_mutation_queue(&p, async {
                let now = active.fetch_add(1, Ordering::SeqCst) + 1;
                max_seen.fetch_max(now, Ordering::SeqCst);
                tokio::time::sleep(Duration::from_millis(30)).await;
                active.fetch_sub(1, Ordering::SeqCst);
            })
            .await;
        };

        let a = tokio::spawn(mk(path.clone(), active.clone(), max_seen.clone()));
        let b = tokio::spawn(mk(path.clone(), active.clone(), max_seen.clone()));
        a.await.unwrap();
        b.await.unwrap();

        assert_eq!(max_seen.load(Ordering::SeqCst), 1, "ops must not overlap");
    }

    #[tokio::test]
    async fn different_paths_interleave() {
        let dir = TempDir::new("fmq-parallel");
        let p1 = dir.write("a.txt", "");
        let p2 = dir.write("b.txt", "");

        // Both tasks meet at a 2-party barrier *inside* the critical section.
        // If the queue serialized across distinct files, the second could never
        // reach the barrier and this would deadlock (guarded by a timeout).
        let barrier = Arc::new(tokio::sync::Barrier::new(2));

        let mk = |p: PathBuf, barrier: Arc<tokio::sync::Barrier>| async move {
            with_file_mutation_queue(&p, async move {
                barrier.wait().await;
            })
            .await;
        };

        let fut = async {
            let a = tokio::spawn(mk(p1, barrier.clone()));
            let b = tokio::spawn(mk(p2, barrier.clone()));
            a.await.unwrap();
            b.await.unwrap();
        };

        tokio::time::timeout(Duration::from_secs(5), fut)
            .await
            .expect("distinct paths must run concurrently, not deadlock");
    }

    #[tokio::test]
    async fn entry_pruned_after_use() {
        let dir = TempDir::new("fmq-prune");
        let path = dir.write("c.txt", "");
        let key = mutation_queue_key(&path);

        with_file_mutation_queue(&path, async {}).await;

        let map = FILE_MUTATION_QUEUES.lock().unwrap();
        assert!(!map.contains_key(&key), "drained entry should be pruned");
    }

    #[tokio::test]
    async fn returns_inner_value() {
        let dir = TempDir::new("fmq-value");
        let path = dir.write("d.txt", "");
        let v = with_file_mutation_queue(&path, async { 40 + 2 }).await;
        assert_eq!(v, 42);
    }
}
