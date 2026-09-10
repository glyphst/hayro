//! Huffman table decoding, described in Annex B.

use alloc::rc::Rc;
use alloc::vec;
use alloc::vec::Vec;
use core::num::NonZeroU32;

use crate::error::{FormatError, HuffmanError, ParseError, Result, bail};
use crate::lazy::Lazy;
use crate::reader::Reader;

include!("huffman_tables_generated.rs");

#[cfg(test)]
#[path = "huffman_integer_table_tests.rs"]
mod integer_tests;

#[cfg(test)]
#[path = "huffman_range_table_tests.rs"]
mod range_tests;

#[cfg(test)]
#[path = "huffman_prefix_table_tests.rs"]
mod prefix_tests;

/// Maximum number of nodes in an inline Huffman table.
const INLINE_TABLE_SIZE: usize = 43;

/// A queryable Huffman table.
#[derive(Debug, Clone)]
pub(crate) struct HuffmanTable(Rc<InnerHuffmanTable>);

impl HuffmanTable {
    /// Create a new inline Huffman table from a fixed-size node array.
    fn from_inline(nodes: [HuffmanNode; INLINE_TABLE_SIZE]) -> Self {
        Self(Rc::new(InnerHuffmanTable::Inline { nodes }))
    }

    /// Create a new dynamic Huffman table from a vector of nodes.
    fn from_dynamic(nodes: Vec<HuffmanNode>) -> Self {
        Self(Rc::new(InnerHuffmanTable::Dynamic { nodes }))
    }

    /// Decode a value from the bit reader using this Huffman table
    /// (B.4 "Using a Huffman table").
    ///
    /// Returns `Ok(None)` for out-of-band (OOB) values, `Ok(Some(value))` for decoded values.
    pub(crate) fn decode(&self, reader: &mut Reader<'_>) -> Result<Option<i32>> {
        let nodes: &[HuffmanNode] = match self.0.as_ref() {
            InnerHuffmanTable::Inline { nodes } => nodes,
            InnerHuffmanTable::Dynamic { nodes } => nodes,
        };

        HuffmanNode::decode_from(nodes, 0, reader)
    }

    /// Whether any code produces an OOB marker (7.4.2.1.6, 7.4.3.1.6).
    pub(crate) fn has_out_of_band(&self) -> bool {
        let nodes: &[HuffmanNode] = match self.0.as_ref() {
            InnerHuffmanTable::Inline { nodes } => nodes,
            InnerHuffmanTable::Dynamic { nodes } => nodes,
        };
        nodes.iter().any(|node| {
            matches!(
                node,
                HuffmanNode::Leaf(LeafData {
                    is_out_of_band: true,
                    ..
                })
            )
        })
    }

    /// Decode a value using the huffman table, erroring out in case an OOB
    /// was encountered.
    pub(crate) fn decode_no_oob(&self, reader: &mut Reader<'_>) -> Result<i32> {
        Ok(self.decode(reader)?.ok_or(HuffmanError::UnexpectedOob)?)
    }

    /// Build a Huffman table from table line definitions (B.3 "Assigning
    /// the prefix codes").
    pub(crate) fn build(lines: &[TableLine]) -> Result<Self> {
        let max_prefix_length = lines.iter().map(|l| l.prefix_length).max().unwrap_or(0) as usize;
        let mut length_counts = vec![0_usize; max_prefix_length + 1];
        for line in lines {
            length_counts[line.prefix_length as usize] += 1;
        }
        // Reject overfilled spaces before allocating tree nodes. Once the
        // available slots exceed the number of table lines, no remaining
        // assignments can exhaust them; cap that count to avoid overflow at
        // deep levels. Prefix length zero never consumes a slot.
        let mut available = 1_usize;
        for &count in &length_counts[1..] {
            available = available.saturating_mul(2).min(lines.len());
            if count > available {
                bail!(HuffmanError::ConflictingCodes);
            }
            available -= count;
        }
        // B.2.1 permits 255-bit prefixes. Keep the next canonical code as
        // MSB-first bits instead of narrowing it to a machine integer. B.3
        // orders codes by length, then by their original table-line order.
        let mut code = [false; u8::MAX as usize];
        let mut exhausted = false;
        let mut nodes = vec![HuffmanNode::new_intermediate()];
        for current_length in 1..=max_prefix_length {
            // Increasing the length appends a zero bit, equivalent to B.3's
            // left shift. Prefix length zero is omitted from this traversal.
            for line in lines
                .iter()
                .filter(|line| line.prefix_length as usize == current_length)
            {
                if exhausted {
                    bail!(HuffmanError::ConflictingCodes);
                }
                Self::insert_code(&mut nodes, &code[..current_length], line)?;

                // Increment within the current width. Carry out means the
                // complete code space is used, including its final all-ones
                // code; any further line would conflict at this or a deeper
                // level. No extra bit or arbitrarily large integer is needed.
                exhausted = true;
                for bit in code[..current_length].iter_mut().rev() {
                    *bit = !*bit;
                    if *bit {
                        exhausted = false;
                        break;
                    }
                }
            }
        }

        Ok(Self::from_dynamic(nodes))
    }

