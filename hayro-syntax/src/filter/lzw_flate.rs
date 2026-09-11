pub(crate) mod flate {
    use crate::filter::predictor::{PredictorParams, apply_predictor};
    use crate::object::Dict;
    use alloc::vec::Vec;

    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    pub(crate) enum LimitedDecodeFailure {
        Decode,
        LimitExceeded,
    }

    pub(crate) fn decode(data: &[u8], params: &Dict<'_>) -> Option<Vec<u8>> {
        let params = PredictorParams::from_params(params)?;
        let decoded = decode_with_limit(data, usize::MAX).ok()?;
        apply_predictor(decoded, &params)
    }

    // An inline image ends at EI rather than a stream Length. Preserve all
    // compressed bytes, including whitespace-valued checksums, and accept only
    // PDF whitespace after the decoder reports the end of the compressed data.
    pub(crate) fn decode_inline(data: &[u8], params: &Dict<'_>) -> Option<Vec<u8>> {
        let params = PredictorParams::from_params(params)?;
        let decoded = decode_with_limit_inner(data, usize::MAX, true).ok()?;
        apply_predictor(decoded, &params)
    }

    pub(crate) fn decode_with_limit(
        data: &[u8],
        max_output_bytes: usize,
    ) -> Result<Vec<u8>, LimitedDecodeFailure> {
        decode_with_limit_inner(data, max_output_bytes, false)
    }

    // PDF Flate is a zlib stream (RFC 1950), with no preset dictionary.
    pub(crate) fn zlib_window(data: &[u8]) -> Option<usize> {
        let &[cmf, flg] = data.get(..2)? else {
            return None;
        };
        ((cmf & 15) == 8
            && cmf >> 4 <= 7
            && flg & 32 == 0
            && u16::from_be_bytes([cmf, flg]).is_multiple_of(31))
        .then(|| 1_usize << ((cmf >> 4) + 8))
    }

    pub(crate) fn valid_suffix(data: &[u8], inline_suffix: bool) -> bool {
        data.is_empty()
            || inline_suffix
                && data
                    .iter()
                    .all(|b| crate::trivia::is_white_space_character(*b))
    }

    fn decode_with_limit_inner(
        data: &[u8],
        max_output_bytes: usize,
        inline_suffix: bool,
    ) -> Result<Vec<u8>, LimitedDecodeFailure> {
        let window = zlib_window(data).ok_or(LimitedDecodeFailure::Decode)?;
        #[cfg(feature = "unsafe")]
        if window == 32768 {
            match decode_accelerated(data, max_output_bytes, inline_suffix) {
                // zlib-rs rejects HDIST 31/32 even when reserved symbols are
                // unused. RFC 1951 permits these alphabets; retry with the
                // portable validator only after releasing the first output.
                Err(LimitedDecodeFailure::Decode) => {
                    return super::super::flate_portable::decode(
                        data,
                        max_output_bytes,
                        inline_suffix,
                        true,
                    );
                }
                result => return result,
            }
        }
        // zlib-rs does not enforce the smaller history window declared by CMF.
        let _ = window;
        super::super::flate_portable::decode(data, max_output_bytes, inline_suffix, false)
    }

    #[cfg(feature = "unsafe")]
    fn decode_accelerated(
        data: &[u8],
        max_output_bytes: usize,
        inline_suffix: bool,
    ) -> Result<Vec<u8>, LimitedDecodeFailure> {
        use flate2::{Decompress, FlushDecompress, Status};

        let mut decoder = Decompress::new(true);
        let mut input_offset = 0_usize;
        let mut result = Vec::new();
        let mut buffer = [0_u8; 8192];
        loop {
            let remaining = max_output_bytes.saturating_sub(result.len());
            let output_len = buffer.len().min(remaining.saturating_add(1));
            if output_len == 0 {
                return Err(LimitedDecodeFailure::LimitExceeded);
            }
            let input_before = decoder.total_in();
            let output_before = decoder.total_out();
            let status = decoder
                .decompress(
                    &data[input_offset..],
                    &mut buffer[..output_len],
                    FlushDecompress::None,
                )
                .map_err(|_| LimitedDecodeFailure::Decode)?;
            let consumed = usize::try_from(decoder.total_in() - input_before)
                .map_err(|_| LimitedDecodeFailure::Decode)?;
            let produced = usize::try_from(decoder.total_out() - output_before)
                .map_err(|_| LimitedDecodeFailure::Decode)?;
            input_offset = input_offset
                .checked_add(consumed)
                .ok_or(LimitedDecodeFailure::Decode)?;
            if produced > remaining {
                return Err(LimitedDecodeFailure::LimitExceeded);
            }
            result
                .try_reserve_exact(produced)
                .map_err(|_| LimitedDecodeFailure::Decode)?;
            result.extend_from_slice(&buffer[..produced]);

            match status {
                Status::StreamEnd if valid_suffix(&data[input_offset..], inline_suffix) => {
                    return Ok(result);
                }
                Status::StreamEnd => return Err(LimitedDecodeFailure::Decode),
                Status::Ok | Status::BufError if consumed != 0 || produced != 0 => {}
                Status::Ok | Status::BufError => return Err(LimitedDecodeFailure::Decode),
            }
        }
    }
}

