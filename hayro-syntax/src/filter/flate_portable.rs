//! Portable DEFLATE decoder, originally ported from
//! <https://github.com/mozilla/pdf.js/blob/master/src/core/flate_stream.js>.

use super::lzw_flate::flate::{LimitedDecodeFailure, valid_suffix, zlib_window};
use alloc::vec;
use alloc::vec::Vec;

pub(crate) fn decode(
    data: &[u8],
    max_output_bytes: usize,
    inline_suffix: bool,
    require_wide_distances: bool,
) -> Result<Vec<u8>, LimitedDecodeFailure> {
    let window = zlib_window(data).ok_or(LimitedDecodeFailure::Decode)?;
    let mut stream = FlateStream::new(&data[2..], max_output_bytes, window);
    let output = stream.decode()?;
    // Huffman lookahead may include whole bytes from the Adler32 trailer.
    let end = stream.pos - usize::from(stream.code_size / 8);
    let trailer = stream
        .data
        .get(end..end + 4)
        .ok_or(LimitedDecodeFailure::Decode)?;
    if u32::from_be_bytes(
        trailer
            .try_into()
            .map_err(|_| LimitedDecodeFailure::Decode)?,
    ) != adler32(&output)
        || !valid_suffix(&stream.data[end + 4..], inline_suffix)
        || require_wide_distances && !stream.wide_distances
    {
        return Err(LimitedDecodeFailure::Decode);
    }
    Ok(output)
}

fn adler32(data: &[u8]) -> u32 {
    let mut a = 1_u32;
    let mut b = 0_u32;
    // 5552 is the largest chunk that cannot overflow u32 with byte inputs.
    for chunk in data.chunks(5552) {
        for &byte in chunk {
            a += u32::from(byte);
            b += a;
        }
        a %= 65521;
        b %= 65521;
    }
    (b << 16) | a
}

struct FlateStream<'a> {
    data: &'a [u8],
    pos: usize,
    code_buf: u32,
    code_size: u8,
    output: Vec<u8>,
    eof: bool,
    failed: bool,
    max_output_bytes: usize,
    limit_exceeded: bool,
    window: usize,
    wide_distances: bool,
}

impl<'a> FlateStream<'a> {
    fn new(data: &'a [u8], max_output_bytes: usize, window: usize) -> Self {
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
            window,
            wide_distances: false,
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
        if !self
            .output
            .len()
            .checked_add(additional)
            .is_some_and(|n| n <= self.max_output_bytes)
        {
            self.limit_exceeded = true;
            self.eof = true;
            return false;
        }
        if self.output.try_reserve(additional).is_err() {
            self.failed = true;
            self.eof = true;
            return false;
        }
        true
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
        // Discard only partial-byte padding. Put prefetched whole bytes back
        // before reading LEN/NLEN; a short EOD can leave up to two in lookahead.
        self.pos -= usize::from(self.code_size / 8);
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
            let Some(end) = self.pos.checked_add(usize::from(block_len)) else {
                self.failed = true;
                self.eof = true;
                return;
            };
            let Some(block) = self.data.get(self.pos..end) else {
                self.failed = true;
                self.eof = true;
                return;
            };
            self.output.extend_from_slice(block);
            self.pos = end;
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
                if distance == 0 || distance > self.output.len() || distance > self.window {
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
        if num_lit_codes > 286 {
            return None;
        }
        self.wide_distances |= num_dist_codes > 30;

        // Build code length code table
        let mut code_len_code_lengths = vec![0_u8; 19];
        for i in 0..num_code_len_codes {
            code_len_code_lengths[CODE_LEN_CODE_MAP[i] as usize] = self.get_bits(3)? as u8;
        }

        let code_len_table = generate_huffman_table(&code_len_code_lengths, false)?;

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
                16..=18 => {
                    let (value, repeat_count) = match code {
                        16 => (
                            *code_lengths.get(i.checked_sub(1)?)?,
                            self.get_bits(2)? as usize + 3,
                        ),
                        17 => (0, self.get_bits(3)? as usize + 3),
                        18 => (0, self.get_bits(7)? as usize + 11),
                        _ => return None,
                    };
                    let end = i.checked_add(repeat_count)?;
                    code_lengths.get_mut(i..end)?.fill(value);
                    i = end;
                }
                _ => return None,
            }
        }