    /// Build a uniform Huffman table where all symbols have the same code length.
    ///
    /// This is used for symbol ID encoding (6.5.8.2.3): "For each value of i from
    /// 0 to SDNUMINSYMS + NSYMSDECODED – 1, set SBSYMCODES[i] to the binary
    /// representation of i using a SBSYMCODELEN-bit string."
    pub(crate) fn build_uniform(num_symbols: u32, code_length: u32) -> Result<Self> {
        let code_length =
            u8::try_from(code_length).map_err(|_| HuffmanError::PrefixLengthTooLarge)?;
        let lines: Vec<TableLine> = (0..num_symbols)
            .map(|i| TableLine::new(i as i32, code_length, 0))
            .collect();
        Self::build(&lines)
    }

    /// Insert a code into the Huffman tree.
    fn insert_code(nodes: &mut Vec<HuffmanNode>, code: &[bool], line: &TableLine) -> Result<()> {
        let mut node_index = 0;
        // Walk the prefix iteratively so its length does not consume stack.
        for &bit in code {
            let child_index = match nodes[node_index].get_child(!bit) {
                Some(index) => index,
                None => {
                    let index = u32::try_from(nodes.len())
                        .ok()
                        .and_then(NonZeroU32::new)
                        .ok_or(HuffmanError::InvalidCode)?;
                    nodes.push(HuffmanNode::new_intermediate());
                    nodes[node_index].set_child(!bit, index)?;
                    index
                }
            };
            node_index = child_index.get() as usize;
        }
        if !matches!(
            nodes[node_index],
            HuffmanNode::Intermediate {
                zero: None,
                one: None
            }
        ) {
            bail!(HuffmanError::ConflictingCodes);
        }
        nodes[node_index] = HuffmanNode::new_leaf(
            line.range_low,
            line.range_length,
            line.is_lower,
            line.is_out_of_band,
        );
        Ok(())
    }

