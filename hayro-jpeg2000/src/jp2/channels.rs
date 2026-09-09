//! Resolve codestream components, mapped channels, and colour associations.
use alloc::vec;
use alloc::vec::Vec;

use super::ImageBoxes;
use super::cdef::{ChannelAssociation, ChannelDefinitionBox, ChannelType};
use super::cmap::{ComponentMappingEntry, ComponentMappingType};
use crate::j2c::Header;
use crate::{ColorSpace, DecodeSettings, Result, ValidationError, bail, get_color_space};

pub(crate) struct ChannelLayout {
    pub(crate) color_space: ColorSpace,
    pub(crate) has_alpha: bool,
    pub(crate) mappings: Vec<ComponentMappingEntry>,
}

pub(crate) fn resolve(
    boxes: &ImageBoxes,
    header: &Header<'_>,
    settings: &DecodeSettings,
    color_components: Option<u8>,
) -> Result<ChannelLayout> {
    let layout = resolve_expanded(boxes, header, settings, color_components)?;
    // PDF Indexed uses the original index samples with its own palette. Still
    // validate the container's declared mappings before bypassing their output.
    if !settings.resolve_palette_indices && boxes.palette.is_some() {
        return Ok(ChannelLayout {
            color_space: ColorSpace::Gray,
            has_alpha: false,
            mappings: (0..header.component_infos.len())
                .map(|index| ComponentMappingEntry {
                    component_index: index as u16,
                    mapping_type: ComponentMappingType::Direct,
                })
                .collect(),
        });
    }
    Ok(layout)
}

fn resolve_expanded(
    boxes: &ImageBoxes,
    header: &Header<'_>,
    settings: &DecodeSettings,
    color_components: Option<u8>,
) -> Result<ChannelLayout> {
    let raw_count = header.component_infos.len();
    let resolve_palette = boxes.palette.is_some();
    let mappings = if resolve_palette {
        boxes
            .component_mapping
            .as_ref()
            .ok_or(ValidationError::InvalidComponentMetadata)?
            .entries
            .clone()
    } else {
        (0..raw_count)
            .map(|index| ComponentMappingEntry {
                component_index: index as u16,
                mapping_type: ComponentMappingType::Direct,
            })
            .collect()
    };
    for mapping in &mappings {
        if usize::from(mapping.component_index) >= raw_count {
            bail!(ValidationError::InvalidComponentMetadata);
        }
        if let ComponentMappingType::Palette { column } = mapping.mapping_type
            && boxes
                .palette
                .as_ref()
                .and_then(|palette| palette.columns.get(usize::from(column)))
                .is_none()
        {
            bail!(ValidationError::InvalidComponentMetadata);
        }
    }
    let declared_colors = boxes.channel_definition.as_ref().and_then(|definitions| {
        definitions
            .channel_definitions
            .iter()
            .filter_map(
                |definition| match (definition.channel_type, definition.association) {
                    (ChannelType::Colour, ChannelAssociation::Colour(color)) => {
                        Some(usize::from(color))
                    }
                    _ => None,
                },
            )
            .max()
    });
    let expanded_components =
        color_components.filter(|_| settings.resolve_palette_indices || boxes.palette.is_none());
    let mut color_space = match expanded_components {
        Some(num_channels) => ColorSpace::Unknown { num_channels },
        None => get_color_space(boxes, declared_colors.unwrap_or(mappings.len()))?,
    };
    if let Some(definitions) = &boxes.channel_definition {
        let (order, has_alpha) =
            definitions.order(mappings.len(), usize::from(color_space.num_channels()))?;
        // Table I.16 constrains the component mapped to opacity, which need not
        // be the final codestream component. Palette outputs have their own type.
        for definition in &definitions.channel_definitions {
            if definition.channel_type.is_opacity() {
                let mapping = &mappings[usize::from(definition.channel_index)];
                if mapping.mapping_type == ComponentMappingType::Direct
                    && header.component_infos[usize::from(mapping.component_index)]
                        .size_info
                        .is_signed
                {
                    bail!(ValidationError::InvalidComponentMetadata);
                }
            }
        }
        return Ok(ChannelLayout {
            color_space,
            has_alpha,
            mappings: order.into_iter().map(|index| mappings[index]).collect(),
        });
    }

    // Preserve the standalone decoder's legacy colour/count recovery for files
    // without cdef. A PDF requesting opacity separately requires declared alpha.
    let mut has_alpha = false;
    if mappings.len() != usize::from(color_space.num_channels()) {
        if !settings.strict && mappings.len() == usize::from(color_space.num_channels()) + 1 {
            has_alpha = true;
        } else {
            color_space = match mappings.len() {
                1 => ColorSpace::Gray,
                3 => ColorSpace::RGB,
                4 => ColorSpace::CMYK,
                _ => bail!(ValidationError::TooManyChannels),
            };
        }
    }
    Ok(ChannelLayout {
        color_space,
        has_alpha,
        mappings,
    })
}

