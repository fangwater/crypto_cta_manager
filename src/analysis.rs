//! Bounded CPU work and disposable, coalesced query results. No durable data.
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, OnceLock};

use anyhow::{Result, anyhow};

static POOL: OnceLock<rayon::ThreadPool> = OnceLock::new();

pub fn initialize(threads: usize) -> Result<()> {
    if POOL.get().is_none() {
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(threads)
            .thread_name(|index| format!("cta-analysis-{index}"))
            .build()?;
        let _ = POOL.set(pool);
    }
    Ok(())
}

pub fn run<T: Send>(work: impl FnOnce() -> T + Send) -> T {
    POOL.get_or_init(|| {
        rayon::ThreadPoolBuilder::new()
            .num_threads(default_threads())
            .thread_name(|index| format!("cta-analysis-{index}"))
            .build()
            .expect("create analysis CPU pool")
    })
    .install(work)
}

pub fn default_threads() -> usize {
    std::thread::available_parallelism()
        .map(usize::from)
        .unwrap_or(1)
        .min(16)
}

type CachedResult<V> = Arc<OnceLock<std::result::Result<Arc<V>, String>>>;

/// Same-key callers share work. Different keys can run concurrently. Only
/// finished entries are evicted so an in-flight computation is never duplicated.
#[derive(Debug)]
pub struct QueryCache<K, V> {
    entries: Mutex<BTreeMap<K, (u64, CachedResult<V>)>>,
    clock: Mutex<u64>,
    capacity: usize,
}

impl<K: Ord + Clone, V> QueryCache<K, V> {
    pub fn new(capacity: usize) -> Self {
        Self {
            entries: Mutex::new(BTreeMap::new()),
            clock: Mutex::new(0),
            capacity,
        }
    }

    pub fn get_or_compute(&self, key: K, compute: impl FnOnce() -> Result<V>) -> Result<Arc<V>> {
        self.get_or_compute_if(key, compute, |_| true)
    }

    pub fn get(&self, key: &K) -> Option<Arc<V>> {
        let mut entries = self.entries.lock().unwrap();
        let entry = entries.get_mut(key)?;
        let value = entry.1.get()?.as_ref().ok()?.clone();
        let mut clock = self.clock.lock().unwrap();
        *clock += 1;
        entry.0 = *clock;
        Some(value)
    }

    pub fn get_or_compute_if(
        &self,
        key: K,
        compute: impl FnOnce() -> Result<V>,
        retain: impl FnOnce(&V) -> bool,
    ) -> Result<Arc<V>> {
        let cell = {
            let mut entries = self.entries.lock().unwrap();
            let mut clock = self.clock.lock().unwrap();
            *clock += 1;
            if !entries.contains_key(&key) && entries.len() >= self.capacity {
                let oldest = entries
                    .iter()
                    .filter(|(_, (_, cell))| cell.get().is_some())
                    .min_by_key(|(_, (used, _))| *used)
                    .map(|(key, _)| key.clone());
                if let Some(oldest) = oldest {
                    entries.remove(&oldest);
                }
            }
            let entry = entries
                .entry(key.clone())
                .or_insert_with(|| (*clock, Arc::new(OnceLock::new())));
            entry.0 = *clock;
            Arc::clone(&entry.1)
        };
        let result =
            cell.get_or_init(|| compute().map(Arc::new).map_err(|error| error.to_string()));
        match result {
            Ok(value) => {
                let mut entries = self.entries.lock().unwrap();
                if !retain(value) {
                    if entries
                        .get(&key)
                        .is_some_and(|(_, entry)| Arc::ptr_eq(entry, &cell))
                    {
                        entries.remove(&key);
                    }
                }
                while entries.len() > self.capacity {
                    let oldest = entries
                        .iter()
                        .filter(|(_, (_, cell))| cell.get().is_some())
                        .min_by_key(|(_, (used, _))| *used)
                        .map(|(key, _)| key.clone());
                    let Some(oldest) = oldest else { break };
                    entries.remove(&oldest);
                }
                Ok(Arc::clone(value))
            }
            Err(error) => {
                let mut entries = self.entries.lock().unwrap();
                if entries
                    .get(&key)
                    .is_some_and(|(_, entry)| Arc::ptr_eq(entry, &cell))
                {
                    entries.remove(&key);
                }
                Err(anyhow!(error.clone()))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn coalesces_concurrent_queries_and_retries_errors() {
        let cache = QueryCache::new(2);
        let computations = AtomicUsize::new(0);
        std::thread::scope(|scope| {
            for _ in 0..8 {
                scope.spawn(|| {
                    assert_eq!(
                        *cache
                            .get_or_compute(1, || {
                                computations.fetch_add(1, Ordering::Relaxed);
                                std::thread::sleep(std::time::Duration::from_millis(20));
                                Ok(42)
                            })
                            .unwrap(),
                        42
                    );
                });
            }
        });
        assert_eq!(computations.load(Ordering::Relaxed), 1);
        assert!(
            cache
                .get_or_compute(2, || Err(anyhow!("temporary failure")))
                .is_err()
        );
        assert_eq!(*cache.get_or_compute(2, || Ok(7)).unwrap(), 7);
        cache.get_or_compute(3, || Ok(8)).unwrap();
        assert_eq!(cache.entries.lock().unwrap().len(), 2);
    }

    #[test]
    fn incomplete_results_are_not_reused() {
        let cache = QueryCache::new(2);
        assert_eq!(
            *cache
                .get_or_compute_if(1, || Ok(0), |value| *value > 0)
                .unwrap(),
            0
        );
        assert!(cache.entries.lock().unwrap().is_empty());
        assert_eq!(
            *cache
                .get_or_compute_if(1, || Ok(1), |value| *value > 0)
                .unwrap(),
            1
        );
        assert_eq!(
            *cache
                .get_or_compute_if(1, || Ok(2), |value| *value > 0)
                .unwrap(),
            1
        );
    }

    #[test]
    fn concurrent_distinct_queries_shrink_back_to_capacity() {
        let cache = QueryCache::new(2);
        let barrier = std::sync::Barrier::new(8);
        std::thread::scope(|scope| {
            for key in 0..8 {
                let cache = &cache;
                let barrier = &barrier;
                scope.spawn(move || {
                    assert_eq!(
                        *cache
                            .get_or_compute(key, || {
                                barrier.wait();
                                Ok(key)
                            })
                            .unwrap(),
                        key
                    );
                });
            }
        });
        assert_eq!(cache.entries.lock().unwrap().len(), 2);
        cache.get_or_compute(9, || Ok(9)).unwrap();
        assert_eq!(cache.get(&9).as_deref(), Some(&9));
        assert!(cache.get(&10).is_none());
    }
}
