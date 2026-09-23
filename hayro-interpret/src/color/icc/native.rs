//! Bounded real-component conversion for decoded ICC image samples.
use super::{ICCProfile, memory, source_lut};
use crate::cache::MemoryReservation;
use crate::color::{ColorConversionError, ColorSpace, ColorSpaceType};
use moxcms::{ColorProfile, DataColorSpace, Layout, LutType, LutWarehouse, TransformF64Executor};
use smallvec::SmallVec;
use std::sync::Arc;

const BATCH_PIXELS: usize = 1024;
const CONVERSION_SCRATCH_BYTES: u64 = 128 * 1024;

#[cfg(test)]
#[path = "native_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "native_lut_tests.rs"]
mod lut_tests;

pub(super) struct OwnedNativeTransform {
    pub(super) executor: Arc<TransformF64Executor>,
    replicate_gray: bool,
    _reservation: Option<MemoryReservation>,
}

impl ICCProfile {
    pub(super) fn native_transform(&self) -> Result<&OwnedNativeTransform, ColorConversionError> {
        self.native_transform_for(false)
    }

    fn native_transform_for(
        &self,
        unquantized: bool,
    ) -> Result<&OwnedNativeTransform, ColorConversionError> {
        let slot = if unquantized {
            &self.data.float_transforms[self.intent as usize]
        } else {
            &self.data.native_transforms[self.intent as usize]
        };
        if slot.get().is_none() {
            let _guard = self
                .data
                .transform_build
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            if slot.get().is_none() {
                let mut bytes = memory::native_transform_bytes(&self.data.src_profile, self.intent);
                if unquantized {
                    // The opt-in converters own original curves or integer
                    // LUT tables, independent of sampled executor capacity.
                    bytes = bytes.saturating_add(memory::source_bytes(&self.data.src_profile));
                }
                let reservation = self.reserve_memory(bytes)?;
                let _construction = self.reserve_memory(memory::native_construction_bytes(
                    &self.data.src_profile,
                    bytes,
                ))?;
                let value =
                    self.build_native_transform(unquantized)
                        .map(|(executor, replicate_gray)| OwnedNativeTransform {
                            executor,
                            replicate_gray,
                            _reservation: reservation,
                        });
                // A quota refusal returns before setting the slot. Keep the
                // same shared ownership and retry contract as byte transforms.
                let _ = slot.set(value);
            }
        }
        slot.get()
            .and_then(Option::as_ref)
            .ok_or(ColorConversionError::IccTransform)
    }

    fn build_native_transform(
        &self,
        unquantized: bool,
    ) -> Option<(Arc<TransformF64Executor>, bool)> {
        if unquantized && !self.has_float_image_conversion() {
            return None;
        }
        let (mut source, destination, options) = self.conversion_profiles()?;
        let mut replicate_gray = false;
        let layout = match self.number_components() {
            1 if source_lut(&source, self.intent).is_none() => {
                if source.pcs == DataColorSpace::Xyz {
                    let curve = source.gray_trc.take()?;
                    source = ColorProfile::new_srgb();
                    source.red_trc = Some(curve.clone());
                    source.green_trc = Some(curve.clone());
                    source.blue_trc = Some(curve);
                    source.cicp = None;
                    replicate_gray = true;
                    Layout::Rgb
                } else if source.pcs == DataColorSpace::Lab {
                    source.lut_a_to_b_perceptual = None;
                    source.lut_a_to_b_colorimetric = None;
                    source.lut_a_to_b_saturation = None;
                    Layout::Gray
                } else {
                    return None;
                }
            }
            1 => Layout::Gray,
            3 => Layout::Rgb,
            // As in the byte path, the source profile identifies four-channel
            // CMYK; Layout::Rgba supplies its component stride, without alpha.
            4 => Layout::Rgba,
            _ => return None,
        };
        let executor = if unquantized {
            if source_lut(&source, self.intent).is_some() {
                source.create_transform_f64_lut_to_linear_rgb(
                    layout,
                    &destination,
                    Layout::Rgb,
                    options,
                )
            } else {
                source.create_transform_f64_to_linear_rgb(layout, &destination, Layout::Rgb)
            }
        } else {
            source.create_transform_f64(layout, &destination, Layout::Rgb, options)
        }
        .ok()?;
        Some((executor, replicate_gray))
    }

    fn convert_native(&self, input: &[f64], output: &mut [u8]) -> Option<()> {
        self.convert_native_into(input, output, false, encode_component)
    }

    fn convert_native_f64(&self, input: &[f64], output: &mut [f64]) -> Option<()> {
        self.convert_native_into(input, output, true, |value| {
            // The fixed output device is sRGB. Apply its transfer curve to
            // the original linear result, without a sampled output table.
            value.is_finite().then(|| {
                let value = value.clamp(0.0, 1.0);
                if value <= 0.0031308 {
                    12.92 * value
                } else {
                    1.055 * value.powf(1.0 / 2.4) - 0.055
                }
            })
        })
    }

