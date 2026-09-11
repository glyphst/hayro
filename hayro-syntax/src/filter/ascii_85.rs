// Keep group arithmetic and z placement in sync with PostScript strings.
// Unlike their already-delimited bodies, PDF streams require the ~> marker.

use crate::reader::Reader;
use crate::trivia::is_white_space_character;
use alloc::vec::Vec;

pub(crate) fn decode(data: &[u8]) -> Option<Vec<u8>> {
    const POW_85: [u32; 5] = [52200625, 614125, 7225, 85, 1];

    let mut reader = Reader::new(data);

    let mut read_byte = |skip_whitespace: bool| -> Option<u8> {
        loop {
            let b = reader.read_byte()?;

            // White space characters should be ignored.
            if !skip_whitespace || !is_white_space_character(b) {
                return Some(b);
            }
        }
    };

    let flush_group = |group: &mut Vec<u8>, decoded: &mut Vec<u8>| -> Option<()> {
        let (digits, output_len): ([u32; 5], usize) = match group.len() {
            0 => return Some(()),
            1 => return None, // A single character is not valid.
            2 => (
                [group[0], group[1], b'u', b'u', b'u'].map(|b| (b - b'!') as u32),
                1,
            ),
            3 => (
                [group[0], group[1], group[2], b'u', b'u'].map(|b| (b - b'!') as u32),
                2,
            ),
            4 => (
                [group[0], group[1], group[2], group[3], b'u'].map(|b| (b - b'!') as u32),
                3,
            ),
            5 => (
                [group[0], group[1], group[2], group[3], group[4]].map(|b| (b - b'!') as u32),
                4,
            ),
            _ => unreachable!(),
        };

        let value = digits[0]
            .checked_mul(POW_85[0])?
            .checked_add(digits[1].checked_mul(POW_85[1])?)?
            .checked_add(digits[2].checked_mul(POW_85[2])?)?
            .checked_add(digits[3].checked_mul(POW_85[3])?)?
            .checked_add(digits[4])?;

        decoded.extend_from_slice(&value.to_be_bytes()[..output_len]);
        group.clear();
        Some(())
    };

    let mut decoded = Vec::with_capacity(data.len() / 5 * 4);
    let mut group = Vec::with_capacity(5);

    loop {
        // EOF without an EOD marker is a decoding failure, not a usable prefix.
        let b = read_byte(true)?;

        match b {
            b'!'..=b'u' => {
                group.push(b);

                if group.len() == 5 {
                    flush_group(&mut group, &mut decoded)?;
                }
            }
            b'z' => {
                if !group.is_empty() {
                    return None;
                }
                decoded.extend_from_slice(&[0, 0, 0, 0]);
            }
            b'~' => {
                // The two-byte EOD marker is contiguous (PDF 7.4.3).
                if read_byte(false)? != b'>' {
                    return None;
                }
                flush_group(&mut group, &mut decoded)?;

                return Some(decoded);
            }
            _ => return None, // Invalid character.
        }
    }
}