    /// Read a custom Huffman table from the bitstream (B.2 "Decoding a code table").
    pub(crate) fn read_custom(reader: &mut Reader<'_>) -> Result<Self> {
        // 1) "Decode the code table flags field as described in B.2.1. This sets the values
        //    HTOOB, HTPS and HTRS."
        let flags = reader.read_byte().ok_or(ParseError::UnexpectedEof)?;
        if flags & 0x80 != 0 {
            bail!(FormatError::ReservedBits);
        }

        // `HTOOB`
        let has_out_of_band = (flags & 1) != 0;
        // `HTPS`
        let prefix_length_bits = ((flags >> 1) & 7) + 1;
        // `HTRS`
        let range_length_bits = ((flags >> 4) & 7) + 1;

        // 2) "Decode the code table lowest value field as described in B.2.2. Let HTLOW be
        //    the value decoded."
        // `HTLOW`
        let minimum_value = reader.read_i32().ok_or(ParseError::UnexpectedEof)?;

        // 3) "Decode the code table highest value field as described in B.2.3. Let HTHIGH be
        //    the value decoded."
        // `HTHIGH`
        let maximum_value = reader.read_i32().ok_or(ParseError::UnexpectedEof)?;
        if minimum_value >= maximum_value {
            bail!(HuffmanError::InvalidCode);
        }

        // 4) "Set: CURRANGELOW = HTLOW, NTEMP = 0"
        let mut lines = Vec::new();
        // `CURRANGELOW`
        let mut current_range_low = minimum_value;

        // 5) "Decode each table line as follows:"
        //    d) "If CURRANGELOW ≥ HTHIGH then proceed to step 6."
        while current_range_low < maximum_value {
            // a) "Read HTPS bits. Set PREFLEN[NTEMP] to the value decoded."
            let prefix_length = reader
                .read_bits(prefix_length_bits)
                .ok_or(HuffmanError::InvalidCode)? as u8;
            // b) "Read HTRS bits. Let RANGELEN[NTEMP] be the value decoded."
            let range_length = reader
                .read_bits(range_length_bits)
                .ok_or(HuffmanError::InvalidCode)? as u8;

            // c) "Set: RANGELOW[NTEMP] = CURRANGELOW
            //         CURRANGELOW = CURRANGELOW + 2^RANGELEN[NTEMP]
            //         NTEMP = NTEMP + 1"
            lines.push(TableLine::new(
                current_range_low,
                prefix_length,
                range_length,
            ));

            // Any range of at least 2^32 spans the remaining signed domain.
            // B.2 ends the normal lines on reaching *or passing* HTHIGH;
            // the terminal boundary need not itself fit in an i32.
            if range_length >= 32 {
                break;
            }
            let next_range_low = i64::from(current_range_low) + (1_i64 << range_length);
            if next_range_low >= i64::from(maximum_value) {
                break;
            }
            current_range_low =
                i32::try_from(next_range_low).map_err(|_| HuffmanError::InvalidCode)?;
        }

        // 6) "Read HTPS bits. Let LOWPREFLEN be the value read."
        // 7) "Set: PREFLEN[NTEMP] = LOWPREFLEN, RANGELEN[NTEMP] = 32,
        //         RANGELOW[NTEMP] = HTLOW − 1, NTEMP = NTEMP + 1
        //    This is the lower range table line for this table."
        lines.push(TableLine::lower(
            i64::from(minimum_value) - 1,
            reader
                .read_bits(prefix_length_bits)
                .ok_or(HuffmanError::InvalidCode)? as u8,
            32,
        ));

        // 8) "Read HTPS bits. Let HIGHPREFLEN be the value read."
        // 9) "Set: PREFLEN[NTEMP] = HIGHPREFLEN, RANGELEN[NTEMP] = 32,
        //         RANGELOW[NTEMP] = HTHIGH, NTEMP = NTEMP + 1
        //    This is the upper range table line for this table."
        lines.push(TableLine::upper(
            maximum_value,
            reader
                .read_bits(prefix_length_bits)
                .ok_or(HuffmanError::InvalidCode)? as u8,
            32,
        ));

        // 10) "If HTOOB is 1, then:
        //     a) Read HTPS bits. Let OOBPREFLEN be the value read.
        //     b) Set: PREFLEN[NTEMP] = OOBPREFLEN, NTEMP = NTEMP + 1
        //     This is the out-of-band table line for this table."
        if has_out_of_band {
            lines.push(TableLine::oob(
                reader
                    .read_bits(prefix_length_bits)
                    .ok_or(HuffmanError::InvalidCode)? as u8,
            ));
        }

        // 11) "Create the prefix codes using the algorithm described in B.3."
        Self::build(&lines)
    }
}

/// A table line definition used to build the Huffman tree.
pub(crate) struct TableLine {
    /// `RANGELOW` - The base value for computing the decoded value.
    /// For normal/upper lines: value = `range_low` + offset
    /// For lower lines: value = `range_low` - offset
    // The lower open range may start at i32::MIN - 1. Keep that table
    // definition without excluding the table's valid normal/upper codes.
    pub(crate) range_low: i64,
    /// `PREFLEN` - Prefix code length.
    pub(crate) prefix_length: u8,
    /// `RANGELEN` - Number of additional bits.
    pub(crate) range_length: u8,
    /// True if this is a lower range line (uses subtraction).
    pub(crate) is_lower: bool,
    /// `OOB` - True if this is the out-of-band marker.
    pub(crate) is_out_of_band: bool,
}

