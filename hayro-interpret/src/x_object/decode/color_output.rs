//! Bounded output conversion after native source sample recovery.
use super::DecodeContext;
use crate::cache::MemoryReservation;
use crate::color::{ColorSpace, ColorSpaceKind, ToRgb};
use crate::x_object::image::ImageXObject;
use crate::{CmykData, ImageData, LumaData, RgbData};

pub(super) const CONVERSION_BATCH_PIXELS: usize = 4096;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum ColorTarget {
    Rgb,
    Cmyk,
}

pub(super) struct ColorOutput<'a> {
    space: &'a ColorSpace,
    data: Vec<u8>,
    batch: Vec<u8>,
    native_batch: Vec<f64>,
    icc_space: Option<&'a ColorSpace>,
    _native_batch_reservation: Option<MemoryReservation>,
    batch_length: usize,
    remaining: usize,
    native: bool,
    gray: bool,
    target: ColorTarget,
    byte_tints: Vec<Option<[u8; 4]>>,
}

impl<'a> ColorOutput<'a> {
    pub(super) fn with_target(
        space: &'a ColorSpace,
        count: usize,
        target: ColorTarget,
    ) -> Option<Self> {
        let components = usize::from(space.num_components());
        if count == 0 || components == 0 {
            return None;
        }
        if target == ColorTarget::Cmyk
            && space.device_alternate_kind() != Some(ColorSpaceKind::DeviceCmyk)
        {
            return None;
        }
        let gray = space.kind() == ColorSpaceKind::DeviceGray
            || space.device_alternate_kind() == Some(ColorSpaceKind::DeviceGray);
        let native = space.is_all_colorants()
            || space.uses_tint_transform()
            || matches!(
                space.kind(),
                ColorSpaceKind::DeviceGray
                    | ColorSpaceKind::DeviceRgb
                    | ColorSpaceKind::CalGray
                    | ColorSpaceKind::CalRgb
                    | ColorSpaceKind::Lab
            );
        let mut data = Vec::new();
        let channels = if target == ColorTarget::Cmyk {
            4
        } else if gray {
            1
        } else {
            3
        };
        data.try_reserve_exact(count.checked_mul(channels)?).ok()?;
        let icc_space = space.image_icc_space();
        let batch_components =
            icc_space.map_or(components, |space| usize::from(space.num_components()));
        let batch_length = count
            .min(CONVERSION_BATCH_PIXELS)
            .checked_mul(batch_components)?;
        let mut batch = Vec::new();
        let mut native_batch = Vec::new();
        let native_batch_reservation = if let Some(icc_space) = icc_space {
            let reservation = icc_space.reserve_icc_image_samples(batch_length)?;
            native_batch.try_reserve_exact(batch_length).ok()?;
            reservation
        } else {
            None
        };
        if !native && icc_space.is_none() {
            batch.try_reserve_exact(batch_length).ok()?;
        }
        Some(Self {
            space,
            data,
            batch,
            native_batch,
            icc_space,
            _native_batch_reservation: native_batch_reservation,
            batch_length,
            remaining: count,
            native,
            gray,
            target,
            byte_tints: Vec::new(),
        })
    }

    /// The caller proves exact normalized byte tints or integral palette indices.
    /// Native Decode, Matte and embedded-alpha recovery must bypass this table.
    pub(super) fn cache_byte_tints(&mut self) -> Option<()> {
        if self.target != ColorTarget::Cmyk || self.space.num_components() != 1 {
            return None;
        }
        self.byte_tints.try_reserve_exact(256).ok()?;
        self.byte_tints.resize(256, None);
        Some(())
    }

