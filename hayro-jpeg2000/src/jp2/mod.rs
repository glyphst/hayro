//! Reading a JP2 file, defined in Annex I.

use alloc::vec::Vec;

use crate::error::{FormatError, Result, bail};
use crate::j2c::ComponentData;
use crate::jp2::r#box::{FILE_TYPE, JP2_SIGNATURE};
use crate::jp2::cdef::ChannelDefinitionBox;
use crate::jp2::cmap::ComponentMappingBox;
use crate::jp2::colr::ColorSpecificationBox;
use crate::jp2::pclr::PaletteBox;
use crate::reader::BitReader;
use crate::{DecodeSettings, Image};

pub(crate) mod r#box;
pub(crate) mod cdef;
pub(crate) mod channels;
pub(crate) mod cmap;
pub(crate) mod colr;
pub(crate) mod icc;
pub(crate) mod pclr;

#[derive(Debug, Clone, Default)]
pub(crate) struct ImageBoxes {
    pub(crate) color_specification: Option<ColorSpecificationBox>,
    pub(crate) channel_definition: Option<ChannelDefinitionBox>,
    pub(crate) palette: Option<PaletteBox>,
    pub(crate) component_mapping: Option<ComponentMappingBox>,
}

/// A decoded JPEG2000 image.
pub struct DecodedImage<'a> {
    /// The raw decoded JPEG2000 codestream components.
    pub(crate) decoded_components: &'a mut Vec<ComponentData>,
    /// The JP2 boxes of the image. In the case of a raw codestream, we
    /// will synthesize the necessary boxes.
    pub(crate) boxes: ImageBoxes,
}

pub(crate) fn parse<'a>(
    data: &'a [u8],
    mut settings: DecodeSettings,
    color_components: Option<u8>,
) -> Result<Image<'a>> {
    let mut reader = BitReader::new(data);
    let signature_box = r#box::read(&mut reader).ok_or(FormatError::InvalidBox)?;

    if signature_box.box_type != JP2_SIGNATURE {
        bail!(FormatError::InvalidSignature);
    }

    let file_type_box = r#box::read(&mut reader).ok_or(FormatError::InvalidBox)?;

    if file_type_box.box_type != FILE_TYPE {
        bail!(FormatError::InvalidFileType);
    }

    let mut image_boxes: Option<ImageBoxes> = None;
    let mut parsed_codestream = None;

    // Read boxes until we find the JP2 Header box
    while !reader.at_end() {
        let Some(current_box) = r#box::read(&mut reader) else {
            if settings.strict {
                bail!(FormatError::InvalidBox);
            }

            break;
        };

        match current_box.box_type {
            r#box::JP2_HEADER => {
                if image_boxes.is_some() {
                    bail!(FormatError::InvalidBox);
                }
                let mut boxes = ImageBoxes::default();

                let mut jp2h_reader = BitReader::new(current_box.data);

                // Read child boxes within JP2 Header box
                while !jp2h_reader.at_end() {
                    let child_box = r#box::read(&mut jp2h_reader).ok_or(FormatError::InvalidBox)?;

                    match child_box.box_type {
                        r#box::CHANNEL_DEFINITION => {
                            if boxes.channel_definition.is_some() {
                                bail!(FormatError::InvalidBox);
                            }
                            cdef::parse(&mut boxes, child_box.data)?;
                        }
                        r#box::COLOUR_SPECIFICATION => {
                            if color_components.is_none() {
                                colr::parse(&mut boxes, child_box.data)?;
                            }
                        }
                        r#box::PALETTE => {
                            if boxes.palette.is_some() {
                                bail!(FormatError::InvalidBox);
                            }
                            pclr::parse(&mut boxes, child_box.data)?;

                            // If we have a palettized image, decoding at a
                            // lower resolution will corrupt it, so we can't do
                            // it in this case.
                            settings.target_resolution = None;
                        }
                        r#box::COMPONENT_MAPPING => {
                            if boxes.component_mapping.is_some() {
                                bail!(FormatError::InvalidBox);
                            }
                            cmap::parse(&mut boxes, child_box.data)?;
                        }
                        _ => {
                            debug!(
                                "ignoring header box {}",
                                r#box::tag_to_string(child_box.box_type)
                            );
                        }
                    }
                }

                image_boxes = Some(boxes);
            }
            r#box::CONTIGUOUS_CODESTREAM => {
                if parsed_codestream.is_some() {
                    bail!(FormatError::Unsupported);
                }
                parsed_codestream = Some(crate::j2c::parse_raw(current_box.data, &settings)?);
            }
            _ => {}
        }
    }

    let image_boxes = image_boxes.ok_or(FormatError::InvalidBox)?;
    let parsed_codestream = parsed_codestream.ok_or(FormatError::MissingCodestream)?;

    if image_boxes.palette.is_some() != image_boxes.component_mapping.is_some() {
        bail!(FormatError::InvalidBox);
    }
    let layout = channels::resolve(
        &image_boxes,
        &parsed_codestream.header,
        &settings,
        color_components,
    )?;

    Ok(Image {
        codestream: parsed_codestream.data,
        header: parsed_codestream.header,
        boxes: image_boxes,
        color_space: layout.color_space,
        has_alpha: layout.has_alpha,
        channels: layout.mappings,
    })
}
