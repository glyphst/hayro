//! Bounded output conversion after native source sample recovery.
use super::DecodeContext;
use crate::color::{ColorSpace, ColorSpaceKind, ToRgb};
use crate::x_object::image::ImageXObject;
use crate::{ImageData, LumaData, RgbData};

pub(super) const CONVERSION_BATCH_PIXELS: usize = 4096;

pub(super) struct ColorOutput<'a> {
    space: &'a ColorSpace,
    data: Vec<u8>,
    batch: Vec<u8>,
    batch_length: usize,
    remaining: usize,
    native: bool,
    gray: bool,
}

impl<'a> ColorOutput<'a> {
    pub(super) fn new(space: &'a ColorSpace, count: usize) -> Option<Self> {
        let components = usize::from(space.num_components());
        if count == 0 || components == 0 {
            return None;
        }
        let gray = space.kind() == ColorSpaceKind::DeviceGray;
        let native = matches!(
            space.kind(),
            ColorSpaceKind::DeviceGray
                | ColorSpaceKind::DeviceRgb
                | ColorSpaceKind::CalGray
                | ColorSpaceKind::CalRgb
                | ColorSpaceKind::Lab
        );
        let mut data = Vec::new();
        data.try_reserve_exact(count.checked_mul(if gray { 1 } else { 3 })?)
            .ok()?;
        let batch_length = count.min(CONVERSION_BATCH_PIXELS).checked_mul(components)?;
        let mut batch = Vec::new();
        if !native {
            batch.try_reserve_exact(batch_length).ok()?;
        }
        Some(Self {
            space,
            data,
            batch,
            batch_length,
            remaining: count,
            native,
            gray,
        })
    }

    pub(super) fn push(&mut self, values: &[f64]) -> Option<()> {
        if self.remaining == 0
            || values.len() != usize::from(self.space.num_components())
            || values.iter().any(|value| !value.is_finite())
        {
            return None;
        }
        self.remaining -= 1;
        if self.native {
            let rgb = self.space.image_rgb_f64(values)?;
            self.data.extend(
                rgb.into_iter()
                    .take(if self.gray { 1 } else { 3 })
                    .map(|value| (value * 255.0).round() as u8),
            );
        } else {
            // Preserve the existing byte-input ICC/tint policy, after recovery.
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
        if self.remaining != 0 {
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
}

pub(super) fn encode_component(value: f64, low: f32, high: f32) -> u8 {
    if low == high {
        0
    } else {
        (((value - f64::from(low)) / (f64::from(high) - f64::from(low))) * 255.0).round() as u8
    }
}