impl TableLine {
    /// Create a normal table line.
    pub(crate) const fn new(range_low: i32, prefix_length: u8, range_length: u8) -> Self {
        Self {
            range_low: range_low as i64,
            prefix_length,
            range_length,
            is_lower: false,
            is_out_of_band: false,
        }
    }

    /// Create a lower range line (-∞...`range_high`).
    const fn lower(range_high: i64, prefix_length: u8, range_length: u8) -> Self {
        Self {
            range_low: range_high,
            prefix_length,
            range_length,
            is_lower: true,
            is_out_of_band: false,
        }
    }

    /// Create an upper range line (`range_low`...+∞).
    const fn upper(range_low: i32, prefix_length: u8, range_length: u8) -> Self {
        Self {
            range_low: range_low as i64,
            prefix_length,
            range_length,
            is_lower: false,
            is_out_of_band: false,
        }
    }

    /// Create an out-of-band marker line.
    const fn oob(prefix_length: u8) -> Self {
        Self {
            range_low: 0,
            prefix_length,
            range_length: 0,
            is_lower: false,
            is_out_of_band: true,
        }
    }
}

/// A node in the Huffman tree.
#[derive(Debug, Clone, Copy)]
enum HuffmanNode {
    /// Intermediate node.
    Intermediate {
        zero: Option<NonZeroU32>,
        one: Option<NonZeroU32>,
    },
    /// Leaf node.
    Leaf(LeafData),
    /// The lower range starts below `i32::MIN` and has no encodable value.
    OutOfRange,
    /// Empty node (padding to fill fixed-size arrays in inline tables).
    Empty,
}

impl HuffmanNode {
    fn new_intermediate() -> Self {
        Self::Intermediate {
            zero: None,
            one: None,
        }
    }

    fn new_leaf(range_low: i64, range_length: u8, is_lower: bool, is_out_of_band: bool) -> Self {
        // Only the HTLOW - 1 lower base can lie outside i32. Subtracting
        // any unsigned offset from it stays outside the encoded domain.
        let Ok(range_low) = i32::try_from(range_low) else {
            return Self::OutOfRange;
        };
        Self::Leaf(LeafData {
            range_low,
            range_length,
            is_lower,
            is_out_of_band,
        })
    }

    /// Get the child index for a given bit (0 or 1).
    fn get_child(&self, child_zero: bool) -> Option<NonZeroU32> {
        match self {
            Self::Intermediate { zero, one } => {
                if child_zero {
                    *zero
                } else {
                    *one
                }
            }
            _ => None,
        }
    }

    /// Set the child index for a given bit (0 or 1).
    fn set_child(
        &mut self,
        child_zero: bool,
        index: NonZeroU32,
    ) -> core::result::Result<(), HuffmanError> {
        match self {
            Self::Intermediate { zero, one } => {
                if child_zero {
                    *zero = Some(index);
                } else {
                    *one = Some(index);
                }
                Ok(())
            }
            _ => Err(HuffmanError::ConflictingCodes),
        }
    }

