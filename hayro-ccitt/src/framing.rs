use crate::bit_reader::BitReader;
use crate::context::{DecoderContext, Output};
use crate::decode::EOFB;
use crate::{DecodeError, EncodingMode, Result, coding};

pub(crate) fn decode(
    data: &[u8],
    output: &mut impl Output,
    ctx: &mut DecoderContext,
    pdf_damage: Option<u32>,
) -> Result<usize> {
    ctx.reset();
    if ctx.settings.columns == 0 {
        return Err(DecodeError::LineLengthMismatch);
    }
    let pdf = pdf_damage.is_some();
    let require_eob = pdf && ctx.settings.end_of_block;
    ctx.row_limit = if !require_eob && ctx.settings.rows != 0 {
        ctx.settings.rows
    } else {
        u32::MAX
    };
    let mut reader = BitReader::new(data);
    let mut damaged = 0;
    let mut previous_damaged = false;
    loop {
        // T.88 permits an EOFB immediately after the last requested MMR row.
        if ctx.settings.end_of_block && end_block(&mut reader, ctx.settings.encoding) {
            break;
        }
        if ctx.decoded_rows == ctx.row_limit {
            break;
        }
        if !require_eob && reader.only_padding() {
            break;
        }
        if ctx.settings.rows_are_byte_aligned {
            reader.align_zero()?;
            if ctx.settings.end_of_block && end_block(&mut reader, ctx.settings.encoding) {
                break;
            }
            if !require_eob && reader.at_end() {
                break;
            }
        }
        let eol = reader.read_eol();
        if ctx.settings.end_of_line && !eol {
            return Err(DecodeError::InvalidCode);
        }
        if !require_eob && reader.only_padding() {
            break;
        }
        let row_start = reader.clone();
        let result = (|| {
            let one_dimensional = match ctx.settings.encoding {
                EncodingMode::Group4 => false,
                EncodingMode::Group3_1D => true,
                EncodingMode::Group3_2D { .. } => reader.read_bit()? == 1,
            };
            coding::line(ctx, &mut reader, output, one_dimensional)
        })();
        if let Err(error) = result {
            let recovery = ctx.settings.end_of_line
                && ctx.settings.encoding != EncodingMode::Group4
                && damaged < pdf_damage.unwrap_or(0)
                && !matches!(error, DecodeError::LimitExceeded);
            if !recovery {
                return Err(error);
            }
            // Search from the beginning of the damaged data: a failed code may
            // already have consumed part of its terminating EOL.
            reader = find_eol(row_start).ok_or(error)?;
            ctx.repair(previous_damaged, output)?;
            previous_damaged = true;
            damaged += 1;
        } else {
            ctx.finish(output)?;
            previous_damaged = false;
        }
    }
    reader.align();
    Ok(reader.byte_pos())
}

fn end_block(reader: &mut BitReader<'_>, encoding: EncodingMode) -> bool {
    if encoding == EncodingMode::Group4 {
        if reader.peak_bits(24) == Ok(EOFB) {
            return reader.read_bits(24).is_ok();
        }
        return false;
    }
    let mut trial = reader.clone();
    for _ in 0..6 {
        if !trial.read_eol() {
            return false;
        }
        // T.4 §4.2.4: MR RTC is six EOL+1 synchronization words.
        if matches!(encoding, EncodingMode::Group3_2D { .. }) && trial.read_bit() != Ok(1) {
            return false;
        }
    }
    *reader = trial;
    true
}

fn find_eol(mut reader: BitReader<'_>) -> Option<BitReader<'_>> {
    let mut zeros = 0;
    let mut start = reader.clone();
    loop {
        match reader.read_bit().ok()? {
            0 => zeros += 1,
            _ if zeros >= 11 => return Some(start),
            _ => {
                zeros = 0;
                start = reader.clone();
            }
        }
    }
}
