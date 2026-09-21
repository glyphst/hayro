//! Conservative bounds for the pinned moxcms 0.8.1 byte/native-to-sRGB paths.
//! Keep these with its feature/pin compatibility review: they are requested
//! allocation estimates, not process RSS or general ICC executor introspection.
use super::{ICCColorRepr, RenderingIntent, source_lut};
use moxcms::{
    ColorProfile, DataColorSpace, LutStore, LutWarehouse, PointeeSizeExpressible, ProfileText,
    ToneReprCurve,
};

fn capacity<T>(values: &Vec<T>) -> u64 {
    (values.capacity() as u64).saturating_mul(size_of::<T>() as u64)
}

fn curve_heap(curve: &ToneReprCurve) -> u64 {
    match curve {
        ToneReprCurve::Lut(values) => capacity(values),
        ToneReprCurve::Parametric(values) => capacity(values),
    }
}

fn store_heap(store: &LutStore) -> u64 {
    match store {
        LutStore::Store8(values) => capacity(values),
        LutStore::Store16(values) => capacity(values),
    }
}

fn float_store(store: &LutStore) -> u64 {
    let length = match store {
        LutStore::Store8(values) => values.len(),
        LutStore::Store16(values) => values.len(),
    };
    (length as u64).saturating_mul(size_of::<f32>() as u64)
}

fn lut_heap(lut: &LutWarehouse) -> u64 {
    match lut {
        LutWarehouse::Lut(lut) => [&lut.input_table, &lut.clut_table, &lut.output_table]
            .into_iter()
            .map(store_heap)
            .fold(0, u64::saturating_add),
        LutWarehouse::Multidimensional(lut) => {
            let mut bytes = lut.clut.as_ref().map_or(0, store_heap);
            for curves in [&lut.a_curves, &lut.b_curves, &lut.m_curves] {
                bytes = bytes.saturating_add(capacity(curves));
                for curve in curves {
                    bytes = bytes.saturating_add(curve_heap(curve));
                }
            }
            bytes
        }
    }
}

fn text_heap(text: &ProfileText) -> u64 {
    match text {
        ProfileText::PlainString(value) => value.capacity() as u64,
        ProfileText::Description(value) => [
            &value.ascii_string,
            &value.unicode_string,
            &value.mac_string,
        ]
        .into_iter()
        .map(|s| s.capacity() as u64)
        .fold(0, u64::saturating_add),
        ProfileText::Localizable(values) => values.iter().fold(capacity(values), |bytes, value| {
            [&value.language, &value.country, &value.value]
                .into_iter()
                .fold(bytes, |bytes, text| {
                    bytes.saturating_add(text.capacity() as u64)
                })
        }),
    }
}

pub(super) fn source_bytes(profile: &ColorProfile) -> u64 {
    // Include Arc/cache bookkeeping and the byte/native intent wrappers, in
    // addition to the parsed profile's exact owned capacities below.
    let mut bytes = (size_of::<ICCColorRepr>() + 2 * size_of::<usize>() + 4096) as u64;
    for curve in [
        &profile.red_trc,
        &profile.green_trc,
        &profile.blue_trc,
        &profile.gray_trc,
    ]
    .into_iter()
    .flatten()
    {
        bytes = bytes.saturating_add(curve_heap(curve));
    }
    for lut in [
        &profile.lut_a_to_b_perceptual,
        &profile.lut_a_to_b_colorimetric,
        &profile.lut_a_to_b_saturation,
        &profile.lut_b_to_a_perceptual,
        &profile.lut_b_to_a_colorimetric,
        &profile.lut_b_to_a_saturation,
        &profile.gamut,
    ]
    .into_iter()
    .flatten()
    {
        bytes = bytes.saturating_add(lut_heap(lut));
    }
    for text in [
        &profile.copyright,
        &profile.description,
        &profile.device_manufacturer,
        &profile.device_model,
        &profile.char_target,
        &profile.viewing_conditions_description,
    ]
    .into_iter()
    .flatten()
    {
        bytes = bytes.saturating_add(text_heap(text));
    }
    bytes
}

fn expanded_curve(curve: &ToneReprCurve) -> u64 {
    // moxcms ToneReprCurve::to_clut uses 16384 entries for identity and
    // the f32 input-table capacity for parametric curves. Explicit LUTs retain
    // their length.
    let entries = match curve {
        ToneReprCurve::Lut(values) if !values.is_empty() => values.len(),
        ToneReprCurve::Lut(_) => 16384,
        ToneReprCurve::Parametric(_) => f32::NOT_FINITE_LINEAR_TABLE_SIZE,
    };
    (entries as u64).saturating_mul(size_of::<f32>() as u64)
}

pub(super) fn transform_bytes(
    profile: &ColorProfile,
    components: usize,
    intent: RenderingIntent,
) -> u64 {
    if components == 1 {
        return 1024;
    }
    executor_bytes(profile, intent)
}

fn executor_bytes(profile: &ColorProfile, intent: RenderingIntent) -> u64 {
    // Three 65536-entry u8 output tables, 256-entry input tables, executor
    // boxes, Arc headers, matrices and stage vectors fit within 256 KiB.
    // Raw LUT/curve storage is added separately, even if a stage can elide it.
    let fixed = 256 * 1024_u64;
    match source_lut(profile, intent) {
        None => fixed,
        Some(LutWarehouse::Lut(lut)) => [&lut.input_table, &lut.clut_table, &lut.output_table]
            .into_iter()
            .map(float_store)
            .fold(fixed, u64::saturating_add),
        Some(LutWarehouse::Multidimensional(lut)) => {
            let bytes = fixed.saturating_add(lut.clut.as_ref().map_or(0, float_store));
            lut.a_curves
                .iter()
                .chain(&lut.b_curves)
                .chain(&lut.m_curves)
                .map(expanded_curve)
                .fold(bytes, u64::saturating_add)
        }
    }
}

pub(super) fn native_transform_bytes(profile: &ColorProfile, intent: RenderingIntent) -> u64 {
    // The f64 input path uses up to three 65536-entry f32 linear tables
    // and three 65536-entry f64 destination tables. Original LUT stages still
    // retain f32 curves/CLUTs, accounted by executor_bytes. Destination is
    // the fixed matrix/TRC sRGB profile, never an arbitrary destination LUT.
    executor_bytes(profile, intent).saturating_add(3 * 1024 * 1024)
}

pub(super) fn native_construction_bytes(profile: &ColorProfile, transform: u64) -> u64 {
    source_bytes(profile)
        .saturating_mul(2)
        .saturating_add(transform.saturating_mul(2))
        .saturating_add(1024 * 1024)
}

pub(super) fn construction_bytes(
    profile: &ColorProfile,
    transform: u64,
    intent: RenderingIntent,
) -> u64 {
    // The conversion clones the source plus at most one missing intent tag.
    // Curve normalization can temporarily duplicate retained stage tables;
    // one MiB also covers individual curve/gamma construction work arrays.
    // Gray LUT execution is temporary: only its 256-entry RGB table survives.
    // Charge both the executor and its construction workspace here rather than
    // mistaking the retained table's 1024-byte allowance for either allocation.
    let transient =
        if profile.color_space == DataColorSpace::Gray && source_lut(profile, intent).is_some() {
            executor_bytes(profile, intent).saturating_mul(2)
        } else {
            transform
        };
    source_bytes(profile)
        .saturating_mul(2)
        .saturating_add(transient)
        .saturating_add(1024 * 1024)
}