    /// Implements B.4 "Using a Huffman table".
    fn decode_from(
        nodes: &[Self],
        mut node_index: u32,
        reader: &mut Reader<'_>,
    ) -> Result<Option<i32>> {
        // 1) "Read one bit at a time until the bit string read matches the code assigned to
        //    one of the table lines."
        loop {
            match nodes[node_index as usize] {
                Self::Intermediate { zero, one } => {
                    let bit = reader.read_bit().ok_or(ParseError::UnexpectedEof)?;
                    let child_index = if bit == 0 { zero } else { one };
                    node_index = child_index.ok_or(HuffmanError::InvalidCode)?.get();
                }
                Self::Leaf(leaf) => {
                    // 3) "If HTOOB is 1 for this table, and table line I is the out-of-band
                    //    table line for this table, then set: HTVAL = OOB"
                    if leaf.is_out_of_band {
                        return Ok(None);
                    }

                    // 2) "Read RANGELEN[I] bits. Let HTOFFSET be the value read."
                    // `HTOFFSET`
                    // The field can declare up to 255 suffix bits. A nonzero
                    // bit above the low 32 cannot yield a signed-domain value
                    // with any representable base. Consume zero extension in
                    // bounded chunks without constructing a larger integer.
                    let mut excess = leaf.range_length.saturating_sub(32);
                    while excess > 0 {
                        let count = excess.min(32);
                        if reader.read_bits(count).ok_or(HuffmanError::InvalidCode)? != 0 {
                            bail!(HuffmanError::InvalidCode);
                        }
                        excess -= count;
                    }
                    let range_offset = reader
                        .read_bits(leaf.range_length.min(32))
                        .ok_or(HuffmanError::InvalidCode)?;

                    // 4) "Otherwise, if table line I is the lower range table line for this
                    //    table, then set: HTVAL = RANGELOW[I] − HTOFFSET"
                    // 5) "Otherwise, set: HTVAL = RANGELOW[I] + HTOFFSET"
                    // `HTVAL`
                    let value = if leaf.is_lower {
                        i64::from(leaf.range_low) - i64::from(range_offset)
                    } else {
                        i64::from(leaf.range_low) + i64::from(range_offset)
                    };
                    let value = i32::try_from(value).map_err(|_| HuffmanError::InvalidCode)?;
                    return Ok(Some(value));
                }
                Self::Empty | Self::OutOfRange => {
                    bail!(HuffmanError::InvalidCode);
                }
            }
        }
    }
}

/// Information stored at a leaf node of the Huffman tree.
#[derive(Debug, Clone, Copy)]
struct LeafData {
    /// `RANGELOW` - The base value for computing the decoded value.
    range_low: i32,
    /// `RANGELEN` - Number of additional bits to read.
    range_length: u8,
    /// True if this is a lower range line (uses subtraction).
    is_lower: bool,
    /// `OOB` - True if this is the out-of-band marker.
    is_out_of_band: bool,
}

/// The inner representation of a Huffman table.
///
/// This can be either an inline table (fixed-size array for standard tables)
/// or a dynamic table (Vec for runtime-built custom tables).
#[derive(Debug, Clone)]
#[allow(
    clippy::large_enum_variant,
    reason = "Inline variant is expected to be large."
)]
enum InnerHuffmanTable {
    Inline {
        nodes: [HuffmanNode; INLINE_TABLE_SIZE],
    },
    Dynamic {
        nodes: Vec<HuffmanNode>,
    },
}

/// Standard Huffman tables (`TABLE_A` through `TABLE_O`).
#[derive(Debug)]
pub(crate) struct StandardHuffmanTables {
    table_a: Lazy<HuffmanTable>,
    table_b: Lazy<HuffmanTable>,
    table_c: Lazy<HuffmanTable>,
    table_d: Lazy<HuffmanTable>,
    table_e: Lazy<HuffmanTable>,
    table_f: Lazy<HuffmanTable>,
    table_g: Lazy<HuffmanTable>,
    table_h: Lazy<HuffmanTable>,
    table_i: Lazy<HuffmanTable>,
    table_j: Lazy<HuffmanTable>,
    table_k: Lazy<HuffmanTable>,
    table_l: Lazy<HuffmanTable>,
    table_m: Lazy<HuffmanTable>,
    table_n: Lazy<HuffmanTable>,
    table_o: Lazy<HuffmanTable>,
}

impl Default for StandardHuffmanTables {
    fn default() -> Self {
        Self::new()
    }
}

