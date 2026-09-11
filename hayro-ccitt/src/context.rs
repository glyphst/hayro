use crate::{Color, DecodeError, DecodeSettings, Result};
use alloc::vec::Vec;

#[derive(Clone, Copy)]
pub(crate) struct ColorChange {
    pub(crate) idx: u32,
    pub(crate) color: Color,
}

pub(crate) trait Output {
    fn row(&mut self, changes: &[ColorChange], width: u32, invert: bool) -> Result<()>;
}

pub(crate) fn runs(changes: &[ColorChange], width: u32, mut visit: impl FnMut(Color, u32)) {
    let mut start = 0;
    let mut color = Color::White;
    for change in changes {
        if change.idx > start {
            visit(color, change.idx - start);
        }
        start = change.idx;
        color = change.color;
    }
    if start < width {
        visit(color, width - start);
    }
}

/// A reusable context for decoding CCITT images.
pub struct DecoderContext {
    pub(crate) settings: DecodeSettings,
    pub(crate) ref_changes: Vec<ColorChange>,
    pub(crate) coding_changes: Vec<ColorChange>,
    ref_pos: usize,
    b1_idx: usize,
    pub(crate) pixels: u32,
    pub(crate) color: Color,
    // The imaginary initial a0 precedes column zero, including for a black row.
    pub(crate) initial: bool,
    pub(crate) decoded_rows: u32,
    pub(crate) row_limit: u32,
}

impl DecoderContext {
    /// Creates a reusable decoder context with the supplied settings.
    pub fn new(settings: DecodeSettings) -> Self {
        Self {
            settings,
            ref_changes: Vec::new(),
            coding_changes: Vec::new(),
            ref_pos: 0,
            b1_idx: 0,
            pixels: 0,
            color: Color::White,
            initial: true,
            decoded_rows: 0,
            row_limit: u32::MAX,
        }
    }

    pub(crate) fn reset(&mut self) {
        self.ref_changes.clear();
        self.reset_row();
        self.decoded_rows = 0;
    }

    pub(crate) fn reset_row(&mut self) {
        self.coding_changes.clear();
        self.pixels = 0;
        self.color = Color::White;
        self.initial = true;
        self.ref_pos = 0;
        self.update_b();
    }

    pub(crate) fn b1(&self) -> u32 {
        self.ref_changes
            .get(self.b1_idx)
            .map_or(self.settings.columns, |c| c.idx)
    }

    pub(crate) fn b2(&self) -> u32 {
        self.ref_changes
            .get(self.b1_idx.saturating_add(1))
            .map_or(self.settings.columns, |c| c.idx)
    }

    pub(crate) fn update_b(&mut self) {
        let target = self.color.opposite();
        self.b1_idx = self.ref_changes.len();
        for i in self.ref_pos..self.ref_changes.len() {
            let change = &self.ref_changes[i];
            if !self.initial && change.idx <= self.pixels {
                self.ref_pos = i + 1;
            } else if change.color == target {
                self.b1_idx = i;
                break;
            }
        }
    }

    pub(crate) fn push(&mut self, count: u32) -> Result<()> {
        if count > self.settings.columns - self.pixels {
            return Err(DecodeError::LineLengthMismatch);
        }
        if count != 0 {
            if self
                .coding_changes
                .last()
                .map_or(!self.color.is_white(), |last| last.color != self.color)
            {
                self.coding_changes
                    .try_reserve(1)
                    .map_err(|_| DecodeError::LimitExceeded)?;
                self.coding_changes.push(ColorChange {
                    idx: self.pixels,
                    color: self.color,
                });
            }
            self.pixels += count;
        }
        self.initial = false;
        Ok(())
    }

    pub(crate) fn at_eol(&self) -> bool {
        self.pixels == self.settings.columns
    }

    pub(crate) fn finish(&mut self, output: &mut impl Output) -> Result<()> {
        if !self.at_eol() {
            return Err(DecodeError::LineLengthMismatch);
        }
        if self.decoded_rows == self.row_limit {
            return Err(DecodeError::LimitExceeded);
        }
        output.row(
            &self.coding_changes,
            self.settings.columns,
            self.settings.invert_black,
        )?;
        core::mem::swap(&mut self.ref_changes, &mut self.coding_changes);
        self.decoded_rows += 1;
        self.reset_row();
        Ok(())
    }

    pub(crate) fn repair(
        &mut self,
        previous_damaged: bool,
        output: &mut impl Output,
    ) -> Result<()> {
        self.reset_row();
        if !previous_damaged {
            self.coding_changes
                .try_reserve(self.ref_changes.len())
                .map_err(|_| DecodeError::LimitExceeded)?;
            self.coding_changes.extend_from_slice(&self.ref_changes);
        }
        self.pixels = self.settings.columns;
        self.finish(output)
    }
}