impl ChannelDefinitionBox {
    fn order(&self, channel_count: usize, color_count: usize) -> Result<(Vec<usize>, bool)> {
        if color_count == 0 || channel_count == 0 {
            bail!(ValidationError::InvalidComponentMetadata);
        }
        let mut colors = vec![None; color_count];
        let mut types = vec![None; channel_count];
        let mut opacity = None;
        let mut whole_image = false;
        let mut opacity_colors = vec![false; color_count];
        for definition in &self.channel_definitions {
            let index = usize::from(definition.channel_index);
            let prior = types
                .get_mut(index)
                .ok_or(ValidationError::InvalidComponentMetadata)?;
            if prior.is_some_and(|prior| prior != definition.channel_type) {
                bail!(ValidationError::InvalidComponentMetadata);
            }
            *prior = Some(definition.channel_type);
            match (definition.channel_type, definition.association) {
                (ChannelType::Colour, ChannelAssociation::Colour(color)) => {
                    let slot = colors
                        .get_mut(usize::from(color) - 1)
                        .ok_or(ValidationError::InvalidComponentMetadata)?;
                    if slot.replace(index).is_some() {
                        bail!(ValidationError::InvalidComponentMetadata);
                    }
                }
                (ChannelType::Colour, ChannelAssociation::WholeImage) => {
                    bail!(ValidationError::InvalidComponentMetadata);
                }
                (kind, association)
                    if kind.is_opacity() && association != ChannelAssociation::Unspecified =>
                {
                    if opacity.is_some_and(|prior| prior != index) {
                        // Distinct per-colour opacity needs a different output
                        // representation; never reduce it to one arbitrary mask.
                        bail!(ValidationError::InvalidComponentMetadata);
                    }
                    opacity = Some(index);
                    let covered = match association {
                        ChannelAssociation::WholeImage => &mut whole_image,
                        ChannelAssociation::Colour(color) => opacity_colors
                            .get_mut(usize::from(color) - 1)
                            .ok_or(ValidationError::InvalidComponentMetadata)?,
                        ChannelAssociation::Unspecified => unreachable!(),
                    };
                    if *covered {
                        bail!(ValidationError::InvalidComponentMetadata);
                    }
                    *covered = true;
                }
                // Unspecified type/association is an auxiliary channel, not a
                // colour or an inferred opacity plane.
                _ => {}
            }
        }
        if types.iter().any(Option::is_none)
            || opacity.is_some() && !whole_image && opacity_colors.iter().any(|covered| !covered)
        {
            bail!(ValidationError::InvalidComponentMetadata);
        }
        let mut order = colors
            .into_iter()
            .collect::<Option<Vec<_>>>()
            .ok_or(ValidationError::InvalidComponentMetadata)?;
        order.extend(opacity);
        Ok((order, opacity.is_some()))
    }
}
