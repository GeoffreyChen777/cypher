//! File mentions: a cached, ignore-filtered path index per checkout and the
//! bounded fuzzy ranking over it.

use super::*;

/// File mentions should remain responsive even in very large checkouts.
const FILE_SEARCH_MAX_RESULTS: usize = 8;
/// A dead network mount must not leave the composer search spinning forever.
const FILE_SEARCH_TIMEOUT: Duration = Duration::from_secs(6);
/// File-mention indexes are rebuilt at most every 10s per checkout, so a
/// keystroke ranks against an in-memory index instead of re-walking the tree.
const FILE_INDEX_TTL: Duration = Duration::from_secs(10);
/// Cap on indexed entries per checkout (bounds index memory + rebuild time).
const FILE_INDEX_MAX_ENTRIES: usize = 250_000;
/// Initial capacity for the bounded ranking heap. Ranking itself only ever
/// holds the top [`FILE_SEARCH_MAX_RESULTS`] hits, so an empty query (which
/// matches every indexed path) never sorts the whole tree.
const RANK_BUFFER: usize = 1_024;

/// One indexed path in a checkout, pre-tokenized for nucleo ranking so a query
/// only re-ranks the cached haystacks (never re-walks the filesystem).
struct IndexedPath {
    path: String,
    haystack: nucleo_matcher::Utf32String,
    is_dir: bool,
}

/// A checkout's ignore-filtered path index, valid for [`FILE_INDEX_TTL`].
pub(super) struct FileIndex {
    entries: Vec<IndexedPath>,
    built: std::time::Instant,
}

pub(super) type FileIndexCache = std::sync::Mutex<HashMap<PathBuf, std::sync::Arc<FileIndex>>>;

impl Repos {
    /// Search a checkout's files and directories by fuzzy relative path. The
    /// `ignore` walker honors `.gitignore`, `.ignore`, and global git excludes.
    /// Dotfiles remain searchable; only repository metadata is always pruned.
    ///
    /// The first search per checkout (per [`FILE_INDEX_TTL`]) builds a parallel
    /// index of canonical-root paths; subsequent queries within the TTL are pure
    /// in-memory rankings over that index. The per-root gate still serializes
    /// concurrent rebuilds of one checkout.
    pub async fn search_files(
        &self,
        root: PathBuf,
        query: String,
        featured_paths: Vec<String>,
    ) -> Result<Vec<FileSearchMatch>, EngineError> {
        let deadline = tokio::time::Instant::now() + FILE_SEARCH_TIMEOUT;
        let gate = {
            let mut searches = self
                .inner
                .file_searches
                .lock()
                .map_err(|_| EngineError::Other("file search registry poisoned".into()))?;
            if let Some(gate) = searches.get(&root).and_then(std::sync::Weak::upgrade) {
                gate
            } else {
                let gate = std::sync::Arc::new(tokio::sync::Mutex::new(()));
                searches.insert(root.clone(), std::sync::Arc::downgrade(&gate));
                gate
            }
        };
        let gate = tokio::time::timeout_at(deadline, gate.lock_owned())
            .await
            .map_err(|_| EngineError::Other("file search timed out".into()))?;
        let cancelled = std::sync::Arc::new(AtomicBool::new(false));
        let _cancel_on_drop = CancelOnDrop(cancelled.clone());
        let worker_cancelled = cancelled.clone();
        let cache = self.inner.clone();
        let (tx, rx) = tokio::sync::oneshot::channel();
        std::thread::Builder::new()
            .name("file-search".into())
            .spawn(move || {
                let _gate = gate;
                let _ = tx.send(search_files_cached(
                    &cache.file_index,
                    &root,
                    &query,
                    &featured_paths,
                    || worker_cancelled.load(Ordering::Relaxed),
                ));
            })
            .map_err(|e| EngineError::Other(format!("file search failed: {e}")))?;
        match tokio::time::timeout_at(deadline, rx).await {
            Ok(Ok(result)) => result,
            Ok(Err(_)) => Err(EngineError::Other("file search worker exited".into())),
            Err(_) => Err(EngineError::Other("file search timed out".into())),
        }
    }
}

/// Nucleo path score (higher is better; `match_paths` mode is tuned for source
/// trees — the basename dominates, a path subsequence still works, and smart
/// case + normalization handle mixed input like `Cmp Rs` for `composer.rs`).
/// Test-only: production ranking reuses the index's pre-tokenized haystacks.
#[cfg(test)]
pub(super) fn nucleo_path_score(query: &str, candidate: &str) -> Option<u32> {
    let mut matcher = nucleo_matcher::Matcher::new({
        let mut config = nucleo_matcher::Config::DEFAULT;
        config.set_match_paths();
        config
    });
    nucleo_matcher::pattern::Pattern::parse(
        query,
        nucleo_matcher::pattern::CaseMatching::Smart,
        nucleo_matcher::pattern::Normalization::Smart,
    )
    .score(
        nucleo_matcher::Utf32String::from(candidate).slice(..),
        &mut matcher,
    )
}