        if code_lengths[256] == 0 {
            return None;
        }
        let lit_table = generate_huffman_table(&code_lengths[..num_lit_codes], true)?;
        let dist_table = generate_huffman_table(&code_lengths[num_lit_codes..], true)?;

        Some((lit_table, dist_table))
    }
}

struct HuffmanTable {
    codes: Vec<u32>,
    max_len: u8,
}

fn generate_huffman_table(lengths: &[u8], allow_single: bool) -> Option<HuffmanTable> {
    let max_len = lengths.iter().copied().max().unwrap_or(0);
    if max_len > 15 {
        return None;
    }
    if max_len == 0 {
        // A literals-only block can omit the distance tree. Any attempted
        // distance decode still fails because its table entry has zero length.
        return allow_single.then(|| HuffmanTable {
            codes: vec![0],
            max_len: 1,
        });
    }
    let mut counts = [0_u32; 16];
    for &length in lengths {
        counts[usize::from(length)] += 1;
    }
    let mut left = 1_u32;
    for &count in &counts[1..] {
        left = (left * 2).checked_sub(count)?;
    }
    if left != 0 && !(allow_single && max_len == 1) {
        return None;
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

    Some(HuffmanTable { codes, max_len })
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
    0x00003, 0x00004, 0x00005, 0x00006, 0x00007, 0x00008, 0x00009, 0x0000a, 0x1000b, 0x1000d,
    0x1000f, 0x10011, 0x20013, 0x20017, 0x2001b, 0x2001f, 0x30023, 0x3002b, 0x30033, 0x3003b,
    0x40043, 0x40053, 0x40063, 0x40073, 0x50083, 0x500a3, 0x500c3, 0x500e3, 0x00102,
];

const DIST_DECODE: [u32; 30] = [
    0x00001, 0x00002, 0x00003, 0x00004, 0x10005, 0x10007, 0x20009, 0x2000d, 0x30011, 0x30019,
    0x40021, 0x40031, 0x50041, 0x50061, 0x60081, 0x600c1, 0x70101, 0x70181, 0x80201, 0x80301,
    0x90401, 0x90601, 0xa0801, 0xa0c01, 0xb1001, 0xb1801, 0xc2001, 0xc3001, 0xd4001, 0xd6001,
];

const FIXED_LIT_CODE_TAB: [u32; 512] = [
    0x70100, 0x80050, 0x80010, 0x80118, 0x70110, 0x80070, 0x80030, 0x900c0, 0x70108, 0x80060,
    0x80020, 0x900a0, 0x80000, 0x80080, 0x80040, 0x900e0, 0x70104, 0x80058, 0x80018, 0x90090,
    0x70114, 0x80078, 0x80038, 0x900d0, 0x7010c, 0x80068, 0x80028, 0x900b0, 0x80008, 0x80088,
    0x80048, 0x900f0, 0x70102, 0x80054, 0x80014, 0x8011c, 0x70112, 0x80074, 0x80034, 0x900c8,
    0x7010a, 0x80064, 0x80024, 0x900a8, 0x80004, 0x80084, 0x80044, 0x900e8, 0x70106, 0x8005c,
    0x8001c, 0x90098, 0x70116, 0x8007c, 0x8003c, 0x900d8, 0x7010e, 0x8006c, 0x8002c, 0x900b8,
    0x8000c, 0x8008c, 0x8004c, 0x900f8, 0x70101, 0x80052, 0x80012, 0x8011a, 0x70111, 0x80072,
    0x80032, 0x900c4, 0x70109, 0x80062, 0x80022, 0x900a4, 0x80002, 0x80082, 0x80042, 0x900e4,
    0x70105, 0x8005a, 0x8001a, 0x90094, 0x70115, 0x8007a, 0x8003a, 0x900d4, 0x7010d, 0x8006a,
    0x8002a, 0x900b4, 0x8000a, 0x8008a, 0x8004a, 0x900f4, 0x70103, 0x80056, 0x80016, 0x8011e,
    0x70113, 0x80076, 0x80036, 0x900cc, 0x7010b, 0x80066, 0x80026, 0x900ac, 0x80006, 0x80086,
    0x80046, 0x900ec, 0x70107, 0x8005e, 0x8001e, 0x9009c, 0x70117, 0x8007e, 0x8003e, 0x900dc,
    0x7010f, 0x8006e, 0x8002e, 0x900bc, 0x8000e, 0x8008e, 0x8004e, 0x900fc, 0x70100, 0x80051,
    0x80011, 0x80119, 0x70110, 0x80071, 0x80031, 0x900c2, 0x70108, 0x80061, 0x80021, 0x900a2,
    0x80001, 0x80081, 0x80041, 0x900e2, 0x70104, 0x80059, 0x80019, 0x90092, 0x70114, 0x80079,
    0x80039, 0x900d2, 0x7010c, 0x80069, 0x80029, 0x900b2, 0x80009, 0x80089, 0x80049, 0x900f2,
    0x70102, 0x80055, 0x80015, 0x8011d, 0x70112, 0x80075, 0x80035, 0x900ca, 0x7010a, 0x80065,
    0x80025, 0x900aa, 0x80005, 0x80085, 0x80045, 0x900ea, 0x70106, 0x8005d, 0x8001d, 0x9009a,
    0x70116, 0x8007d, 0x8003d, 0x900da, 0x7010e, 0x8006d, 0x8002d, 0x900ba, 0x8000d, 0x8008d,
    0x8004d, 0x900fa, 0x70101, 0x80053, 0x80013, 0x8011b, 0x70111, 0x80073, 0x80033, 0x900c6,
    0x70109, 0x80063, 0x80023, 0x900a6, 0x80003, 0x80083, 0x80043, 0x900e6, 0x70105, 0x8005b,
    0x8001b, 0x90096, 0x70115, 0x8007b, 0x8003b, 0x900d6, 0x7010d, 0x8006b, 0x8002b, 0x900b6,
    0x8000b, 0x8008b, 0x8004b, 0x900f6, 0x70103, 0x80057, 0x80017, 0x8011f, 0x70113, 0x80077,
    0x80037, 0x900ce, 0x7010b, 0x80067, 0x80027, 0x900ae, 0x80007, 0x80087, 0x80047, 0x900ee,
    0x70107, 0x8005f, 0x8001f, 0x9009e, 0x70117, 0x8007f, 0x8003f, 0x900de, 0x7010f, 0x8006f,
    0x8002f, 0x900be, 0x8000f, 0x8008f, 0x8004f, 0x900fe, 0x70100, 0x80050, 0x80010, 0x80118,
    0x70110, 0x80070, 0x80030, 0x900c1, 0x70108, 0x80060, 0x80020, 0x900a1, 0x80000, 0x80080,
    0x80040, 0x900e1, 0x70104, 0x80058, 0x80018, 0x90091, 0x70114, 0x80078, 0x80038, 0x900d1,
    0x7010c, 0x80068, 0x80028, 0x900b1, 0x80008, 0x80088, 0x80048, 0x900f1, 0x70102, 0x80054,
    0x80014, 0x8011c, 0x70112, 0x80074, 0x80034, 0x900c9, 0x7010a, 0x80064, 0x80024, 0x900a9,
    0x80004, 0x80084, 0x80044, 0x900e9, 0x70106, 0x8005c, 0x8001c, 0x90099, 0x70116, 0x8007c,
    0x8003c, 0x900d9, 0x7010e, 0x8006c, 0x8002c, 0x900b9, 0x8000c, 0x8008c, 0x8004c, 0x900f9,
    0x70101, 0x80052, 0x80012, 0x8011a, 0x70111, 0x80072, 0x80032, 0x900c5, 0x70109, 0x80062,
    0x80022, 0x900a5, 0x80002, 0x80082, 0x80042, 0x900e5, 0x70105, 0x8005a, 0x8001a, 0x90095,
    0x70115, 0x8007a, 0x8003a, 0x900d5, 0x7010d, 0x8006a, 0x8002a, 0x900b5, 0x8000a, 0x8008a,
    0x8004a, 0x900f5, 0x70103, 0x80056, 0x80016, 0x8011e, 0x70113, 0x80076, 0x80036, 0x900cd,
    0x7010b, 0x80066, 0x80026, 0x900ad, 0x80006, 0x80086, 0x80046, 0x900ed, 0x70107, 0x8005e,
    0x8001e, 0x9009d, 0x70117, 0x8007e, 0x8003e, 0x900dd, 0x7010f, 0x8006e, 0x8002e, 0x900bd,
    0x8000e, 0x8008e, 0x8004e, 0x900fd, 0x70100, 0x80051, 0x80011, 0x80119, 0x70110, 0x80071,
    0x80031, 0x900c3, 0x70108, 0x80061, 0x80021, 0x900a3, 0x80001, 0x80081, 0x80041, 0x900e3,
    0x70104, 0x80059, 0x80019, 0x90093, 0x70114, 0x80079, 0x80039, 0x900d3, 0x7010c, 0x80069,
    0x80029, 0x900b3, 0x80009, 0x80089, 0x80049, 0x900f3, 0x70102, 0x80055, 0x80015, 0x8011d,
    0x70112, 0x80075, 0x80035, 0x900cb, 0x7010a, 0x80065, 0x80025, 0x900ab, 0x80005, 0x80085,
    0x80045, 0x900eb, 0x70106, 0x8005d, 0x8001d, 0x9009b, 0x70116, 0x8007d, 0x8003d, 0x900db,
    0x7010e, 0x8006d, 0x8002d, 0x900bb, 0x8000d, 0x8008d, 0x8004d, 0x900fb, 0x70101, 0x80053,
    0x80013, 0x8011b, 0x70111, 0x80073, 0x80033, 0x900c7, 0x70109, 0x80063, 0x80023, 0x900a7,
    0x80003, 0x80083, 0x80043, 0x900e7, 0x70105, 0x8005b, 0x8001b, 0x90097, 0x70115, 0x8007b,
    0x8003b, 0x900d7, 0x7010d, 0x8006b, 0x8002b, 0x900b7, 0x8000b, 0x8008b, 0x8004b, 0x900f7,
    0x70103, 0x80057, 0x80017, 0x8011f, 0x70113, 0x80077, 0x80037, 0x900cf, 0x7010b, 0x80067,
    0x80027, 0x900af, 0x80007, 0x80087, 0x80047, 0x900ef, 0x70107, 0x8005f, 0x8001f, 0x9009f,
    0x70117, 0x8007f, 0x8003f, 0x900df, 0x7010f, 0x8006f, 0x8002f, 0x900bf, 0x8000f, 0x8008f,
    0x8004f, 0x900ff,
];

const FIXED_DIST_CODE_TAB: [u32; 32] = [
    0x50000, 0x50010, 0x50008, 0x50018, 0x50004, 0x50014, 0x5000c, 0x5001c, 0x50002, 0x50012,
    0x5000a, 0x5001a, 0x50006, 0x50016, 0x5000e, 0x00000, 0x50001, 0x50011, 0x50009, 0x50019,
    0x50005, 0x50015, 0x5000d, 0x5001d, 0x50003, 0x50013, 0x5000b, 0x5001b, 0x50007, 0x50017,
    0x5000f, 0x00000,
];
