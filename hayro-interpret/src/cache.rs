use crate::util::hash128;
use hayro_syntax::object::{Array, Dict, MaybeRef, Name, Null, ObjRef, Object, Stream};
use kurbo::{Affine, Rect};
use rustc_hash::FxHashMap;
use std::any::Any;
use std::collections::VecDeque;
use std::sync::{Arc, Mutex, OnceLock};

type CacheValue = Option<Box<dyn Any + Send + Sync>>;

struct CacheEntry {
    value: Arc<OnceLock<CacheValue>>,
    last_used: u64,
}

struct CacheState {
    entries: FxHashMap<u128, CacheEntry>,
    recency: VecDeque<(u128, u64)>,
    clock: u64,
    max_entries: usize,
    hits: u64,
    misses: u64,
    evictions: u64,
}

impl CacheState {
    fn touch(&mut self, id: u128) {
        self.clock = self.clock.wrapping_add(1);
        if let Some(entry) = self.entries.get_mut(&id) {
            entry.last_used = self.clock;
            self.recency.push_back((id, self.clock));
        }

        if self.recency.len() > self.entries.len().saturating_mul(4).saturating_add(32) {
            let mut current = self
                .entries
                .iter()
                .map(|(id, entry)| (*id, entry.last_used))
                .collect::<Vec<_>>();
            current.sort_unstable_by_key(|(_, last_used)| *last_used);
            self.recency = current.into();
        }
    }

    fn evict_to_limit(&mut self) {
        while self.entries.len() > self.max_entries {
            let Some((id, last_used)) = self.recency.pop_front() else {
                break;
            };
            let is_current = self
                .entries
                .get(&id)
                .is_some_and(|entry| entry.last_used == last_used);
            if is_current {
                self.entries.remove(&id);
                self.evictions = self.evictions.saturating_add(1);
            }
        }
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct CacheStats {
    pub(crate) entries: usize,
    pub(crate) hits: u64,
    pub(crate) misses: u64,
    pub(crate) evictions: u64,
}

#[derive(Clone)]
pub(crate) struct Cache(Arc<Mutex<CacheState>>);

impl Default for Cache {
    fn default() -> Self {
        Self::new()
    }
}

impl Cache {
    pub(crate) fn new() -> Self {
        Self::with_max_entries(usize::MAX)
    }

    pub(crate) fn with_max_entries(max_entries: usize) -> Self {
        Self(Arc::new(Mutex::new(CacheState {
            entries: FxHashMap::default(),
            recency: VecDeque::new(),
            clock: 0,
            max_entries,
            hits: 0,
            misses: 0,
            evictions: 0,
        })))
    }

    pub(crate) fn get_or_insert_with<T: Clone + Send + Sync + 'static>(
        &self,
        id: u128,
        f: impl FnOnce() -> Option<T>,
    ) -> Option<T> {
        let value = {
            let mut state = self.0.lock().unwrap_or_else(|error| error.into_inner());
            if state.entries.contains_key(&id) {
                state.hits = state.hits.saturating_add(1);
                state.touch(id);
                state
                    .entries
                    .get(&id)
                    .map(|entry| entry.value.clone())
                    .expect("a cache entry checked above must remain present")
            } else {
                state.misses = state.misses.saturating_add(1);
                state.clock = state.clock.wrapping_add(1);
                let last_used = state.clock;
                let value = Arc::new(OnceLock::new());
                state.entries.insert(
                    id,
                    CacheEntry {
                        value: value.clone(),
                        last_used,
                    },
                );
                state.recency.push_back((id, last_used));
                state.evict_to_limit();
                value
            }
        };

        // Initialization happens without holding the map lock. OnceLock makes construction
        // single-flight for a given key while still allowing unrelated keys to initialize in
        // parallel. The local Arc keeps an entry alive if the LRU evicts it during construction.
        value
            .get_or_init(|| f().map(|value| Box::new(value) as Box<dyn Any + Send + Sync>))
            .as_ref()
            .and_then(|value| value.downcast_ref::<T>().cloned())
    }

    pub(crate) fn stats(&self) -> CacheStats {
        let state = self.0.lock().unwrap_or_else(|error| error.into_inner());
        CacheStats {
            entries: state.entries.len(),
            hits: state.hits,
            misses: state.misses,
            evictions: state.evictions,
        }
    }

    pub(crate) fn clear(&self) {
        let mut state = self.0.lock().unwrap_or_else(|error| error.into_inner());
        state.entries.clear();
        state.recency.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::Cache;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::thread;

    #[test]
    fn bounded_cache_evicts_least_recently_used_entries() {
        let cache = Cache::with_max_entries(2);
        assert_eq!(cache.get_or_insert_with(1, || Some(10_u32)), Some(10));
        assert_eq!(cache.get_or_insert_with(2, || Some(20_u32)), Some(20));
        assert_eq!(cache.get_or_insert_with(1, || Some(11_u32)), Some(10));
        assert_eq!(cache.get_or_insert_with(3, || Some(30_u32)), Some(30));
        assert_eq!(cache.get_or_insert_with(2, || Some(21_u32)), Some(21));

        let stats = cache.stats();
        assert_eq!(stats.entries, 2);
        assert_eq!(stats.hits, 1);
        assert_eq!(stats.misses, 4);
        assert_eq!(stats.evictions, 2);
    }

    #[test]
    fn construction_is_single_flight_per_key() {
        let cache = Cache::with_max_entries(2);
        let constructions = Arc::new(AtomicUsize::new(0));
        let threads = (0..8)
            .map(|_| {
                let cache = cache.clone();
                let constructions = constructions.clone();
                thread::spawn(move || {
                    cache.get_or_insert_with(7, || {
                        constructions.fetch_add(1, Ordering::SeqCst);
                        Some(42_u32)
                    })
                })
            })
            .collect::<Vec<_>>();

        for thread in threads {
            assert_eq!(thread.join().unwrap(), Some(42));
        }
        assert_eq!(constructions.load(Ordering::SeqCst), 1);
    }
}

/// A trait for objects that can generate a unique cache key.
pub trait CacheKey {
    /// Returns the cache key for this object.
    fn cache_key(&self) -> u128;
}

impl<T: CacheKey, U: CacheKey> CacheKey for (T, U) {
    fn cache_key(&self) -> u128 {
        hash128(&(self.0.cache_key(), self.1.cache_key()))
    }
}

impl CacheKey for Dict<'_> {
    fn cache_key(&self) -> u128 {
        hash128(self.data())
    }
}

impl CacheKey for Stream<'_> {
    fn cache_key(&self) -> u128 {
        self.dict().cache_key()
    }
}