pub(crate) mod lzw {
    use crate::bit_reader::BitReader;
    use crate::filter::predictor::{PredictorParams, apply_predictor, integer_param};
    use crate::object::Dict;
    use crate::object::dict::keys::EARLY_CHANGE;
    use alloc::vec;
    use alloc::vec::Vec;

    /// Decode a LZW-encoded stream.
    pub(crate) fn decode(data: &[u8], params: &Dict<'_>) -> Option<Vec<u8>> {
        let early_change = match integer_param(params, EARLY_CHANGE, 1)? {
            0 => false,
            1 => true,
            _ => return None,
        };
        let params = PredictorParams::from_params(params)?;
        let decoded = decode_impl(data, early_change)?;

        apply_predictor(decoded, &params)
    }

    const CLEAR_TABLE: usize = 256;
    const EOD: usize = 257;
    const MAX_ENTRIES: usize = 4096;
    const INITIAL_SIZE: u16 = 258;

    fn decode_impl(data: &[u8], early_change: bool) -> Option<Vec<u8>> {
        let mut table = Table::new(early_change);
        let mut bit_size = table.code_length();
        let mut reader = BitReader::new(data);
        let mut decoded = vec![];
        let mut prev = None;

        // PDF 1.7, 7.4.4.2 requires an initial clear, a complete EOD, and
        // zero padding through the end of the byte containing that EOD.
        if reader.read(bit_size)? as usize != CLEAR_TABLE {
            return None;
        }
        loop {
            let next = match reader.read(bit_size) {
                Some(code) => code as usize,
                None => {
                    warn!("premature EOF in LZW stream, EOD code missing");
                    return None;
                }
            };

            match next {
                CLEAR_TABLE => {
                    table.clear();
                    prev = None;
                    bit_size = table.code_length();
                }
                EOD => {
                    let padding = (8 - reader.bit_pos()) % 8;
                    if padding != 0 && reader.read(padding as u8)? != 0 {
                        return None;
                    }
                    return Some(decoded);
                }
                new => {
                    if table.size() == MAX_ENTRIES || new > table.size() {
                        warn!("invalid LZW code: {} (table size: {})", new, table.size());
                        return None;
                    }

                    if new < table.size() {
                        let entry = table.get(new)?;
                        let first_byte = entry[0];
                        decoded.extend_from_slice(entry);

                        if let Some(prev_code) = prev {
                            table.register(prev_code, first_byte);
                        }
                    } else if new == table.size() && prev.is_some() {
                        let prev_code = prev.unwrap();
                        let prev_entry = table.get(prev_code)?;
                        let first_byte = prev_entry[0];

                        let new_entry = table.register(prev_code, first_byte)?;
                        decoded.extend_from_slice(new_entry);
                    } else {
                        warn!("LZW decode error: code {new} not found and prev is None");
                        return None;
                    }

                    bit_size = table.code_length();
                    prev = Some(new);
                }
            }
        }
    }

    struct Table {
        early_change: bool,
        entries: Vec<Option<Vec<u8>>>,
    }

    impl Table {
        fn new(early_change: bool) -> Self {
            let mut entries: Vec<_> = (0..=255).map(|b| Some(vec![b])).collect();

            // Clear table and EOD don't have any data.
            entries.push(None); // 256 = CLEAR_TABLE
            entries.push(None); // 257 = EOD

            Self {
                early_change,
                entries,
            }
        }

        fn push(&mut self, entry: Vec<u8>) -> Option<&[u8]> {
            if self.entries.len() >= MAX_ENTRIES {
                None
            } else {
                self.entries.push(Some(entry));
                self.entries.last()?.as_ref().map(|v| &**v)
            }
        }

