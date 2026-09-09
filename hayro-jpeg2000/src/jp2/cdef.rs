//! The channel definition box (cdef), defined in I.5.3.6.

use alloc::vec::Vec;

use crate::error::{FormatError, Result, bail};
use crate::jp2::ImageBoxes;
use crate::reader::BitReader;

pub(crate) fn parse(boxes: &mut ImageBoxes, data: &[u8]) -> Result<()> {
    let mut reader = BitReader::new(data);
    let count = reader.read_u16().ok_or(FormatError::InvalidBox)? as usize;
    let mut definitions = Vec::with_capacity(count);

    if count == 0 || data.len() != 2 + count * 6 {
        bail!(FormatError::InvalidBox);
    }

    for _ in 0..count {
        let channel_index = reader.read_u16().ok_or(FormatError::InvalidBox)?;
        let channel_type = reader.read_u16().ok_or(FormatError::InvalidBox)?;
        let association = reader.read_u16().ok_or(FormatError::InvalidBox)?;

        definitions.push(ChannelDefinition {
            channel_index,
            channel_type: ChannelType::from_raw(channel_type).ok_or(FormatError::InvalidBox)?,
            association: ChannelAssociation::from_raw(association),
        });
    }

    boxes.channel_definition = Some(ChannelDefinitionBox {
        channel_definitions: definitions,
    });

    Ok(())
}

#[derive(Debug, Clone)]
pub(crate) struct ChannelDefinitionBox {
    pub(crate) channel_definitions: Vec<ChannelDefinition>,
}

#[derive(Debug, Clone)]
pub(crate) struct ChannelDefinition {
    pub(crate) channel_index: u16,
    pub(crate) channel_type: ChannelType,
    pub(crate) association: ChannelAssociation,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ChannelType {
    Colour,
    Opacity,
    PremultipliedOpacity,
    Unspecified,
}

impl ChannelType {
    fn from_raw(value: u16) -> Option<Self> {
        match value {
            0 => Some(Self::Colour),
            1 => Some(Self::Opacity),
            2 => Some(Self::PremultipliedOpacity),
            u16::MAX => Some(Self::Unspecified),
            _ => None,
        }
    }

    pub(crate) fn is_opacity(self) -> bool {
        matches!(self, Self::Opacity | Self::PremultipliedOpacity)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ChannelAssociation {
    WholeImage,
    Colour(u16),
    Unspecified,
}

impl ChannelAssociation {
    fn from_raw(value: u16) -> Self {
        match value {
            0 => Self::WholeImage,
            u16::MAX => Self::Unspecified,
            v => Self::Colour(v),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{ChannelAssociation, ChannelType, parse};
    use crate::jp2::ImageBoxes;

    #[test]
    fn parses_premultiplied_opacity_channel_definition() {
        let mut boxes = ImageBoxes::default();
        parse(
            &mut boxes,
            &[
                0, 2, // channel count
                0, 0, 0, 0, 0, 1, // color channel 0, association 1
                0, 1, 0, 2, 0, 0, // channel 1, premultiplied opacity, whole image
            ],
        )
        .expect("premultiplied cdef");

        let definitions = &boxes
            .channel_definition
            .expect("channel definition")
            .channel_definitions;
        assert_eq!(
            definitions[1].channel_type,
            ChannelType::PremultipliedOpacity
        );
        assert_eq!(definitions[1].association, ChannelAssociation::WholeImage);
        assert!(definitions[1].channel_type.is_opacity());
    }
}