impl CacheKey for Null {
    fn cache_key(&self) -> u128 {
        hash128(self)
    }
}

impl CacheKey for bool {
    fn cache_key(&self) -> u128 {
        hash128(self)
    }
}

impl CacheKey for hayro_syntax::object::Number {
    fn cache_key(&self) -> u128 {
        hash128(&self.as_f64().to_bits())
    }
}

impl CacheKey for hayro_syntax::object::String<'_> {
    fn cache_key(&self) -> u128 {
        hash128(self.as_ref())
    }
}

impl CacheKey for Name<'_> {
    fn cache_key(&self) -> u128 {
        hash128(self)
    }
}

impl CacheKey for Array<'_> {
    fn cache_key(&self) -> u128 {
        hash128(self.data())
    }
}

impl CacheKey for Object<'_> {
    fn cache_key(&self) -> u128 {
        match self {
            Object::Null(n) => n.cache_key(),
            Object::Boolean(b) => b.cache_key(),
            Object::Number(n) => n.cache_key(),
            Object::String(s) => s.cache_key(),
            Object::Name(n) => n.cache_key(),
            Object::Dict(d) => d.cache_key(),
            Object::Array(a) => a.cache_key(),
            Object::Stream(s) => s.cache_key(),
        }
    }
}

impl CacheKey for ObjRef {
    fn cache_key(&self) -> u128 {
        hash128(self)
    }
}

impl<T: CacheKey> CacheKey for MaybeRef<T> {
    fn cache_key(&self) -> u128 {
        match self {
            Self::Ref(r) => r.cache_key(),
            Self::NotRef(o) => o.cache_key(),
        }
    }
}

impl CacheKey for Affine {
    fn cache_key(&self) -> u128 {
        let c = self.as_coeffs();
        hash128(&[
            c[0].to_bits(),
            c[1].to_bits(),
            c[2].to_bits(),
            c[3].to_bits(),
            c[4].to_bits(),
            c[5].to_bits(),
        ])
    }
}

impl CacheKey for Rect {
    fn cache_key(&self) -> u128 {
        hash128(&[
            self.x0.to_bits(),
            self.y0.to_bits(),
            self.x1.to_bits(),
            self.y1.to_bits(),
        ])
    }
}

impl CacheKey for u128 {
    fn cache_key(&self) -> u128 {
        hash128(self)
    }
}
