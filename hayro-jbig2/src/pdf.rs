//! PDF 1.7 §7.4.7 and T.88 §§7.2.5–7.3.2, 7.4.8, D.3.
use crate::error::{FormatError, Result, bail};
use crate::file::parse_segments_sequential;
use crate::reader::Reader;
use crate::segment::{Segment, SegmentType};
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
    for (index, segment) in segments.iter().enumerate() {
        if index > 0 && segments[index - 1].header.segment_number == segment.header.segment_number {
            bail!(FormatError::InvalidPdfEmbedding);
        }
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
        }
    }
    Ok(segments)
}
