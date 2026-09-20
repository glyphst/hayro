use super::CacheState;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, Weak};

pub(super) struct MemoryBudget {
    limit: u64,
    bytes: AtomicU64,
    refusals: AtomicU64,
}

impl MemoryBudget {
    pub(super) fn new(limit: u64) -> Self {
        Self {
            limit,
            bytes: AtomicU64::new(0),
            refusals: AtomicU64::new(0),
        }
    }

    pub(super) fn bytes(&self) -> u64 {
        self.bytes.load(Ordering::Relaxed)
    }
    pub(super) fn refusals(&self) -> u64 {
        self.refusals.load(Ordering::Relaxed)
    }

    pub(super) fn record_refusal(&self) {
        self.refusals.fetch_add(1, Ordering::Relaxed);
    }

    fn reserve(self: &Arc<Self>, bytes: u64) -> Option<MemoryReservation> {
        self.bytes
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |used| {
                used.checked_add(bytes).filter(|sum| *sum <= self.limit)
            })
            .ok()?;
        Some(MemoryReservation {
            budget: self.clone(),
            bytes,
        })
    }
}

/// Charges follow the final owner, including an active page after cache eviction.
/// The weak map reference prevents cached profiles from owning their own cache.
#[derive(Clone)]
pub(crate) struct IccMemory {
    cache: Weak<Mutex<CacheState>>,
    budget: Arc<MemoryBudget>,
}

pub(crate) struct MemoryReservation {
    budget: Arc<MemoryBudget>,
    bytes: u64,
}

impl Drop for MemoryReservation {
    fn drop(&mut self) {
        self.budget.bytes.fetch_sub(self.bytes, Ordering::AcqRel);
    }
}

impl IccMemory {
    pub(super) fn new(cache: Weak<Mutex<CacheState>>, budget: Arc<MemoryBudget>) -> Self {
        Self { cache, budget }
    }

    pub(crate) fn reserve(&self, bytes: u64) -> Option<MemoryReservation> {
        if bytes <= self.budget.limit {
            loop {
                if let Some(reservation) = self.budget.reserve(bytes) {
                    return Some(reservation);
                }
                let Some(cache) = self.cache.upgrade() else {
                    break;
                };
                let removed = cache
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .evict_oldest();
                // Dropping an entry may release a profile's final owner. Do it
                // after releasing the map lock, before checking the budget again.
                if removed.is_none() {
                    break;
                }
                drop(removed);
            }
        }
        self.budget.record_refusal();
        None
    }
}

#[cfg(test)]
mod tests {
    use crate::cache::Cache;
    use std::sync::{Arc, Barrier};

    #[test]
    fn evicted_live_owners_keep_their_charge_and_release_it_on_drop() {
        let cache = Cache::with_limits(2, 100);
        let memory = cache.icc_memory();
        let held = cache
            .get_or_insert_with(1, || memory.reserve(70).map(Arc::new))
            .unwrap();
        assert!(memory.reserve(40).is_none());
        assert_eq!(cache.stats().entries, 0);
        assert_eq!(cache.stats().icc_bytes, 70);
        assert_eq!(cache.stats().icc_memory_refusals, 1);
        cache.clear();
        assert_eq!(cache.stats().icc_bytes, 70);
        drop(held);
        assert_eq!(cache.stats().icc_bytes, 0);
        let full = memory.reserve(100).unwrap();
        assert_eq!(cache.stats().icc_bytes, 100);
        drop(full);
        assert_eq!(cache.stats().icc_bytes, 0);
    }

    #[test]
    fn byte_pressure_reclaims_the_least_recently_used_cached_owner() {
        let cache = Cache::with_limits(10, 100);
        let memory = cache.icc_memory();
        drop(
            cache
                .get_or_insert_with(1, || memory.reserve(40).map(Arc::new))
                .unwrap(),
        );
        drop(
            cache
                .get_or_insert_with(2, || memory.reserve(40).map(Arc::new))
                .unwrap(),
        );
        drop(
            cache
                .get_or_insert_with::<Arc<super::MemoryReservation>>(1, || panic!("cache hit"))
                .unwrap(),
        );
        let additional = memory.reserve(30).unwrap();
        assert_eq!(cache.stats().icc_bytes, 70);
        assert_eq!(cache.stats().entries, 1);
        assert_eq!(cache.stats().evictions, 1);
        drop(
            cache
                .get_or_insert_with::<Arc<super::MemoryReservation>>(1, || {
                    panic!("the touched owner must survive")
                })
                .unwrap(),
        );
        drop(additional);
        cache.clear();
        assert_eq!(cache.stats().icc_bytes, 0);
    }

    #[test]
    fn cached_reservations_do_not_keep_the_cache_alive() {
        let cache = Cache::with_limits(2, 100);
        let weak = Arc::downgrade(&cache.0);
        let memory = cache.icc_memory();
        drop(
            cache
                .get_or_insert_with(1, || memory.reserve(70).map(Arc::new))
                .unwrap(),
        );
        drop(cache);
        assert!(weak.upgrade().is_none());
        assert_eq!(memory.budget.bytes(), 0);
    }

    #[test]
    fn refused_construction_is_not_retained_even_when_it_exceeds_the_limit() {
        let cache = Cache::with_limits(2, 100);
        let memory = cache.icc_memory();
        for attempt in 1..=2 {
            assert!(
                cache
                    .get_or_insert_with(1, || memory.reserve(101).map(Arc::new))
                    .is_none()
            );
            assert_eq!(cache.stats().entries, 0);
            assert!(cache.stats().icc_memory_refusals >= attempt);
        }
    }

    #[test]
    fn concurrent_reservations_never_exceed_the_shared_limit() {
        let cache = Cache::with_limits(0, 256);
        let barrier = Arc::new(Barrier::new(9));
        let threads: Vec<_> = (0..8)
            .map(|_| {
                let memory = cache.icc_memory();
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    let held = memory.reserve(64);
                    let accepted = held.is_some();
                    barrier.wait();
                    barrier.wait();
                    drop(held);
                    accepted
                })
            })
            .collect();
        barrier.wait();
        assert_eq!(cache.stats().icc_bytes, 256);
        barrier.wait();
        assert_eq!(
            threads
                .into_iter()
                .map(|thread| thread.join().unwrap())
                .filter(|accepted| *accepted)
                .count(),
            4
        );
        assert_eq!(cache.stats().icc_bytes, 0);
    }

    #[test]
    fn reservation_addition_cannot_wrap_the_limit() {
        let cache = Cache::with_limits(0, u64::MAX);
        let memory = cache.icc_memory();
        let full = memory.reserve(u64::MAX).unwrap();
        assert!(memory.reserve(1).is_none());
        assert_eq!(cache.stats().icc_bytes, u64::MAX);
        drop(full);
        assert_eq!(cache.stats().icc_bytes, 0);
    }
}
