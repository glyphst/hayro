use crate::bit_reader::BitReader;
use crate::context::{DecoderContext, Output};
use crate::decode::Mode;
use crate::{Color, DecodeError, EncodingMode, Result};

pub(crate) fn line(
    ctx: &mut DecoderContext,
    reader: &mut BitReader<'_>,
    output: &mut impl Output,
    one_dimensional: bool,
) -> Result<()> {
    while !ctx.at_eol() {
        if one_dimensional {
            // T.4 Table 5 has a distinct twelve-bit entrance on an MH line.
            if reader.peak_bits(12) == Ok(0b0000_0000_1111) {
                reader.read_bits(12)?;
                uncompressed(ctx, reader, output)?;
            } else {
                let count = reader.decode_run(ctx.color)?;
                ctx.push(count)?;
                ctx.color = ctx.color.opposite();
            }
        } else {
            match reader.decode_mode()? {
                Mode::Pass => {
                    let count = ctx
                        .b2()
                        .checked_sub(ctx.pixels)
                        .ok_or(DecodeError::Overflow)?;
                    if count == 0 {
                        return Err(DecodeError::InvalidCode);
                    }
                    ctx.push(count)?;
                    ctx.update_b();
                }
                Mode::Vertical(offset) => {
                    let a1 = ctx
                        .b1()
                        .checked_add_signed(i32::from(offset))
                        .ok_or(DecodeError::Overflow)?;
                    ctx.push(a1.checked_sub(ctx.pixels).ok_or(DecodeError::Overflow)?)?;
                    ctx.color = ctx.color.opposite();
                    ctx.update_b();
                }
                Mode::Horizontal => {
                    let start = ctx.pixels;
                    let count = reader.decode_run(ctx.color)?;
                    ctx.push(count)?;
                    ctx.color = ctx.color.opposite();
                    let count = reader.decode_run(ctx.color)?;
                    ctx.push(count)?;
                    ctx.color = ctx.color.opposite();
                    if start == ctx.pixels {
                        return Err(DecodeError::InvalidCode);
                    }
                    ctx.update_b();
                }
                Mode::Uncompressed => uncompressed(ctx, reader, output)?,
            }
        }
    }
    Ok(())
}

fn literal(
    ctx: &mut DecoderContext,
    output: &mut impl Output,
    color: Color,
    mut count: u32,
) -> Result<()> {
    while count > 0 {
        if ctx.at_eol() {
            // T.6 §2.3.1 explicitly concatenates literals across scan lines.
            if ctx.settings.encoding != EncodingMode::Group4 || ctx.settings.rows_are_byte_aligned {
                return Err(DecodeError::LineLengthMismatch);
            }
            ctx.finish(output)?;
        }
        ctx.color = color;
        let chunk = count.min(ctx.settings.columns - ctx.pixels);
        ctx.push(chunk)?;
        count -= chunk;
    }
    Ok(())
}

fn uncompressed(
    ctx: &mut DecoderContext,
    reader: &mut BitReader<'_>,
    output: &mut impl Output,
) -> Result<()> {
    loop {
        let mut zeros = 0;
        while reader.read_bit()? == 0 {
            zeros += 1;
            if zeros > 10 {
                return Err(DecodeError::InvalidCode);
            }
        }
        if zeros < 5 {
            literal(ctx, output, Color::White, zeros)?;
            literal(ctx, output, Color::Black, 1)?;
        } else if zeros == 5 {
            literal(ctx, output, Color::White, 5)?;
        } else {
            literal(ctx, output, Color::White, zeros - 6)?;
            ctx.color = if reader.read_bit()? == 1 {
                Color::Black
            } else {
                Color::White
            };
            ctx.update_b();
            return Ok(());
        }
    }
}
