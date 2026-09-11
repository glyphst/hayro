use crate::context::{ColorChange, DecoderContext, Output, runs};
use crate::{DecodeError, DecodeSettings, Result, framing};
use alloc::vec::Vec;

/// Packed PDF samples and the extent of the decoded fax stream.
pub struct PdfImage {
    /// Most-significant-bit-first samples, with each row padded to a whole byte.
    pub data: Vec<u8>,
    /// Actual number of decoded rows, independent of an overridden `Rows` value.
    pub height: u32,
    /// Encoded bytes consumed, rounded up after the terminating code or row.
    pub consumed: usize,
}

/// Decode a PDF 1.7 §7.4.6 CCITT filter with a packed-output byte limit.
///
/// Unlike [`crate::decode`], `end_of_block` requires RTC/EOFB and overrides
/// `rows`. Without it, zero rows means an unknown height. A nonzero damaged-row
/// budget only applies to Group 3 data with required EOL markers. Recovery
/// substitutes the preceding undamaged row, or a white row after damage.
/// No partial output is returned on failure. Allocation growth is fallible.
pub fn decode_pdf(
    data: &[u8],
    settings: DecodeSettings,
    damaged_rows_before_error: u32,
    max_output_bytes: usize,
) -> Result<PdfImage> {
    struct Packed {
        data: Vec<u8>,
        limit: usize,
    }
    impl Output for Packed {
        fn row(&mut self, changes: &[ColorChange], width: u32, invert: bool) -> Result<()> {
            let len = (width as usize).div_ceil(8);
            let start = self.data.len();
            let end = start
                .checked_add(len)
                .filter(|end| *end <= self.limit)
                .ok_or(DecodeError::LimitExceeded)?;
            self.data
                .try_reserve(len)
                .map_err(|_| DecodeError::LimitExceeded)?;
            self.data.resize(end, 0);
            let row = &mut self.data[start..end];
            let mut x = 0_u32;
            runs(changes, width, |color, count| {
                let stop = x + count;
                if color.is_white() ^ invert {
                    let first = x as usize / 8;
                    let last = (stop - 1) as usize / 8;
                    let head = u8::MAX >> (x % 8);
                    let tail = u8::MAX << ((8 - stop % 8) % 8);
                    if first == last {
                        row[first] |= head & tail;
                    } else {
                        row[first] |= head;
                        row[first + 1..last].fill(u8::MAX);
                        row[last] |= tail;
                    }
                }
                x = stop;
            });
            Ok(())
        }
    }
    if (settings.columns as usize).div_ceil(8) > max_output_bytes {
        return Err(DecodeError::LimitExceeded);
    }
    let mut output = Packed {
        data: Vec::new(),
        limit: max_output_bytes,
    };
    let mut ctx = DecoderContext::new(settings);
    let consumed = framing::decode(data, &mut output, &mut ctx, Some(damaged_rows_before_error))?;
    Ok(PdfImage {
        data: output.data,
        height: ctx.decoded_rows,
        consumed,
    })
}
