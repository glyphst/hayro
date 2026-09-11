//! A decoder for CCITT fax-encoded images.
//!
//! This crate implements the CCITT Group 3 and Group 4 fax compression algorithms
//! as defined in ITU-T Recommendations T.4 and T.6. These encodings are commonly
//! used for bi-level (black and white) images in PDF documents and fax transmissions.
//!
//! The main entry point is the [`decode`] function, which takes encoded data, a
//! [`DecoderContext`], and outputs the decoded pixels through a [`Decoder`] trait
//! that can be implemented according to your needs.
//!
//! The crate is `no_std` compatible but requires an allocator to be available.
//!
//! # Safety
//! Unsafe code is forbidden via a crate-level attribute.
//!
//! # License
//! Licensed under either of
//!
//! - Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE) or <http://www.apache.org/licenses/LICENSE-2.0>)
//! - MIT license ([LICENSE-MIT](LICENSE-MIT) or <http://opensource.org/licenses/MIT>)
//!
//! at your option.
//!
//! [`decode`]: crate::decode
//! [`Decoder`]: crate::Decoder

#![no_std]
#![forbid(unsafe_code)]
#![forbid(missing_docs)]

extern crate alloc;

mod bit_reader;
mod decode;
mod state_machine;

/// A specialized Result type for CCITT decoding operations.
pub type Result<T> = core::result::Result<T, DecodeError>;

/// An error that can occur during CCITT decoding.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecodeError {
    /// Unexpected end of input while reading bits.
    UnexpectedEof,
    /// Invalid Huffman code sequence was encountered during decoding.
    InvalidCode,
    /// A scanline didn't have the expected number of pixels.
    LineLengthMismatch,
    /// Arithmetic overflow in run length or position calculation.
    Overflow,
    /// The decoded output or allocation exceeds its allowed capacity.
    LimitExceeded,
}

impl core::fmt::Display for DecodeError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::UnexpectedEof => write!(f, "unexpected end of input"),
            Self::InvalidCode => write!(f, "invalid CCITT code sequence"),
            Self::LineLengthMismatch => write!(f, "scanline length mismatch"),
            Self::LimitExceeded => write!(f, "CCITT output or allocation limit exceeded"),
            Self::Overflow => write!(f, "arithmetic overflow in position calculation"),
        }
    }
}

impl core::error::Error for DecodeError {}

/// The encoding mode for CCITT fax decoding.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum EncodingMode {
    /// Group 4 (MMR).
    Group4,
    /// Group 3 1D (MH).
    Group3_1D,
    /// Group 3 2D (MR).
    Group3_2D {
        /// The K parameter.
        k: u32,
    },
}

/// Settings to apply during decoding.
#[derive(Copy, Clone, Debug)]
pub struct DecodeSettings {
    /// How many columns the image has (i.e. its width).
    pub columns: u32,
    /// How many rows the image has (i.e. its height).
    ///
    /// Zero means unknown. [`decode`] stops at this row count or an optional
    /// end marker. [`decode_pdf`] ignores it when `end_of_block` is true.
    pub rows: u32,
    /// Enable end-of-block termination. The marker is optional for [`decode`]
    /// (T.88 MMR) and required for [`decode_pdf`] (PDF Table 11).
    pub end_of_block: bool,
    /// Whether the stream contains end-of-line markers.
    pub end_of_line: bool,
    /// Whether the data in the stream for each row is aligned to the byte
    /// boundary.
    pub rows_are_byte_aligned: bool,
    /// The encoding mode used by the image.
    pub encoding: EncodingMode,
    /// Whether black and white should be inverted.
    pub invert_black: bool,
}

/// A decoder for CCITT images.
pub trait Decoder {
    /// Push a run of pixels of the same color.
    fn push_pixels(&mut self, white: bool, count: u32);
    /// Called when a row has been completed.
    fn next_line(&mut self);
}

/// Pixel color in a bi-level (black and white) image.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Color {
    /// White pixel.
    White,
    /// Black pixel.
    Black,
}

impl Color {
    /// Returns the opposite color.
    #[inline(always)]
    fn opposite(self) -> Self {
        match self {
            Self::White => Self::Black,
            Self::Black => Self::White,
        }
    }

    /// Returns true if this color is white.
    #[inline(always)]
    fn is_white(self) -> bool {
        matches!(self, Self::White)
    }
}

mod coding;
mod context;
mod framing;
mod pdf;

pub use context::DecoderContext;
pub use pdf::{PdfImage, decode_pdf};

/// Decode row-bounded fax data, accepting an optional end-of-block marker.
///
/// This entry point retains the T.88 MMR contract: `rows` caps the decoded
/// height, and an EOFB may be present or absent. Successful consumption is
/// rounded up to a whole byte. Use [`decode_pdf`] for PDF filter termination.
/// A failure may follow previously emitted complete rows.
pub fn decode(data: &[u8], decoder: &mut impl Decoder, ctx: &mut DecoderContext) -> Result<usize> {
    struct Output<'a, D>(&'a mut D);
    impl<D: Decoder> context::Output for Output<'_, D> {
        fn row(
            &mut self,
            changes: &[context::ColorChange],
            width: u32,
            invert: bool,
        ) -> Result<()> {
            context::runs(changes, width, |color, count| {
                self.0.push_pixels(color.is_white() ^ invert, count);
            });
            self.0.next_line();
            Ok(())
        }
    }
    framing::decode(data, &mut Output(decoder), ctx, None)
}

#[cfg(test)]
mod tests;