impl StandardHuffmanTables {
    /// Create a new instance.
    pub(crate) fn new() -> Self {
        Self {
            table_a: Lazy::new(|| HuffmanTable::from_inline(TABLE_A)),
            table_b: Lazy::new(|| HuffmanTable::from_inline(TABLE_B)),
            table_c: Lazy::new(|| HuffmanTable::from_inline(TABLE_C)),
            table_d: Lazy::new(|| HuffmanTable::from_inline(TABLE_D)),
            table_e: Lazy::new(|| HuffmanTable::from_inline(TABLE_E)),
            table_f: Lazy::new(|| HuffmanTable::from_inline(TABLE_F)),
            table_g: Lazy::new(|| HuffmanTable::from_inline(TABLE_G)),
            table_h: Lazy::new(|| HuffmanTable::from_inline(TABLE_H)),
            table_i: Lazy::new(|| HuffmanTable::from_inline(TABLE_I)),
            table_j: Lazy::new(|| HuffmanTable::from_inline(TABLE_J)),
            table_k: Lazy::new(|| HuffmanTable::from_inline(TABLE_K)),
            table_l: Lazy::new(|| HuffmanTable::from_inline(TABLE_L)),
            table_m: Lazy::new(|| HuffmanTable::from_inline(TABLE_M)),
            table_n: Lazy::new(|| HuffmanTable::from_inline(TABLE_N)),
            table_o: Lazy::new(|| HuffmanTable::from_inline(TABLE_O)),
        }
    }

    /// Get Table B.1 (`TABLE_A`).
    pub(crate) fn table_a(&self) -> &HuffmanTable {
        self.table_a.get(|| HuffmanTable::from_inline(TABLE_A))
    }

    /// Get Table B.2 (`TABLE_B`).
    pub(crate) fn table_b(&self) -> &HuffmanTable {
        self.table_b.get(|| HuffmanTable::from_inline(TABLE_B))
    }

    /// Get Table B.3 (`TABLE_C`).
    pub(crate) fn table_c(&self) -> &HuffmanTable {
        self.table_c.get(|| HuffmanTable::from_inline(TABLE_C))
    }

    /// Get Table B.4 (`TABLE_D`).
    pub(crate) fn table_d(&self) -> &HuffmanTable {
        self.table_d.get(|| HuffmanTable::from_inline(TABLE_D))
    }

    /// Get Table B.5 (`TABLE_E`).
    pub(crate) fn table_e(&self) -> &HuffmanTable {
        self.table_e.get(|| HuffmanTable::from_inline(TABLE_E))
    }

    /// Get Table B.6 (`TABLE_F`).
    pub(crate) fn table_f(&self) -> &HuffmanTable {
        self.table_f.get(|| HuffmanTable::from_inline(TABLE_F))
    }

    /// Get Table B.7 (`TABLE_G`).
    pub(crate) fn table_g(&self) -> &HuffmanTable {
        self.table_g.get(|| HuffmanTable::from_inline(TABLE_G))
    }

    /// Get Table B.8 (`TABLE_H`).
    pub(crate) fn table_h(&self) -> &HuffmanTable {
        self.table_h.get(|| HuffmanTable::from_inline(TABLE_H))
    }

    /// Get Table B.9 (`TABLE_I`).
    pub(crate) fn table_i(&self) -> &HuffmanTable {
        self.table_i.get(|| HuffmanTable::from_inline(TABLE_I))
    }

    /// Get Table B.10 (`TABLE_J`).
    pub(crate) fn table_j(&self) -> &HuffmanTable {
        self.table_j.get(|| HuffmanTable::from_inline(TABLE_J))
    }

    /// Get Table B.11 (`TABLE_K`).
    pub(crate) fn table_k(&self) -> &HuffmanTable {
        self.table_k.get(|| HuffmanTable::from_inline(TABLE_K))
    }

    /// Get Table B.12 (`TABLE_L`).
    pub(crate) fn table_l(&self) -> &HuffmanTable {
        self.table_l.get(|| HuffmanTable::from_inline(TABLE_L))
    }

    /// Get Table B.13 (`TABLE_M`).
    pub(crate) fn table_m(&self) -> &HuffmanTable {
        self.table_m.get(|| HuffmanTable::from_inline(TABLE_M))
    }

    /// Get Table B.14 (`TABLE_N`).
    pub(crate) fn table_n(&self) -> &HuffmanTable {
        self.table_n.get(|| HuffmanTable::from_inline(TABLE_N))
    }

    /// Get Table B.15 (`TABLE_O`).
    pub(crate) fn table_o(&self) -> &HuffmanTable {
        self.table_o.get(|| HuffmanTable::from_inline(TABLE_O))
    }
}
