//! PDF rendering-intent state, independent of the CMS library's defaults.

use hayro_syntax::object::{Name, Object};

/// The four standard PDF color rendering intents.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
#[repr(u8)]
pub enum RenderingIntent {
    /// Preserve the profile's perceptual reproduction.
    Perceptual,
    /// Match colorimetry relative to the source and destination media whites.
    #[default]
    RelativeColorimetric,
    /// Preserve saturation using the profile's saturation reproduction.
    Saturation,
    /// Preserve media colorimetry with fully adapted viewing conditions.
    AbsoluteColorimetric,
}

impl RenderingIntent {
    /// Resolve a standard name. Unrecognized names use the PDF default.
    /// The boolean indicates that the name was unrecognized.
    pub fn from_name(name: &Name<'_>) -> (Self, bool) {
        match name.as_ref() {
            b"Perceptual" => (Self::Perceptual, false),
            b"RelativeColorimetric" => (Self::RelativeColorimetric, false),
            b"Saturation" => (Self::Saturation, false),
            b"AbsoluteColorimetric" => (Self::AbsoluteColorimetric, false),
            _ => (Self::default(), true),
        }
    }

    /// Resolve an explicitly present intent value, reporting malformed values.
    pub fn from_object(object: Option<Object<'_>>) -> Result<(Self, bool), ColorConversionError> {
        match object {
            Some(Object::Name(name)) => Ok(Self::from_name(&name)),
            _ => Err(ColorConversionError::InvalidIntent),
        }
    }

    pub(crate) fn cms(self) -> moxcms::RenderingIntent {
        match self {
            Self::Perceptual => moxcms::RenderingIntent::Perceptual,
            Self::RelativeColorimetric => moxcms::RenderingIntent::RelativeColorimetric,
            Self::Saturation => moxcms::RenderingIntent::Saturation,
            Self::AbsoluteColorimetric => moxcms::RenderingIntent::AbsoluteColorimetric,
        }
    }
}

/// A color conversion cannot be represented by the interactive output policy.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ColorConversionError {
    /// An explicitly specified rendering intent is not a name.
    InvalidIntent,
    /// The embedded profile or requested transform is invalid or unsupported.
    IccTransform,
    /// Non-default calibrated/Lab intent conversion is outside the proven subset.
    CalibratedIntent,
}

impl std::fmt::Display for ColorConversionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::InvalidIntent => "rendering intent must be a PDF name",
            Self::IccTransform => {
                "ICC profile cannot provide the requested source-to-sRGB transform"
            }
            Self::CalibratedIntent => {
                "non-default calibrated/Lab rendering intent requires unsupported color conversion"
            }
        })
    }
}

impl std::error::Error for ColorConversionError {}
