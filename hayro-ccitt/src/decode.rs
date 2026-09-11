use crate::bit_reader::BitReader;
use crate::state_machine::{BLACK_STATES, INVALID, State, TERMINAL, VALUE_MASK, WHITE_STATES};
use crate::{Color, DecodeError, Result};

/// End-of-facsimile-block marker (T.6 Section 2.4.1.1).
/// Two consecutive EOL codes: 000000000001 000000000001.
pub(crate) const EOFB: u32 = 0x1001;

/// 2D coding modes (T.4 Section 4.2.1.3.2, T.6 Section 2.2.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Mode {
    /// Pass mode (T.4 Section 4.2.1.3.2a, T.6 Section 2.2.3.1).
    Pass,
    /// Horizontal mode (T.4 Section 4.2.1.3.2c, T.6 Section 2.2.3.3).
    Horizontal,
    /// Vertical mode with offset (T.4 Section 4.2.1.3.2b, T.6 Section 2.2.3.2).
    Vertical(i8),
    /// Uncompressed extension (T.4 Table 5, T.6 Table 4).
    Uncompressed,
}

impl BitReader<'_> {
    /// Decode a run length using the given state machine (T.4 Section 4.1.1, T.6 Section 2.2.4).
    ///
    /// Run lengths 0-63 use terminating codes.
    /// Run lengths 64+ use one or more make-up codes followed by a terminating code.
    #[inline(always)]
    fn decode_run_inner(&mut self, states: &[State]) -> Result<u32> {
        let mut total: u32 = 0;
        let mut state: usize = 0;

        loop {
            let bit = self.read_bit()?;

            let transition = if bit == 0 {
                states[state].on_0
            } else {
                states[state].on_1
            };

            if transition == INVALID {
                return Err(DecodeError::InvalidCode);
            } else if transition & TERMINAL != 0 {
                let len = (transition & VALUE_MASK) as u32;
                total = total.checked_add(len).ok_or(DecodeError::Overflow)?;

                // For decoding black/white runs, less than 64 means we have
                // a terminating code.
                if len < 64 {
                    return Ok(total);
                }

                state = 0;
            } else {
                state = transition as usize;
            }
        }
    }

    /// Decode a run length for the specified color.
    #[inline(always)]
    pub(crate) fn decode_run(&mut self, color: Color) -> Result<u32> {
        match color {
            Color::White => self.decode_run_inner(&WHITE_STATES),
            Color::Black => self.decode_run_inner(&BLACK_STATES),
        }
    }

    /// Decode a 2D mode code.
    #[inline(always)]
    pub(crate) fn decode_mode(&mut self) -> Result<Mode> {
        if self.read_bit()? == 1 {
            return Ok(Mode::Vertical(0));
        }

        match self.read_bits(2)? {
            0b01 => return Ok(Mode::Horizontal),
            0b11 => return Ok(Mode::Vertical(1)),
            0b10 => return Ok(Mode::Vertical(-1)),
            0b00 => {}
            _ => unreachable!(),
        }

        if self.read_bit()? == 1 {
            return Ok(Mode::Pass);
        }

        if self.read_bit()? == 1 {
            return Ok(if self.read_bit()? == 1 {
                Mode::Vertical(2)
            } else {
                Mode::Vertical(-2)
            });
        }

        if self.read_bit()? == 0 {
            return if self.read_bits(4)? == 0b1111 {
                Ok(Mode::Uncompressed)
            } else {
                Err(DecodeError::InvalidCode)
            };
        }

        Ok(if self.read_bit()? == 1 {
            Mode::Vertical(3)
        } else {
            Mode::Vertical(-3)
        })
    }

    /// Consume one EOL, including arbitrarily long zero fill, in linear time.
    pub(crate) fn read_eol(&mut self) -> bool {
        let mut trial = self.clone();
        let mut zeros = 0;
        while trial.read_bit() == Ok(0) {
            zeros += 1;
        }
        // The failed read must have been a one, not the end of the stream.
        if zeros >= 11 && trial.bit_offset() > self.bit_offset() + zeros {
            *self = trial;
            true
        } else {
            false
        }
    }
}