type RankedFileMatch = (Option<usize>, u32, String, bool);

fn compare_file_matches(
    empty_query: bool,
    (featured_a, score_a, path_a, dir_a): &RankedFileMatch,
    (featured_b, score_b, path_b, dir_b): &RankedFileMatch,
) -> std::cmp::Ordering {
    featured_a
        .is_none()
        .cmp(&featured_b.is_none())
        .then_with(|| featured_a.cmp(featured_b))
        // nucleo scores higher-is-better (the old subsequence scorer was
        // lower-is-better), so the comparison flips here.
        .then_with(|| score_b.cmp(score_a))
        .then_with(|| {
            if empty_query {
                path_a.split('/').count().cmp(&path_b.split('/').count())
            } else {
                std::cmp::Ordering::Equal
            }
        })
        .then_with(|| {
            if empty_query {
                dir_a.cmp(dir_b)
            } else {
                dir_b.cmp(dir_a)
            }
        })
        .then_with(|| path_a.len().cmp(&path_b.len()))
        .then_with(|| path_a.cmp(path_b))
}

/// [`BinaryHeap`] wrapper so ranking can keep a bounded top-K: the heap root is
/// always the current worst candidate, which `pop()` discards once the heap
/// exceeds [`FILE_SEARCH_MAX_RESULTS`]. `Ord` follows [`compare_file_matches`]
/// (a total order via the final path tie-break), so `into_sorted_vec()` yields
/// best-first results.
#[derive(Eq, PartialEq)]
struct RankedCandidate {
    empty_query: bool,
    ranked: RankedFileMatch,
}

impl PartialOrd for RankedCandidate {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for RankedCandidate {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        compare_file_matches(self.empty_query, &self.ranked, &other.ranked)
    }
}

#[cfg(test)]
pub(super) fn search_files_blocking(
    root: &Path,
    query: &str,
    featured_paths: &[String],
) -> Result<Vec<FileSearchMatch>, EngineError> {
    search_files_blocking_with_cancel(root, query, featured_paths, || false)
}

#[cfg(test)]
pub(super) fn search_files_blocking_with_cancel<F: Fn() -> bool + Sync>(
    root: &Path,
    query: &str,
    featured_paths: &[String],
    cancelled: F,
) -> Result<Vec<FileSearchMatch>, EngineError> {
    let root = canonical_search_root(root)?;
    let index = walk_file_index(&root, &cancelled)?;
    Ok(rank_file_matches(&index, &root, query, featured_paths))
}

pub(super) fn search_files_cached<F: Fn() -> bool + Sync>(
    cache: &FileIndexCache,
    root: &Path,
    query: &str,
    featured_paths: &[String],
    cancelled: F,
) -> Result<Vec<FileSearchMatch>, EngineError> {
    let root = canonical_search_root(root)?;
    let fresh = cache
        .lock()
        .ok()
        .and_then(|indexes| indexes.get(&root).cloned())
        .filter(|index| index.built.elapsed() < FILE_INDEX_TTL);
    let index = match fresh {
        Some(index) => index,
        None => {
            let index = std::sync::Arc::new(FileIndex {
                entries: walk_file_index(&root, &cancelled)?,
                built: std::time::Instant::now(),
            });
            if let Ok(mut indexes) = cache.lock() {
                // Evict stale indexes while we have the lock: rebuilds prune.
                indexes.retain(|_, index| index.built.elapsed() < FILE_INDEX_TTL);
                indexes.insert(root.clone(), index.clone());
            }
            index
        }
    };
    if cancelled() {
        return Err(EngineError::Other("file search cancelled".into()));
    }
    Ok(rank_file_matches(
        &index.entries,
        &root,
        query,
        featured_paths,
    ))
}

fn canonical_search_root(root: &Path) -> Result<PathBuf, EngineError> {
    std::fs::canonicalize(root)
        .map_err(|e| EngineError::Other(format!("could not search workspace: {e}")))
}