        fn register(&mut self, prev: usize, new_byte: u8) -> Option<&[u8]> {
            if self.entries.len() >= MAX_ENTRIES {
                return None;
            }
            let prev_entry = self.get(prev)?;

            let mut new_entry = Vec::with_capacity(prev_entry.len() + 1);
            new_entry.extend(prev_entry);
            new_entry.push(new_byte);
            self.push(new_entry)
        }

        fn get(&self, index: usize) -> Option<&[u8]> {
            self.entries.get(index)?.as_ref().map(|v| &**v)
        }

        fn clear(&mut self) {
            self.entries.truncate(INITIAL_SIZE as usize);
        }

        fn size(&self) -> usize {
            self.entries.len()
        }

        fn code_length(&self) -> u8 {
            const TEN: usize = 512;
            const ELEVEN: usize = 1024;
            const TWELVE: usize = 2048;

            let adjusted = self.entries.len() + (if self.early_change { 1 } else { 0 });

            if adjusted >= TWELVE {
                12
            } else if adjusted >= ELEVEN {
                11
            } else if adjusted >= TEN {
                10
            } else {
                9
            }
        }
    }
}

#[cfg(test)]
#[rustfmt::skip]
mod tests {
    use crate::filter::lzw_flate::{flate, lzw};
    use crate::filter::predictor::{PredictorParams, apply_predictor};
    use crate::object::Dict;

    #[test]
    fn decode_lzw() {
        let input = [0x80, 0x0B, 0x60, 0x50, 0x22, 0x0C, 0x0C, 0x85, 0x01];
        let decoded = lzw::decode(&input, &Dict::default()).unwrap();

        assert_eq!(decoded, vec![45, 45, 45, 45, 45, 65, 45, 45, 45, 66]);
    }

    #[test]
    fn decode_flate_zlib() {
        let input = [
            0x78, 0x9c, 0xf3, 0x48, 0xcd, 0xc9, 0xc9, 0x7, 0x0, 0x5, 0x8c, 0x1, 0xf5,
        ];

        let decoded = flate::decode(&input, &Dict::default()).unwrap();
        assert_eq!(decoded, b"Hello");
    }

    #[test]
    fn raw_deflate_is_not_a_pdf_flate_stream() {
        let input = [0xf3, 0x48, 0xcd, 0xc9, 0xc9, 0x7, 0x0];
        assert!(flate::decode(&input, &Dict::default()).is_none());
    }

    fn predictor_expected() -> Vec<u8> {
        vec![
            // Row 1
            127, 127, 127, 125, 129, 127, 123, 130, 128, 
            // Row 2
            128, 129, 126, 126, 132, 124, 121, 127, 126, 
            // Row 3
            131, 130, 122, 133, 129, 128, 127, 100, 126,
        ]
    }

    fn predictor_test(predictor: u8, input: &[u8]) {
        let params = PredictorParams {
            predictor,
            colors: 3,
            bits_per_component: 8,
            columns: 3,
        };

        let expected = predictor_expected();
        let out = apply_predictor(input.to_vec(), &params).unwrap();

        assert_eq!(expected, out);
    }

    #[test]
    fn predictor_sub() {
        predictor_test(
            11,
            &[
                // Row 1
                1, 127, 127, 127, 254, 2, 0, 254, 1, 1, 
                // Row 2
                1, 128, 129, 126, 254, 3, 254, 251, 251, 2, 
                // Row 3
                1, 131, 130, 122, 2, 255, 6, 250, 227, 254,
            ],
        );
    }

    #[test]
    fn predictor_up() {
        predictor_test(
            12,
            &[
                // Row 1
                2, 127, 127, 127, 125, 129, 127, 123, 130, 128, 
                // Row 2
                2, 1, 2, 255, 1, 3, 253, 254, 253, 254, 
                // Row 3
                2, 3, 1, 252, 7, 253, 4, 6, 229, 0,
            ],
        );
    }

    #[test]
    fn predictor_avg() {
        predictor_test(
            13,
            &[
                // Row 1
                3, 127, 127, 127, 62, 66, 64, 61, 66, 65, 
                // Row 2
                3, 65, 66, 63, 0, 3, 254, 253, 252, 0, 
                // Row 3
                3, 67, 66, 59, 5, 254, 5, 0, 228, 255,
            ],
        );
    }

    #[test]
    fn predictor_paeth() {
        predictor_test(
            14,
            &[
                // Row 1
                4, 127, 127, 127, 254, 2, 0, 254, 1, 1, 
                // Row 2
                4, 1, 2, 255, 1, 3, 254, 254, 251, 2, 
                // Row 3
                4, 3, 1, 252, 5, 253, 6, 1, 229, 254,
            ],
        );
    }
}
