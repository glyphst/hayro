use crate::object::dict::keys::*;
use crate::object::stream::{FilterResult, ImageDecodeParams, optional_entry};
use crate::object::{Dict, Object};
use hayro_ccitt::{DecodeSettings, EncodingMode};

fn integer(params: &Dict<'_>, key: &[u8], default: i64) -> Option<i64> {
    match optional_entry(params, key).ok()? {
        None => Some(default),
        Some(Object::Number(number)) => number.as_i64_exact(),
        _ => None,
    }
}

fn boolean(params: &Dict<'_>, key: &[u8], default: bool) -> Option<bool> {
    match optional_entry(params, key).ok()? {
        None => Some(default),
        Some(Object::Boolean(value)) => Some(value),
        _ => None,
    }
}

pub(crate) fn decode(
    data: &[u8],
    params: &Dict<'_>,
    image_params: &ImageDecodeParams,
) -> Option<FilterResult<'static>> {
    if image_params.bpc.is_some_and(|bpc| bpc != 1)
        || image_params.num_components.is_some_and(|count| count != 1)
    {
        return None;
    }
    let k = integer(params, K, 0)?;
    let columns = u32::try_from(integer(params, COLUMNS, 1728)?).ok()?;
    let rows = u32::try_from(integer(params, ROWS, 0)?).ok()?;
    let damage = u32::try_from(integer(params, b"DamagedRowsBeforeError", 0)?).ok()?;
    let settings = DecodeSettings {
        columns,
        rows,
        end_of_block: boolean(params, END_OF_BLOCK, true)?,
        end_of_line: boolean(params, END_OF_LINE, false)?,
        rows_are_byte_aligned: boolean(params, ENCODED_BYTE_ALIGN, false)?,
        encoding: if k < 0 {
            EncodingMode::Group4
        } else if k == 0 {
            EncodingMode::Group3_1D
        } else {
            EncodingMode::Group3_2D { k: 1 }
        },
        invert_black: boolean(params, BLACK_IS_1, false)?,
    };
    // A fixed codec ceiling also covers generic streams with unknown height.
    // Retained image/page storage remains subject to the caller's normal limits.
    const MAX_PACKED_BYTES: usize = 64 * 1024 * 1024;
    let decoded = hayro_ccitt::decode_pdf(data, settings, damage, MAX_PACKED_BYTES).ok()?;
    // CCITT delivers row-padded one-bit bytes to the next filter or consumer.
    // Columns/Rows control fax decoding; they do not replace the image dictionary.
    Some(FilterResult::from_data(decoded.data))
}

#[cfg(test)]
mod tests {
    use super::decode;
    use crate::object::stream::ImageDecodeParams;
    use crate::object::{Dict, FromBytes};

    #[test]
    fn issue1258_packed_black_row_requires_declared_termination() {
        let params = Dict::from_bytes(b"<< /K 0 /Columns 8 /Rows 1 /EndOfBlock false >>").unwrap();
        let decoded = decode(&[0x35, 0x14], &params, &ImageDecodeParams::default()).unwrap();
        assert_eq!(decoded.data.as_ref(), &[0]);
        let missing_rtc = Dict::from_bytes(b"<< /K 0 /Columns 8 /Rows 1 >>").unwrap();
        assert!(decode(&[0x35, 0x14], &missing_rtc, &ImageDecodeParams::default()).is_none());
    }
}
