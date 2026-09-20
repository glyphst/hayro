use super::*;
use crate::cache::Cache;
use crate::color::{ColorSpace, ColorSpaceType, Pattern};

#[test]
fn profile_and_all_intent_owners_remain_charged_after_cache_clear() {
    let bytes = lut_tests::profile_bytes(4, 4, true);
    let cache = Cache::with_limits(16, 64 * 1024 * 1024);
    let profile = cache
        .get_or_insert_with(7, || {
            ICCProfile::new_with_memory(&bytes, 4, Some(cache.icc_memory()))
        })
        .unwrap();
    let source_bytes = cache.stats().icc_bytes;
    assert!(source_bytes > bytes.len() as u64);
    let mut owners = Vec::new();
    for intent in [
        RenderingIntent::Perceptual,
        RenderingIntent::RelativeColorimetric,
        RenderingIntent::Saturation,
        RenderingIntent::AbsoluteColorimetric,
    ] {
        let before = cache.stats().icc_bytes;
        let bound = profile.with_intent(intent).unwrap();
        assert!(cache.stats().icc_bytes > before);
        let retained = cache.stats().icc_bytes;
        let again = profile.with_intent(intent).unwrap();
        assert!(std::ptr::eq(
            bound.transform().unwrap(),
            again.transform().unwrap()
        ));
        let mut output = [0; 3];
        assert!(bound.convert(&[17, 31, 63, 91], &mut output).is_some());
        assert_eq!(cache.stats().icc_bytes, retained);
        owners.push(bound);
    }
    let retained = cache.stats().icc_bytes;
    cache.clear();
    assert_eq!(cache.stats().icc_bytes, retained);
    drop(profile);
    assert_eq!(cache.stats().icc_bytes, retained);
    drop(owners);
    assert_eq!(cache.stats().icc_bytes, 0);
}

#[test]
fn transient_pressure_does_not_poison_transform_or_wrapped_intent_caches() {
    let bytes = lut_tests::profile_bytes(4, 3, true);
    let limit = 8 * 1024 * 1024;
    let cache = Cache::with_limits(16, limit);
    let profile = ICCProfile::new_with_memory(&bytes, 3, Some(cache.icc_memory())).unwrap();
    let source_bytes = cache.stats().icc_bytes;
    let space = ColorSpace::from_kind(ColorSpaceType::Pattern(Pattern::new(
        ColorSpace::from_kind(ColorSpaceType::ICCBased(profile.clone())),
    )));
    let held = cache
        .icc_memory()
        .reserve(limit - source_bytes - 128 * 1024)
        .unwrap();
    let intent = RenderingIntent::RelativeColorimetric;
    assert!(matches!(
        space.with_rendering_intent(intent),
        Err(ColorConversionError::IccMemoryLimit)
    ));
    assert!(profile.data.transforms[intent as usize].get().is_none());
    drop(held);
    let bound = space.with_rendering_intent(intent).unwrap();
    let retained = cache.stats().icc_bytes;
    let again = space.with_rendering_intent(intent).unwrap();
    assert!(bound.same_instance(&again));
    assert_eq!(cache.stats().icc_bytes, retained);
    drop(bound);
    drop(again);
    drop(space);
    drop(profile);
    assert_eq!(cache.stats().icc_bytes, 0);
}

#[test]
fn failed_construction_releases_reservations_and_keeps_semantic_refusals() {
    let mut source = ColorProfile::new_srgb();
    source.red_trc = None;
    source.green_trc = None;
    source.blue_trc = None;
    let bytes = source.encode().unwrap();
    let cache = Cache::with_limits(4, 8 * 1024 * 1024);
    let profile = ICCProfile::new_with_memory(&bytes, 3, Some(cache.icc_memory())).unwrap();
    let before = cache.stats().icc_bytes;
    assert!(matches!(
        profile.transform(),
        Err(ColorConversionError::IccTransform)
    ));
    assert!(
        profile.data.transforms[profile.intent as usize]
            .get()
            .unwrap()
            .is_none()
    );
    assert_eq!(cache.stats().icc_bytes, before);
    assert!(matches!(
        profile.transform(),
        Err(ColorConversionError::IccTransform)
    ));
    assert_eq!(cache.stats().icc_memory_refusals, 0);
    drop(profile);
    assert_eq!(cache.stats().icc_bytes, 0);
}

#[test]
fn cancelled_and_memory_limited_equivalence_proofs_can_retry() {
    let bytes = ColorProfile::new_srgb().encode().unwrap();
    let limit = 8 * 1024 * 1024;
    let cache = Cache::with_limits(4, limit);
    let profile = ICCProfile::new_with_memory(&bytes, 3, Some(cache.icc_memory())).unwrap();
    let before = cache.stats().icc_bytes;
    assert_eq!(
        profile.device_rgb_bounds(|| true),
        Err(IccEquivalenceError::Cancelled)
    );
    assert_eq!(cache.stats().icc_bytes, before);
    let held = cache
        .icc_memory()
        .reserve(limit - before - 128 * 1024)
        .unwrap();
    assert_eq!(
        profile.device_rgb_bounds(|| false),
        Err(IccEquivalenceError::MemoryLimit)
    );
    assert!(
        profile.data.equivalence[profile.intent as usize]
            .get()
            .is_none()
    );
    drop(held);
    assert!(profile.device_rgb_bounds(|| false).is_ok());
    drop(profile);
    assert_eq!(cache.stats().icc_bytes, 0);
}

#[test]
fn parallel_intent_users_share_one_retained_transform_reservation() {
    let bytes = lut_tests::profile_bytes(4, 3, true);
    let cache = Cache::with_limits(4, 8 * 1024 * 1024);
    let profile = ICCProfile::new_with_memory(&bytes, 3, Some(cache.icc_memory())).unwrap();
    let barrier = Arc::new(std::sync::Barrier::new(8));
    let threads: Vec<_> = (0..8)
        .map(|_| {
            let profile = profile.clone();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                barrier.wait();
                profile
                    .with_intent(RenderingIntent::RelativeColorimetric)
                    .unwrap()
            })
        })
        .collect();
    let owners: Vec<_> = threads
        .into_iter()
        .map(|thread| thread.join().unwrap())
        .collect();
    for owner in &owners {
        assert!(std::ptr::eq(
            owner.transform().unwrap(),
            owners[0].transform().unwrap()
        ));
    }
    let retained = cache.stats().icc_bytes;
    drop(owners);
    assert_eq!(cache.stats().icc_bytes, retained);
    drop(profile);
    assert_eq!(cache.stats().icc_bytes, 0);
    assert_eq!(cache.stats().icc_memory_refusals, 0);
}
