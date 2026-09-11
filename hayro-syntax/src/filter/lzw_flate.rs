pub(crate) mod flate {
    use crate::filter::predictor::{PredictorParams, apply_predictor};
    use crate::object::Dict;
    use alloc::vec::Vec;

    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    pub(crate) enum LimitedDecodeFailure {
        Decode,
        LimitExceeded,
    }

    #[cfg(feature = "unsafe")]
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
        #[cfg(feature = "unsafe")]
        let decoded = decode_with_limit_inner(data, usize::MAX, true).ok()?;
        #[cfg(not(feature = "unsafe"))]
        let decoded = fallback::decode(data)?;
        apply_predictor(decoded, &params)
    }

    #[cfg(feature = "unsafe")]
    pub(crate) fn decode_with_limit(
        data: &[u8],
        max_output_bytes: usize,
    ) -> Result<Vec<u8>, LimitedDecodeFailure> {
        decode_with_limit_inner(data, max_output_bytes, false)
    }

    #[cfg(feature = "unsafe")]
    fn decode_with_limit_inner(
        data: &[u8],
        max_output_bytes: usize,
        inline_suffix: bool,
    ) -> Result<Vec<u8>, LimitedDecodeFailure> {
        use flate2::{Decompress, FlushDecompress, Status};

        let has_zlib_header = data.len() >= 2
            && (data[0] & 0x0f) == 0x08
            && ((data[0] as u16) << 8 | data[1] as u16).is_multiple_of(31);
        let mut decoder = Decompress::new(has_zlib_header);
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
                Status::StreamEnd
                    if input_offset == data.len()
                        || inline_suffix
                            && data[input_offset..]
                                .iter()
                                .all(|byte| crate::trivia::is_white_space_character(*byte)) =>
                {
                    return Ok(result);
                }
                Status::StreamEnd => return Err(LimitedDecodeFailure::Decode),
                Status::Ok | Status::BufError if consumed != 0 || produced != 0 => {}
                Status::Ok | Status::BufError => return Err(LimitedDecodeFailure::Decode),
            }
        }
    }

    #[cfg(not(feature = "unsafe"))]
    pub(crate) fn decode(data: &[u8], params: &Dict<'_>) -> Option<Vec<u8>> {
        let params = PredictorParams::from_params(params)?;
        let decoded = fallback::decode(data)?;
        apply_predictor(decoded, &params)
    }

    #[cfg(not(feature = "unsafe"))]
    pub(crate) fn decode_with_limit(
        data: &[u8],
        max_output_bytes: usize,
    ) -> Result<Vec<u8>, LimitedDecodeFailure> {
        fallback::decode_with_limit(data, max_output_bytes)
    }

    /// Ported from <https://github.com/mozilla/pdf.js/blob/master/src/core/flate_stream.js>
    /// TODO: Rewrite this in idiomatic Rust.
    #[cfg(not(feature = "unsafe"))]
    mod fallback {
        use super::LimitedDecodeFailure;
        use alloc::vec;
        use alloc::vec::Vec;

        pub(crate) fn decode(data: &[u8]) -> Option<Vec<u8>> {
            flate_decode(data, None).ok()
        }

        #[cfg(not(feature = "unsafe"))]
        pub(crate) fn decode_with_limit(
            data: &[u8],
            max_output_bytes: usize,
        ) -> Result<Vec<u8>, LimitedDecodeFailure> {
            flate_decode(data, Some(max_output_bytes))
        }

        fn flate_decode(
            data: &[u8],
            max_output_bytes: Option<usize>,
        ) -> Result<Vec<u8>, LimitedDecodeFailure> {
            if data.is_empty() {
                return Err(LimitedDecodeFailure::Decode);
            }
            if data.len() >= 2 {
                let cmf = data[0];
                let flg = data[1];

                if (cmf & 0x0f) == 0x08
                    && ((cmf as u16) << 8 | flg as u16).is_multiple_of(31)
                    && (flg & 0x20) == 0
                {
                    let mut stream = FlateStream::new(&data[2..], max_output_bytes);
                    return stream.decode();
                }
            }

            let mut stream = FlateStream::new(data, max_output_bytes);
            stream.decode()
        }

        struct FlateStream<'a> {
            data: &'a [u8],
            pos: usize,
            code_buf: u32,
            code_size: u8,
            output: Vec<u8>,
            eof: bool,
            failed: bool,
            max_output_bytes: Option<usize>,
            limit_exceeded: bool,
        }

        impl<'a> FlateStream<'a> {
            fn new(data: &'a [u8], max_output_bytes: Option<usize>) -> Self {
                FlateStream {
                    data,
                    pos: 0,
                    code_buf: 0,
                    code_size: 0,
                    output: Vec::new(),
                    eof: false,
                    failed: false,
                    max_output_bytes,
                    limit_exceeded: false,
                }
            }

            fn decode(&mut self) -> Result<Vec<u8>, LimitedDecodeFailure> {
                while !self.eof {
                    self.read_block();
                }

                if self.limit_exceeded {
                    Err(LimitedDecodeFailure::LimitExceeded)
                } else if self.failed {
                    Err(LimitedDecodeFailure::Decode)
                } else {
                    Ok(core::mem::take(&mut self.output))
                }
            }

            fn can_extend_output(&mut self, additional: usize) -> bool {
                let Some(max_output_bytes) = self.max_output_bytes else {
                    return true;
                };
                if self
                    .output
                    .len()
                    .checked_add(additional)
                    .is_some_and(|new_len| new_len <= max_output_bytes)
                {
                    true
                } else {
                    self.limit_exceeded = true;
                    self.eof = true;
                    false
                }
            }

            fn get_byte(&mut self) -> Option<u8> {
                if self.pos >= self.data.len() {
                    None
                } else {
                    let byte = self.data[self.pos];
                    self.pos += 1;
                    Some(byte)
                }
            }

            fn get_bytes(&mut self, n: usize) -> Vec<u8> {
                let end = (self.pos + n).min(self.data.len());
                let bytes = self.data[self.pos..end].to_vec();
                self.pos = end;
                bytes
            }

            fn get_bits(&mut self, bits: u8) -> Option<u32> {
                while self.code_size < bits {
                    let b = self.get_byte()?;
                    self.code_buf |= (b as u32) << self.code_size;
                    self.code_size += 8;
                }

                let result = self.code_buf & ((1 << bits) - 1);
                self.code_buf >>= bits;
                self.code_size -= bits;

                Some(result)
            }

            fn get_code(&mut self, table: &HuffmanTable) -> Option<u16> {
                let codes = &table.codes;
                let max_len = table.max_len;

                while self.code_size < max_len {
                    if let Some(b) = self.get_byte() {
                        self.code_buf |= (b as u32) << self.code_size;
                        self.code_size += 8;
                    } else {
                        // Premature end of stream
                        break;
                    }
                }

                let code = codes.get((self.code_buf & ((1 << max_len) - 1)) as usize)?;
                let code_len = (code >> 16) as u8;
                let code_val = code & 0xffff;

                if code_len < 1 || self.code_size < code_len {
                    return None;
                }

                self.code_buf >>= code_len;
                self.code_size -= code_len;

                Some(code_val as u16)
            }

            fn read_block(&mut self) {
                // Read block header
                let hdr = match self.get_bits(3) {
                    Some(h) => h,
                    None => {
                        warn!("bad block header in flate stream");
                        self.failed = true;
                        self.eof = true;
                        return;
                    }
                };

                if (hdr & 1) != 0 {
                    self.eof = true;
                }

                let hdr = hdr >> 1;

                match hdr {
                    0 => self.read_uncompressed_block(),
                    1 => self.read_compressed_block(true),
                    2 => self.read_compressed_block(false),
                    _ => {
                        warn!("unknown block type in flate stream");
                        self.failed = true;
                        self.eof = true;
                    }
                }
            }

            fn read_uncompressed_block(&mut self) {
                // Skip any remaining bits in current byte
                self.code_buf = 0;
                self.code_size = 0;

                let len_low = match self.get_byte() {
                    Some(b) => b as u16,
                    None => {
                        warn!("bad block header in flate stream");
                        self.failed = true;
                        self.eof = true;
                        return;
                    }
                };

                let len_high = match self.get_byte() {
                    Some(b) => b as u16,
                    None => {
                        warn!("bad block header in flate stream");
                        self.failed = true;
                        self.eof = true;
                        return;
                    }
                };

                let block_len = len_low | (len_high << 8);

                let nlen_low = match self.get_byte() {
                    Some(b) => b as u16,
                    None => {
                        warn!("bad block header in flate stream");
                        self.failed = true;
                        self.eof = true;
                        return;
                    }
                };

                let nlen_high = match self.get_byte() {
                    Some(b) => b as u16,
                    None => {
                        warn!("bad block header in flate stream");
                        self.failed = true;
                        self.eof = true;
                        return;
                    }
                };

                let check = nlen_low | (nlen_high << 8);

                if check != !block_len {
                    warn!("bad uncompressed block length in flate stream");
                    self.failed = true;
                    self.eof = true;
                    return;
                }

                if block_len != 0 {
                    if !self.can_extend_output(block_len as usize) {
                        return;
                    }
                    let block = self.get_bytes(block_len as usize);
                    self.output.extend_from_slice(&block);
                    if block.len() < block_len as usize {
                        self.failed = true;
                        self.eof = true;
                    }
                }
            }

            fn read_compressed_block(&mut self, fixed: bool) {
                let (lit_code_table, dist_code_table) = if fixed {
                    (get_fixed_lit_table(), get_fixed_dist_table())
                } else {
                    match self.read_dynamic_tables() {
                        Some(tables) => tables,
                        None => {
                            self.failed = true;
                            self.eof = true;
                            return;
                        }
                    }
                };

                loop {
                    let code1 = match self.get_code(&lit_code_table) {
                        Some(c) => c,
                        None => {
                            self.failed = true;
                            self.eof = true;
                            return;
                        }
                    };

                    if code1 < 256 {
                        if !self.can_extend_output(1) {
                            return;
                        }
                        self.output.push(code1 as u8);
                    } else if code1 == 256 {
                        return;
                    } else {
                        let code1 = code1 - 257;
                        let Some(&length_info) = LENGTH_DECODE.get(code1 as usize) else {
                            self.failed = true;
                            self.eof = true;
                            return;
                        };
                        let extra_bits = (length_info >> 16) as u8;
                        let mut length = (length_info & 0xffff) as usize;

                        if extra_bits > 0 {
                            if let Some(extra) = self.get_bits(extra_bits) {
                                length += extra as usize;
                            } else {
                                self.failed = true;
                                self.eof = true;
                                return;
                            }
                        }

                        let dist_code = match self.get_code(&dist_code_table) {
                            Some(c) => c,
                            None => {
                                self.failed = true;
                                self.eof = true;
                                return;
                            }
                        };

                        let dist_info = match DIST_DECODE.get(dist_code as usize) {
                            Some(&info) => info,
                            None => {
                                warn!("invalid distance code {} in flate stream", dist_code);

                                self.failed = true;
                                self.eof = true;
                                return;
                            }
                        };

                        let extra_bits = (dist_info >> 16) as u8;
                        let mut distance = (dist_info & 0xffff) as usize;

                        if extra_bits > 0 {
                            if let Some(extra) = self.get_bits(extra_bits) {
                                distance += extra as usize;
                            } else {
                                self.failed = true;
                                self.eof = true;
                                return;
                            }
                        }

                        // Copy from previous output
                        if !self.can_extend_output(length) {
                            return;
                        }
                        if distance == 0 || distance > self.output.len() {
                            self.failed = true;
                            self.eof = true;
                            return;
                        }
                        for _ in 0..length {
                            let byte = self.output[self.output.len() - distance];
                            self.output.push(byte);
                        }
                    }
                }
            }

            fn read_dynamic_tables(&mut self) -> Option<(HuffmanTable, HuffmanTable)> {
                let num_lit_codes = self.get_bits(5)? as usize + 257;
                let num_dist_codes = self.get_bits(5)? as usize + 1;
                let num_code_len_codes = self.get_bits(4)? as usize + 4;

                // Build code length code table
                let mut code_len_code_lengths = vec![0_u8; 19];
                for i in 0..num_code_len_codes {
                    code_len_code_lengths[CODE_LEN_CODE_MAP[i] as usize] = self.get_bits(3)? as u8;
                }

                let code_len_table = generate_huffman_table(&code_len_code_lengths);

                // Read code lengths
                let total_codes = num_lit_codes + num_dist_codes;
                let mut code_lengths = vec![0_u8; total_codes];
                let mut i = 0;

                while i < total_codes {
                    let code = self.get_code(&code_len_table)?;

                    match code {
                        0..=15 => {
                            code_lengths[i] = code as u8;
                            i += 1;
                        }
                        16 => {
                            // Repeat previous
                            let repeat_count = self.get_bits(2)? as usize + 3;
                            let prev = if i > 0 { code_lengths[i - 1] } else { 0 };
                            for _ in 0..repeat_count {
                                if i < total_codes {
                                    code_lengths[i] = prev;
                                    i += 1;
                                }
                            }
                        }
                        17 => {
                            // Repeat zero 3-10 times
                            let repeat_count = self.get_bits(3)? as usize + 3;
                            for _ in 0..repeat_count {
                                if i < total_codes {
                                    code_lengths[i] = 0;
                                    i += 1;
                                }
                            }
                        }
                        18 => {
                            // Repeat zero 11-138 times
                            let repeat_count = self.get_bits(7)? as usize + 11;
                            for _ in 0..repeat_count {
                                if i < total_codes {
                                    code_lengths[i] = 0;
                                    i += 1;
                                }
                            }
                        }
                        _ => return None,
                    }
                }

                let lit_table = generate_huffman_table(&code_lengths[..num_lit_codes]);
                let dist_table = generate_huffman_table(&code_lengths[num_lit_codes..]);

                Some((lit_table, dist_table))
            }
        }

        struct HuffmanTable {
            codes: Vec<u32>,
            max_len: u8,
        }

        fn generate_huffman_table(lengths: &[u8]) -> HuffmanTable {
            let _n = lengths.len();

            // Find max code length
            let max_len = lengths.iter().cloned().max().unwrap_or(0);

            if max_len == 0 {
                return HuffmanTable {
                    codes: vec![0; 1],
                    max_len: 1,
                };
            }

            // Build the table
            let size = 1 << max_len;
            let mut codes = vec![0_u32; size];

            let mut code = 0_u32;
            for len in 1..=max_len {
                for (val, &length) in lengths.iter().enumerate() {
                    if length == len {
                        // Bit-reverse the code
                        let mut code2 = 0_u32;
                        let mut t = code;
                        for _ in 0..len {
                            code2 = (code2 << 1) | (t & 1);
                            t >>= 1;
                        }

                        // Fill the table entries
                        let skip = 1 << len;
                        let mut i = code2 as usize;
                        while i < size {
                            codes[i] = ((len as u32) << 16) | (val as u32);
                            i += skip;
                        }
                        code += 1;
                    }
                }
                code <<= 1;
            }

            HuffmanTable { codes, max_len }
        }

        fn get_fixed_lit_table() -> HuffmanTable {
            HuffmanTable {
                codes: FIXED_LIT_CODE_TAB.to_vec(),
                max_len: 9,
            }
        }

        fn get_fixed_dist_table() -> HuffmanTable {
            HuffmanTable {
                codes: FIXED_DIST_CODE_TAB.to_vec(),
                max_len: 5,
            }
        }

        const CODE_LEN_CODE_MAP: [u8; 19] = [
            16, 17, 18, 0, 8, 7, 9, 6, 10, 5, 11, 4, 12, 3, 13, 2, 14, 1, 15,
        ];

        const LENGTH_DECODE: [u32; 29] = [
            0x00003, 0x00004, 0x00005, 0x00006, 0x00007, 0x00008, 0x00009, 0x0000a, 0x1000b,
            0x1000d, 0x1000f, 0x10011, 0x20013, 0x20017, 0x2001b, 0x2001f, 0x30023, 0x3002b,
            0x30033, 0x3003b, 0x40043, 0x40053, 0x40063, 0x40073, 0x50083, 0x500a3, 0x500c3,
            0x500e3, 0x00102,
        ];

        const DIST_DECODE: [u32; 30] = [
            0x00001, 0x00002, 0x00003, 0x00004, 0x10005, 0x10007, 0x20009, 0x2000d, 0x30011,
            0x30019, 0x40021, 0x40031, 0x50041, 0x50061, 0x60081, 0x600c1, 0x70101, 0x70181,
            0x80201, 0x80301, 0x90401, 0x90601, 0xa0801, 0xa0c01, 0xb1001, 0xb1801, 0xc2001,
            0xc3001, 0xd4001, 0xd6001,
        ];

        const FIXED_LIT_CODE_TAB: [u32; 512] = [
            0x70100, 0x80050, 0x80010, 0x80118, 0x70110, 0x80070, 0x80030, 0x900c0, 0x70108,
            0x80060, 0x80020, 0x900a0, 0x80000, 0x80080, 0x80040, 0x900e0, 0x70104, 0x80058,
            0x80018, 0x90090, 0x70114, 0x80078, 0x80038, 0x900d0, 0x7010c, 0x80068, 0x80028,
            0x900b0, 0x80008, 0x80088, 0x80048, 0x900f0, 0x70102, 0x80054, 0x80014, 0x8011c,
            0x70112, 0x80074, 0x80034, 0x900c8, 0x7010a, 0x80064, 0x80024, 0x900a8, 0x80004,
            0x80084, 0x80044, 0x900e8, 0x70106, 0x8005c, 0x8001c, 0x90098, 0x70116, 0x8007c,
            0x8003c, 0x900d8, 0x7010e, 0x8006c, 0x8002c, 0x900b8, 0x8000c, 0x8008c, 0x8004c,
            0x900f8, 0x70101, 0x80052, 0x80012, 0x8011a, 0x70111, 0x80072, 0x80032, 0x900c4,
            0x70109, 0x80062, 0x80022, 0x900a4, 0x80002, 0x80082, 0x80042, 0x900e4, 0x70105,
            0x8005a, 0x8001a, 0x90094, 0x70115, 0x8007a, 0x8003a, 0x900d4, 0x7010d, 0x8006a,
            0x8002a, 0x900b4, 0x8000a, 0x8008a, 0x8004a, 0x900f4, 0x70103, 0x80056, 0x80016,
            0x8011e, 0x70113, 0x80076, 0x80036, 0x900cc, 0x7010b, 0x80066, 0x80026, 0x900ac,
            0x80006, 0x80086, 0x80046, 0x900ec, 0x70107, 0x8005e, 0x8001e, 0x9009c, 0x70117,
            0x8007e, 0x8003e, 0x900dc, 0x7010f, 0x8006e, 0x8002e, 0x900bc, 0x8000e, 0x8008e,
            0x8004e, 0x900fc, 0x70100, 0x80051, 0x80011, 0x80119, 0x70110, 0x80071, 0x80031,
            0x900c2, 0x70108, 0x80061, 0x80021, 0x900a2, 0x80001, 0x80081, 0x80041, 0x900e2,
            0x70104, 0x80059, 0x80019, 0x90092, 0x70114, 0x80079, 0x80039, 0x900d2, 0x7010c,
            0x80069, 0x80029, 0x900b2, 0x80009, 0x80089, 0x80049, 0x900f2, 0x70102, 0x80055,
            0x80015, 0x8011d, 0x70112, 0x80075, 0x80035, 0x900ca, 0x7010a, 0x80065, 0x80025,
            0x900aa, 0x80005, 0x80085, 0x80045, 0x900ea, 0x70106, 0x8005d, 0x8001d, 0x9009a,
            0x70116, 0x8007d, 0x8003d, 0x900da, 0x7010e, 0x8006d, 0x8002d, 0x900ba, 0x8000d,
            0x8008d, 0x8004d, 0x900fa, 0x70101, 0x80053, 0x80013, 0x8011b, 0x70111, 0x80073,
            0x80033, 0x900c6, 0x70109, 0x80063, 0x80023, 0x900a6, 0x80003, 0x80083, 0x80043,
            0x900e6, 0x70105, 0x8005b, 0x8001b, 0x90096, 0x70115, 0x8007b, 0x8003b, 0x900d6,
            0x7010d, 0x8006b, 0x8002b, 0x900b6, 0x8000b, 0x8008b, 0x8004b, 0x900f6, 0x70103,
            0x80057, 0x80017, 0x8011f, 0x70113, 0x80077, 0x80037, 0x900ce, 0x7010b, 0x80067,
            0x80027, 0x900ae, 0x80007, 0x80087, 0x80047, 0x900ee, 0x70107, 0x8005f, 0x8001f,
            0x9009e, 0x70117, 0x8007f, 0x8003f, 0x900de, 0x7010f, 0x8006f, 0x8002f, 0x900be,
            0x8000f, 0x8008f, 0x8004f, 0x900fe, 0x70100, 0x80050, 0x80010, 0x80118, 0x70110,
            0x80070, 0x80030, 0x900c1, 0x70108, 0x80060, 0x80020, 0x900a1, 0x80000, 0x80080,
            0x80040, 0x900e1, 0x70104, 0x80058, 0x80018, 0x90091, 0x70114, 0x80078, 0x80038,
            0x900d1, 0x7010c, 0x80068, 0x80028, 0x900b1, 0x80008, 0x80088, 0x80048, 0x900f1,
            0x70102, 0x80054, 0x80014, 0x8011c, 0x70112, 0x80074, 0x80034, 0x900c9, 0x7010a,
            0x80064, 0x80024, 0x900a9, 0x80004, 0x80084, 0x80044, 0x900e9, 0x70106, 0x8005c,
            0x8001c, 0x90099, 0x70116, 0x8007c, 0x8003c, 0x900d9, 0x7010e, 0x8006c, 0x8002c,
            0x900b9, 0x8000c, 0x8008c, 0x8004c, 0x900f9, 0x70101, 0x80052, 0x80012, 0x8011a,
            0x70111, 0x80072, 0x80032, 0x900c5, 0x70109, 0x80062, 0x80022, 0x900a5, 0x80002,
            0x80082, 0x80042, 0x900e5, 0x70105, 0x8005a, 0x8001a, 0x90095, 0x70115, 0x8007a,
            0x8003a, 0x900d5, 0x7010d, 0x8006a, 0x8002a, 0x900b5, 0x8000a, 0x8008a, 0x8004a,
            0x900f5, 0x70103, 0x80056, 0x80016, 0x8011e, 0x70113, 0x80076, 0x80036, 0x900cd,
            0x7010b, 0x80066, 0x80026, 0x900ad, 0x80006, 0x80086, 0x80046, 0x900ed, 0x70107,
            0x8005e, 0x8001e, 0x9009d, 0x70117, 0x8007e, 0x8003e, 0x900dd, 0x7010f, 0x8006e,
            0x8002e, 0x900bd, 0x8000e, 0x8008e, 0x8004e, 0x900fd, 0x70100, 0x80051, 0x80011,
            0x80119, 0x70110, 0x80071, 0x80031, 0x900c3, 0x70108, 0x80061, 0x80021, 0x900a3,
            0x80001, 0x80081, 0x80041, 0x900e3, 0x70104, 0x80059, 0x80019, 0x90093, 0x70114,
            0x80079, 0x80039, 0x900d3, 0x7010c, 0x80069, 0x80029, 0x900b3, 0x80009, 0x80089,
            0x80049, 0x900f3, 0x70102, 0x80055, 0x80015, 0x8011d, 0x70112, 0x80075, 0x80035,
            0x900cb, 0x7010a, 0x80065, 0x80025, 0x900ab, 0x80005, 0x80085, 0x80045, 0x900eb,
            0x70106, 0x8005d, 0x8001d, 0x9009b, 0x70116, 0x8007d, 0x8003d, 0x900db, 0x7010e,
            0x8006d, 0x8002d, 0x900bb, 0x8000d, 0x8008d, 0x8004d, 0x900fb, 0x70101, 0x80053,
            0x80013, 0x8011b, 0x70111, 0x80073, 0x80033, 0x900c7, 0x70109, 0x80063, 0x80023,
            0x900a7, 0x80003, 0x80083, 0x80043, 0x900e7, 0x70105, 0x8005b, 0x8001b, 0x90097,
            0x70115, 0x8007b, 0x8003b, 0x900d7, 0x7010d, 0x8006b, 0x8002b, 0x900b7, 0x8000b,
            0x8008b, 0x8004b, 0x900f7, 0x70103, 0x80057, 0x80017, 0x8011f, 0x70113, 0x80077,
            0x80037, 0x900cf, 0x7010b, 0x80067, 0x80027, 0x900af, 0x80007, 0x80087, 0x80047,
            0x900ef, 0x70107, 0x8005f, 0x8001f, 0x9009f, 0x70117, 0x8007f, 0x8003f, 0x900df,
            0x7010f, 0x8006f, 0x8002f, 0x900bf, 0x8000f, 0x8008f, 0x8004f, 0x900ff,
        ];

        const FIXED_DIST_CODE_TAB: [u32; 32] = [
            0x50000, 0x50010, 0x50008, 0x50018, 0x50004, 0x50014, 0x5000c, 0x5001c, 0x50002,
            0x50012, 0x5000a, 0x5001a, 0x50006, 0x50016, 0x5000e, 0x00000, 0x50001, 0x50011,
            0x50009, 0x50019, 0x50005, 0x50015, 0x5000d, 0x5001d, 0x50003, 0x50013, 0x5000b,
            0x5001b, 0x50007, 0x50017, 0x5000f, 0x00000,
        ];
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
    fn decode_flate() {
        let input = [0xf3, 0x48, 0xcd, 0xc9, 0xc9, 0x7, 0x0];

        let decoded = flate::decode(&input, &Dict::default()).unwrap();
        assert_eq!(decoded, b"Hello");
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