    fn convert_native_into<T>(
        &self,
        input: &[f64],
        output: &mut [T],
        unquantized: bool,
        encode: impl Fn(f64) -> Option<T>,
    ) -> Option<()> {
        let components = self.number_components();
        if !input.len().is_multiple_of(components)
            || output.len() != (input.len() / components).checked_mul(3)?
            || input.iter().any(|value| !value.is_finite())
        {
            return None;
        }
        let transform = if unquantized {
            self.native_transform_for(true)
        } else {
            self.native_transform()
        }
        .ok()?;
        // Charge each concurrent conversion's bounded buffers and the CMS
        // intermediate stage vectors independently of its retained executor.
        let _scratch = self.reserve_memory(CONVERSION_SCRATCH_BYTES).ok()?;
        let source_components = if transform.replicate_gray {
            3
        } else {
            components
        };
        let count = (input.len() / components).min(BATCH_PIXELS);
        let mut source = Vec::new();
        source
            .try_reserve_exact(count.checked_mul(source_components)?)
            .ok()?;
        let mut converted = Vec::new();
        converted.try_reserve_exact(count.checked_mul(3)?).ok()?;
        for (samples, pixels) in input
            .chunks(BATCH_PIXELS * components)
            .zip(output.chunks_mut(BATCH_PIXELS * 3))
        {
            source.clear();
            for &sample in samples {
                source.extend(std::iter::repeat_n(
                    sample.clamp(0.0, 1.0),
                    if transform.replicate_gray { 3 } else { 1 },
                ));
            }
            converted.resize(pixels.len(), 0.0);
            transform.executor.transform(&source, &mut converted).ok()?;
            for (destination, &value) in pixels.iter_mut().zip(&converted) {
                *destination = encode(value)?;
            }
        }
        Some(())
    }

    fn has_float_image_conversion(&self) -> bool {
        (self.data.matrix_tags_only
            && matches!(self.number_components(), 1 | 3)
            && source_lut(&self.data.src_profile, self.intent).is_none())
            || (self.data.original_lut_tags_only
                && self.number_components() == 1
                && self.data.src_profile.color_space == DataColorSpace::Gray
                && self.data.src_profile.pcs == DataColorSpace::Xyz
                && matches!(
                    source_lut(&self.data.src_profile, self.intent),
                    Some(LutWarehouse::Lut(lut)) if lut.lut_type == LutType::Lut16
                        && lut.num_input_channels == 1
                        && lut.num_output_channels == 3
                        && lut.matrix == moxcms::Matrix3d::IDENTITY
                ))
    }
}

pub(super) fn encode_component(value: f64) -> Option<u8> {
    value
        .is_finite()
        .then(|| (value.clamp(0.0, 1.0) * 255.0).round() as u8)
}

impl ColorSpace {
    pub(crate) fn image_float_icc_space(&self) -> Option<&Self> {
        let space = self.image_icc_space()?;
        let ColorSpaceType::ICCBased(profile) = space.0.as_ref() else {
            return None;
        };
        profile.has_float_image_conversion().then_some(space)
    }

    /// Find the selected ICC alternate without evaluating or quantizing samples.
    pub(crate) fn image_icc_space(&self) -> Option<&Self> {
        if self.is_non_marking() || self.is_all_colorants() {
            return None;
        }
        match self.0.as_ref() {
            ColorSpaceType::ICCBased(_) => Some(self),
            ColorSpaceType::Indexed(space) => space.base().image_icc_space(),
            ColorSpaceType::Separation(space) => space.alternate_space().image_icc_space(),
            ColorSpaceType::DeviceN(space) => space.alternate_space().image_icc_space(),
            _ => None,
        }
    }

    pub(crate) fn image_icc_components(&self, input: &[f64]) -> Option<SmallVec<[f64; 4]>> {
        if input.len() != usize::from(self.num_components())
            || input.iter().any(|value| !value.is_finite())
        {
            return None;
        }
        match self.0.as_ref() {
            ColorSpaceType::ICCBased(_) => Some(input.iter().map(|v| v.clamp(0.0, 1.0)).collect()),
            ColorSpaceType::Indexed(space) => {
                let values: SmallVec<[f64; 4]> = space
                    .entry(input[0])
                    .iter()
                    .map(|&value| f64::from(value) / 255.0)
                    .collect();
                space.base().image_icc_components(&values)
            }
            ColorSpaceType::Separation(space) => {
                let values: SmallVec<[f32; 4]> =
                    input.iter().map(|v| v.clamp(0.0, 1.0) as f32).collect();
                let values: SmallVec<[f64; 4]> = space
                    .tint_components(&values)?
                    .into_iter()
                    .map(f64::from)
                    .collect();
                space.alternate_space().image_icc_components(&values)
            }
            ColorSpaceType::DeviceN(space) => {
                let values: SmallVec<[f32; 4]> =
                    input.iter().map(|v| v.clamp(0.0, 1.0) as f32).collect();
                let values: SmallVec<[f64; 4]> = space
                    .tint_components(&values)?
                    .into_iter()
                    .map(f64::from)
                    .collect();
                space.alternate_space().image_icc_components(&values)
            }
            _ => None,
        }
    }

    pub(crate) fn reserve_icc_image_samples(
        &self,
        count: usize,
    ) -> Option<Option<MemoryReservation>> {
        let ColorSpaceType::ICCBased(profile) = self.0.as_ref() else {
            return None;
        };
        profile
            .reserve_memory((count as u64).checked_mul(size_of::<f64>() as u64)?)
            .ok()
    }

    pub(crate) fn convert_icc_image_samples(&self, input: &[f64], output: &mut [u8]) -> Option<()> {
        let ColorSpaceType::ICCBased(profile) = self.0.as_ref() else {
            return None;
        };
        profile.convert_native(input, output)
    }

    pub(crate) fn convert_icc_image_samples_f64(
        &self,
        input: &[f64],
        output: &mut [f64],
    ) -> Option<()> {
        let ColorSpaceType::ICCBased(profile) = self.0.as_ref() else {
            return None;
        };
        profile.convert_native_f64(input, output)
    }
}