/// One parallel `ignore` walk of the checkout (honors `.gitignore`/`.ignore`/
/// global git excludes; dotfiles stay searchable; `.git` is pruned). Batches of
/// [`IndexedPath`] entries are appended to a shared sink; the walk stops early
/// on cancellation or once [`FILE_INDEX_MAX_ENTRIES`] is reached.
fn walk_file_index<F: Fn() -> bool + Sync>(
    root: &Path,
    cancelled: &F,
) -> Result<Vec<IndexedPath>, EngineError> {
    if cancelled() {
        return Err(EngineError::Other("file search cancelled".into()));
    }
    let threads = std::thread::available_parallelism()
        .map(|n| n.get().min(12))
        .unwrap_or(4);
    let collected: std::sync::Mutex<Vec<IndexedPath>> = std::sync::Mutex::new(Vec::new());
    let was_cancelled = AtomicBool::new(false);
    ignore::WalkBuilder::new(root)
        .hidden(false)
        .git_ignore(true)
        .git_global(true)
        .git_exclude(true)
        .threads(threads)
        // Depth 0 is the project root itself: if someone deliberately points a
        // project at a library, that is their call.
        .filter_entry(|entry| {
            entry.depth() == 0 || (entry.file_name() != ".git" && !is_media_library(entry.path()))
        })
        .build_parallel()
        .run(|| {
            const BATCH: usize = 512;
            struct Batch<'a> {
                items: Vec<IndexedPath>,
                sink: &'a std::sync::Mutex<Vec<IndexedPath>>,
            }
            impl Drop for Batch<'_> {
                fn drop(&mut self) {
                    if self.items.is_empty() {
                        return;
                    }
                    if let Ok(mut all) = self.sink.lock() {
                        all.append(&mut self.items);
                    }
                }
            }
            let mut batch = Batch {
                items: Vec::with_capacity(BATCH),
                sink: &collected,
            };
            let was_cancelled = &was_cancelled;
            Box::new(move |entry| {
                if was_cancelled.load(Ordering::Relaxed) {
                    return ignore::WalkState::Quit;
                }
                if cancelled() {
                    was_cancelled.store(true, Ordering::Relaxed);
                    return ignore::WalkState::Quit;
                }
                let entry = match entry {
                    Ok(entry) => entry,
                    Err(err) => {
                        tracing::debug!(%err, "file mention index skipped entry");
                        return ignore::WalkState::Continue;
                    }
                };
                let path = entry.path();
                if path == root {
                    return ignore::WalkState::Continue;
                }
                let Ok(relative) = path.strip_prefix(root) else {
                    return ignore::WalkState::Continue;
                };
                let relative = relative.to_string_lossy().replace('\\', "/");
                if relative == ".git" || relative.starts_with(".git/") {
                    return ignore::WalkState::Continue;
                }
                batch.items.push(IndexedPath {
                    haystack: nucleo_matcher::Utf32String::from(relative.as_str()),
                    path: relative,
                    is_dir: entry.file_type().is_some_and(|kind| kind.is_dir()),
                });
                if batch.items.len() >= BATCH
                    && let Ok(mut all) = batch.sink.lock()
                {
                    all.append(&mut batch.items);
                    if all.len() >= FILE_INDEX_MAX_ENTRIES {
                        return ignore::WalkState::Quit;
                    }
                }
                ignore::WalkState::Continue
            })
        });
    if was_cancelled.load(Ordering::Relaxed) || cancelled() {
        return Err(EngineError::Other("file search cancelled".into()));
    }
    let mut entries = collected
        .into_inner()
        .map_err(|_| EngineError::Other("file index poisoned".into()))?;
    entries.truncate(FILE_INDEX_MAX_ENTRIES);
    Ok(entries)
}

/// Rank the (possibly cached) index against a query. `featured_paths` are
/// canonicalized relative to `root` and pinned to the top (by feature rank).
/// The ranking heap is bounded to [`FILE_SEARCH_MAX_RESULTS`], so an empty
/// query never sorts the whole tree.
fn rank_file_matches(
    entries: &[IndexedPath],
    root: &Path,
    query: &str,
    featured_paths: &[String],
) -> Vec<FileSearchMatch> {
    let featured: HashMap<String, usize> = featured_paths
        .iter()
        .filter_map(|path| {
            let path = Path::new(path);
            let full = if path.is_absolute() {
                path.to_path_buf()
            } else {
                root.join(path)
            };
            let canonical = std::fs::canonicalize(full).ok()?;
            let relative = canonical.strip_prefix(root).ok()?;
            Some(relative.to_string_lossy().replace('\\', "/"))
        })
        .enumerate()
        .fold(HashMap::new(), |mut paths, (rank, path)| {
            paths.entry(path).or_insert(rank);
            paths
        });
    let mut matcher = nucleo_matcher::Matcher::new({
        let mut config = nucleo_matcher::Config::DEFAULT;
        config.set_match_paths();
        config
    });
    let pattern = nucleo_matcher::pattern::Pattern::parse(
        query,
        nucleo_matcher::pattern::CaseMatching::Smart,
        nucleo_matcher::pattern::Normalization::Smart,
    );
    let empty_query = query.trim().is_empty();
    let mut matches: std::collections::BinaryHeap<RankedCandidate> =
        std::collections::BinaryHeap::with_capacity(RANK_BUFFER);
    for entry in entries {
        let Some(score) = pattern.score(entry.haystack.slice(..), &mut matcher) else {
            continue;
        };
        matches.push(RankedCandidate {
            empty_query,
            ranked: (
                featured.get(&entry.path).copied(),
                score,
                entry.path.clone(),
                entry.is_dir,
            ),
        });
        if matches.len() > FILE_SEARCH_MAX_RESULTS {
            matches.pop();
        }
    }
    matches
        .into_sorted_vec()
        .into_iter()
        .map(|candidate| {
            let (_, _, path, is_dir) = candidate.ranked;
            FileSearchMatch { path, is_dir }
        })
        .collect()
}
