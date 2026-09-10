//! PDF 1.7 §7.4.7 and T.88 §§7.2.3–7.3.2, 7.4.8, D.3.
use crate::decode::{CombinationOperator, parse_region_segment_info};
use crate::error::{FormatError, Result, bail};
use crate::file::parse_segments_sequential;
use crate::page_info::parse_page_information;
use crate::reader::Reader;
use crate::segment::{Segment, SegmentType};
use alloc::vec;
use alloc::vec::Vec;

const DISCARDED: u8 = 1;
const CONSUMED: u8 = 2;

pub(crate) fn parse<'a>(data: &'a [u8], globals: Option<&'a [u8]>) -> Result<Vec<Segment<'a>>> {
    let mut segments = Vec::new();
    parse_segments_sequential(&mut Reader::new(globals.unwrap_or_default()), &mut segments)?;
    // Only these segment types can be independent of a page (T.88 §7.3.2).
    if segments.iter().any(|segment| {
        segment.header.page_association != 0
            || !matches!(
                segment.header.segment_type,
                SegmentType::SymbolDictionary
                    | SegmentType::PatternDictionary
                    | SegmentType::Profiles
                    | SegmentType::Tables
                    | SegmentType::Extension
            )
    }) {
        bail!(FormatError::InvalidPdfEmbedding);
    }
    let global_count = segments.len();
    parse_segments_sequential(&mut Reader::new(data), &mut segments)?;
    let page_segments = &segments[global_count..];
    let Some(first) = page_segments.first() else {
        bail!(FormatError::MissingPageInfo);
    };
    // PDF recommends normalization to one, but does not require it.
    let page = first.header.page_association;
    if page == 0
        || page_segments.iter().any(|segment| {
            segment.header.page_association != page
                || matches!(
                    segment.header.segment_type,
                    SegmentType::EndOfPage | SegmentType::EndOfFile
                )
        })
        || page_segments
            .iter()
            .filter(|segment| segment.header.segment_type == SegmentType::PageInformation)
            .count()
            != 1
        || page_segments
            .iter()
            .min_by_key(|segment| segment.header.segment_number)
            .is_none_or(|segment| segment.header.segment_type != SegmentType::PageInformation)
    {
        bail!(FormatError::InvalidPdfEmbedding);
    }

    segments.sort_by_key(|segment| segment.header.segment_number);
    // One byte per segment tracks both reference lifetime and auxiliary use.
    // State is local to this image, even when another image shares its globals.
    let mut states = vec![0_u8; segments.len()];
    let mut attachment: Option<(u32, bool)> = None;
    for (index, segment) in segments.iter().enumerate() {
        if index > 0 && segments[index - 1].header.segment_number == segment.header.segment_number {
            bail!(FormatError::InvalidPdfEmbedding);
        }
        let count = segment.header.referred_to_segments.len();
        let attached_to =
            if segment.header.segment_type == SegmentType::Extension && count == 1 && index > 0 {
                let owner = segment.header.referred_to_segments[0];
                (segments[index - 1].header.segment_number == owner
                    || attachment.is_some_and(|(previous, _)| previous == owner))
                .then_some(owner)
            } else {
                None
            };
        if let Some((owner, true)) = attachment
            && attached_to != Some(owner)
            && segments[index - 1].header.retains(1)
        {
            bail!(FormatError::InvalidPdfEmbedding);
        }
        if !segment.header.retains(0) {
            states[index] |= DISCARDED;
        }
        let mut must_release_owner = false;
        let valid_count = match segment.header.segment_type {
            SegmentType::SymbolDictionary
            | SegmentType::IntermediateTextRegion
            | SegmentType::ImmediateTextRegion
            | SegmentType::ImmediateLosslessTextRegion
            | SegmentType::Extension => true,
            SegmentType::IntermediateHalftoneRegion
            | SegmentType::ImmediateHalftoneRegion
            | SegmentType::ImmediateLosslessHalftoneRegion
            | SegmentType::IntermediateGenericRefinementRegion => count == 1,
            SegmentType::ImmediateGenericRefinementRegion
            | SegmentType::ImmediateLosslessGenericRefinementRegion => count <= 1,
            _ => count == 0,
        };
        if !valid_count {
            bail!(FormatError::InvalidPdfEmbedding);
        }
        let mut tables = 0;
        for referred in &segment.header.referred_to_segments {
            // The header parser already requires strictly lower references.
            let Ok(referred_index) = segments[..index]
                .binary_search_by_key(referred, |segment| segment.header.segment_number)
            else {
                bail!(FormatError::InvalidPdfEmbedding);
            };
            let referred_header = &segments[referred_index].header;
            if states[referred_index] & DISCARDED != 0
                || (referred_header.deferred_non_retain && attached_to != Some(*referred))
            {
                bail!(FormatError::InvalidPdfEmbedding);
            }
            let referred_page = referred_header.page_association;
            if referred_page != 0 && referred_page != segment.header.page_association {
                bail!(FormatError::InvalidPdfEmbedding);
            }
            // A global owner's chain can continue in a different PDF image.
            // This image has every segment of its own page, but cannot prove
            // that an observed global chain contains its final extension.
            if attached_to == Some(*referred) {
                must_release_owner = referred_header.deferred_non_retain && referred_page != 0;
            }
            let kind = referred_header.segment_type;
            tables += usize::from(kind == SegmentType::Tables);
            let valid_type = match segment.header.segment_type {
                SegmentType::SymbolDictionary
                | SegmentType::IntermediateTextRegion
                | SegmentType::ImmediateTextRegion
                | SegmentType::ImmediateLosslessTextRegion => {
                    matches!(kind, SegmentType::SymbolDictionary | SegmentType::Tables)
                }
                SegmentType::IntermediateHalftoneRegion
                | SegmentType::ImmediateHalftoneRegion
                | SegmentType::ImmediateLosslessHalftoneRegion => {
                    kind == SegmentType::PatternDictionary
                }
                SegmentType::IntermediateGenericRefinementRegion
                | SegmentType::ImmediateGenericRefinementRegion
                | SegmentType::ImmediateLosslessGenericRefinementRegion => is_intermediate(kind),
                SegmentType::Extension => true,
                _ => false,
            };
            if !valid_type {
                bail!(FormatError::InvalidPdfEmbedding);
            }
            if is_intermediate(kind) && segment.header.segment_type != SegmentType::Extension {
                if states[referred_index] & CONSUMED != 0 {
                    bail!(FormatError::InvalidPdfEmbedding);
                }
                states[referred_index] |= CONSUMED;
            }
        }
        let max_tables = if segment.header.segment_type == SegmentType::SymbolDictionary {
            4
        } else {
            8
        };
        if segment.header.segment_type != SegmentType::Extension && tables > max_tables {
            bail!(FormatError::InvalidPdfEmbedding);
        }
        // Zero retention bits prohibit references in later segments. Apply
        // them after checking all current references, including duplicate
        // numbers; a later keep bit cannot resurrect a discarded segment.
        for (bit, referred) in segment.header.referred_to_segments.iter().enumerate() {
            if !segment.header.retains(bit + 1) {
                let Ok(referred_index) = segments[..index]
                    .binary_search_by_key(referred, |segment| segment.header.segment_number)
                else {
                    bail!(FormatError::InvalidPdfEmbedding);
                };
                states[referred_index] |= DISCARDED;
            }
        }
        attachment = attached_to.map(|owner| (owner, must_release_owner));
    }
    if attachment.is_some_and(|(_, must_release)| must_release)
        && segments
            .last()
            .is_some_and(|segment| segment.header.retains(1))
    {
        bail!(FormatError::InvalidPdfEmbedding);
    }
    let page_info_segment = segments
        .iter()
        .find(|segment| segment.header.segment_type == SegmentType::PageInformation)
        .ok_or(FormatError::MissingPageInfo)?;
    let page_info = parse_page_information(&mut Reader::new(page_info_segment.data))?;
    if page_info.flags.might_contain_coloured {
        bail!(FormatError::InvalidPdfEmbedding);
    }
    for (index, segment) in segments.iter().enumerate() {
        let kind = segment.header.segment_type;
        let refinement = matches!(
            kind,
            SegmentType::IntermediateGenericRefinementRegion
                | SegmentType::ImmediateGenericRefinementRegion
                | SegmentType::ImmediateLosslessGenericRefinementRegion
        );
        let region = refinement
            || is_intermediate(kind)
            || matches!(
                kind,
                SegmentType::ImmediateTextRegion
                    | SegmentType::ImmediateLosslessTextRegion
                    | SegmentType::ImmediateGenericRegion
                    | SegmentType::ImmediateLosslessGenericRegion
                    | SegmentType::ImmediateHalftoneRegion
                    | SegmentType::ImmediateLosslessHalftoneRegion
            );
        if !region {
            continue;
        }
        if (refinement && !page_info.flags.might_contain_refinements)
            || (is_intermediate(kind)
                && (!page_info.flags.requires_auxiliary_buffers || states[index] & CONSUMED == 0))
        {
            bail!(FormatError::InvalidPdfEmbedding);
        }
        let info = parse_region_segment_info(&mut Reader::new(segment.data))?;
        let page_refinement = refinement && segment.header.referred_to_segments.is_empty();
        if info._colour_extension
            || (page_refinement && info.combination_operator != CombinationOperator::Replace)
            || (!page_refinement
                && !page_info.flags.combination_operator_overridden
                && info.combination_operator != page_info.flags.default_combination_operator)
        {
            bail!(FormatError::InvalidPdfEmbedding);
        }
    }
    Ok(segments)
}

fn is_intermediate(kind: SegmentType) -> bool {
    matches!(
        kind,
        SegmentType::IntermediateTextRegion
            | SegmentType::IntermediateHalftoneRegion
            | SegmentType::IntermediateGenericRegion
            | SegmentType::IntermediateGenericRefinementRegion
    )
}
