//! PDF 1.7 §7.4.7 and T.88 §§7.2.5–7.3.2, 7.4.8, D.3.
use crate::decode::{CombinationOperator, parse_region_segment_info};
use crate::error::{FormatError, Result, bail};
use crate::file::parse_segments_sequential;
use crate::page_info::parse_page_information;
use crate::reader::Reader;
use crate::segment::{Segment, SegmentType};
use alloc::vec;
use alloc::vec::Vec;

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
    let mut consumed = vec![false; segments.len()];
    for (index, segment) in segments.iter().enumerate() {
        if index > 0 && segments[index - 1].header.segment_number == segment.header.segment_number {
            bail!(FormatError::InvalidPdfEmbedding);
        }
        let count = segment.header.referred_to_segments.len();
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
            let referred_page = segments[referred_index].header.page_association;
            if referred_page != 0 && referred_page != segment.header.page_association {
                bail!(FormatError::InvalidPdfEmbedding);
            }
            let kind = segments[referred_index].header.segment_type;
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
                if consumed[referred_index] {
                    bail!(FormatError::InvalidPdfEmbedding);
                }
                consumed[referred_index] = true;
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
                && (!page_info.flags.requires_auxiliary_buffers || !consumed[index]))
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
