//! Bit-level reader for CCITT encoded data streams.

use crate::{DecodeError, Result};
use core::fmt::Debug;

#[derive(Debug, Clone)]
pub(crate) struct BitReader<'a> {
    data: &'a [u8],
    bit_offset: usize,
}

impl<'a> BitReader<'a> {
    #[inline(always)]
    pub(crate) fn new(data: &'a [u8]) -> Self {
        Self {
            data,
            bit_offset: 0,
        }
    }

    #[inline(always)]
    pub(crate) fn read_bit(&mut self) -> Result<u32> {
        let byte_pos = self.byte_pos();
        let byte = *self.data.get(byte_pos).ok_or(DecodeError::UnexpectedEof)? as u32;
        let shift = 7 - self.bit_pos();
        self.bit_offset += 1;
        Ok((byte >> shift) & 1)
    }

    #[inline(always)]
    pub(crate) fn read_bits(&mut self, num_bits: usize) -> Result<u32> {
        let mut result = 0_u32;

        for i in (0..num_bits).rev() {
            result |= (self.read_bit()?) << i;
        }

        Ok(result)
    }

    #[inline(always)]
    pub(crate) fn peak_bits(&mut self, num_bits: usize) -> Result<u32> {
        self.clone().read_bits(num_bits)
    }

    #[inline(always)]
    pub(crate) fn align(&mut self) {
        let bit_pos = self.bit_pos();

        if !bit_pos.is_multiple_of(8) {
            self.bit_offset += 8 - bit_pos;
        }
    }

    #[inline(always)]
    pub(crate) fn at_end(&self) -> bool {
        self.byte_pos() >= self.data.len()
    }

    #[inline(always)]
    pub(crate) fn byte_pos(&self) -> usize {
        self.bit_offset >> 3
    }

    pub(crate) fn bit_offset(&self) -> usize {
        self.bit_offset
    }

    pub(crate) fn only_padding(&self) -> bool {
        let mut trial = self.clone();
        // At most the unused tail of the current byte can be padding.
        if self.data.len().saturating_sub(self.byte_pos()) > usize::from(self.bit_pos() != 0) {
            return false;
        }
        while !trial.at_end() {
            if trial.read_bit() != Ok(0) {
                return false;
            }
        }
        true
    }

    pub(crate) fn align_zero(&mut self) -> Result<()> {
        while self.bit_pos() != 0 {
            if self.read_bit()? != 0 {
                return Err(DecodeError::InvalidCode);
            }
        }
        Ok(())
    }

    #[inline(always)]
    fn bit_pos(&self) -> usize {
        self.bit_offset & 7
    }
}