    pub(super) fn push(&mut self, values: &[f64]) -> Option<()> {
        if self.remaining == 0
            || values.len() != usize::from(self.space.num_components())
            || values.iter().any(|value| !value.is_finite())
        {
            return None;
        }
        self.remaining -= 1;
        if self.target == ColorTarget::Cmyk {
            let index = (!self.byte_tints.is_empty()).then(|| {
                let value = if self.space.is_indexed() {
                    values[0]
                } else {
                    values[0] * 255.0
                };
                value.round().clamp(0.0, 255.0) as usize
            });
            let pixel = if let Some(pixel) = index.and_then(|i| self.byte_tints[i]) {
                pixel
            } else {
                let values = self.space.image_device_alternate_components(values)?;
                let values: [f32; 4] = values.as_slice().try_into().ok()?;
                let pixel = values.map(|value| (f64::from(value) * 255.0).round() as u8);
                if let Some(index) = index {
                    self.byte_tints[index] = Some(pixel);
                }
                pixel
            };
            self.data.extend(pixel);
        } else if let Some(icc_space) = self.icc_space {
            self.native_batch
                .extend(self.space.image_icc_components(values)?);
            if self.native_batch.len() == self.batch_length || self.remaining == 0 {
                let start = self.data.len();
                let length = (self.native_batch.len() / usize::from(icc_space.num_components()))
                    .checked_mul(3)?;
                self.data.resize(start.checked_add(length)?, 0);
                icc_space.convert_icc_image_samples(&self.native_batch, &mut self.data[start..])?;
                self.native_batch.clear();
            }
        } else if self.native {
            let rgb = self.space.image_rgb_f64(values)?;
            self.data.extend(
                rgb.into_iter()
                    .take(if self.gray { 1 } else { 3 })
                    .map(|value| (value * 255.0).round() as u8),
            );
        } else {
            // Other byte-based spaces retain their existing conversion policy.
            self.batch.extend(
                values
                    .iter()
                    .zip(self.space.component_ranges())
                    .map(|(&value, (low, high))| encode_component(value, low, high)),
            );
            if self.batch.len() == self.batch_length || self.remaining == 0 {
                let start = self.data.len();
                let length = (self.batch.len() / values.len()).checked_mul(3)?;
                self.data.resize(start.checked_add(length)?, 0);
                self.space.convert(&self.batch, &mut self.data[start..])?;
                self.batch.clear();
            }
        }
        Some(())
    }

    pub(super) fn finish(
        mut self,
        obj: &ImageXObject<'_>,
        context: &DecodeContext<'_>,
    ) -> Option<ImageData> {
        if self.remaining != 0 || self.target != ColorTarget::Rgb {
            return None;
        }
        if self.gray
            && obj.transfer_function.as_ref().is_some_and(|transfer| {
                !matches!(
                    transfer,
                    crate::interpret::state::ActiveTransferFunction::Single(_)
                )
            })
        {
            let mut rgb = Vec::new();
            rgb.try_reserve_exact(self.data.len().checked_mul(3)?)
                .ok()?;
            for value in self.data {
                rgb.extend([value; 3]);
            }
            self.data = rgb;
            self.gray = false;
        }
        if let Some(transfer) = &obj.transfer_function {
            transfer.apply_to(&mut self.data);
        }
        Some(if self.gray {
            ImageData::Luma(LumaData {
                data: self.data,
                width: context.width,
                height: context.height,
                interpolate: obj.interpolate,
                scale_factors: context.scale_factors,
            })
        } else {
            ImageData::Rgb(RgbData {
                data: self.data,
                width: context.width,
                height: context.height,
                interpolate: obj.interpolate,
                scale_factors: context.scale_factors,
            })
        })
    }

    pub(super) fn finish_cmyk(
        self,
        obj: &ImageXObject<'_>,
        context: &DecodeContext<'_>,
    ) -> Option<CmykData> {
        if self.remaining != 0
            || self.target != ColorTarget::Cmyk
            || obj.transfer_function.is_some()
        {
            return None;
        }
        Some(CmykData {
            data: self.data,
            width: context.width,
            height: context.height,
            interpolate: obj.interpolate,
            scale_factors: context.scale_factors,
        })
    }
}

pub(super) fn encode_component(value: f64, low: f32, high: f32) -> u8 {
    if low == high {
        0
    } else {
        (((value - f64::from(low)) / (f64::from(high) - f64::from(low))) * 255.0).round() as u8
    }
}
