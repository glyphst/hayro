use super::{ColorSpace, ColorSpaceKind, ColorSpaceType};
use crate::ImageColorSpaceProperties;
use crate::cache::Cache;
use hayro_syntax::object::dict::keys::{DEFAULT_CMYK, DEFAULT_GRAY, DEFAULT_RGB};
use hayro_syntax::object::{Name, Null, Object};
use hayro_syntax::page::Resources;

/// A raster image color space could not be resolved exactly.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum ImageColorSpaceError {
    /// The declared space or a selected resource is invalid.
    Invalid,
    /// A default is invalid, has the wrong arity, or uses a prohibited family.
    InvalidDefault(&'static str),
    /// Color-space nesting or resource aliases exceed eight levels.
    NestingLimit,
}

impl std::fmt::Display for ImageColorSpaceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Invalid => f.write_str("invalid image color space or resource"),
            Self::InvalidDefault(name) => write!(f, "invalid {name} image color-space replacement"),
            Self::NestingLimit => f.write_str("image color-space nesting exceeds eight levels"),
        }
    }
}

impl std::error::Error for ImageColorSpaceError {}

impl ColorSpace {
    /// Resolve an image's color space in its current resource scope.
    ///
    /// Default device spaces replace direct selections, Indexed bases, and
    /// selected Separation/DeviceN alternatives before any sample conversion.
    /// ICC identity is preserved. The returned data owns its color transforms
    /// and does not borrow the resource dictionary or source object.
    pub fn from_image_pdf_object<'a>(
        object: Object<'a>,
        resources: &Resources<'a>,
    ) -> Result<(Self, ImageColorSpaceProperties), ImageColorSpaceError> {
        Self::new_image(object, &Cache::new(), resources)
    }

    pub(crate) fn new_image<'a>(
        object: Object<'a>,
        cache: &Cache,
        resources: &Resources<'a>,
    ) -> Result<(Self, ImageColorSpaceProperties), ImageColorSpaceError> {
        parse_image_color_space(
            object,
            cache,
            &mut |name| {
                let mut current = Some(resources);
                while let Some(resources) = current {
                    if resources.color_spaces.contains_key(name.as_ref()) {
                        // A broken local entry shadows its parent. Preserve its
                        // presence so it cannot masquerade as an absent default.
                        return Some(
                            resources
                                .color_spaces
                                .get::<Object<'_>>(name.as_ref())
                                .unwrap_or(Object::Null(Null)),
                        );
                    }
                    current = resources.parent();
                }
                None
            },
            0,
            true,
        )
    }
}

fn parse_image_color_space<'a>(
    object: Object<'a>,
    cache: &Cache,
    resolve: &mut dyn FnMut(&Name<'_>) -> Option<Object<'a>>,
    depth: u8,
    apply_defaults: bool,
) -> Result<(ColorSpace, ImageColorSpaceProperties), ImageColorSpaceError> {
    if depth >= 8 {
        return Err(ImageColorSpaceError::NestingLimit);
    }
    if let Object::Name(name) = &object
        && ColorSpaceType::new_from_name(name).is_none()
    {
        let object = resolve(name).ok_or(ImageColorSpaceError::Invalid)?;
        return parse_image_color_space(object, cache, resolve, depth + 1, apply_defaults);
    }

    let mut overridden = false;
    let mut nested_error = None;
    let parsed =
        ColorSpaceType::new_inner(object, cache, &mut |object| match parse_image_color_space(
            object,
            cache,
            resolve,
            depth + 1,
            apply_defaults,
        ) {
            Ok((space, properties)) => {
                overridden |= properties.color_space_is_default_overridden();
                Some(space)
            }
            Err(error) => {
                nested_error = Some(error);
                None
            }
        });
    if let Some(error) = nested_error {
        return Err(error);
    }
    let mut space = ColorSpace::from_kind(parsed.ok_or(ImageColorSpaceError::Invalid)?);
    let declared = space.kind();
    if declared == ColorSpaceKind::Pattern {
        return Err(ImageColorSpaceError::Invalid);
    }
    let default = match declared {
        ColorSpaceKind::DeviceGray => Some((DEFAULT_GRAY, "DefaultGray", 1)),
        ColorSpaceKind::DeviceRgb => Some((DEFAULT_RGB, "DefaultRGB", 3)),
        ColorSpaceKind::DeviceCmyk => Some((DEFAULT_CMYK, "DefaultCMYK", 4)),
        _ => None,
    };
    if apply_defaults
        && let Some((name, label, components)) = default
        && let Some(object) = resolve(&Name::new_unescaped(name))
    {
        // The replacement is interpreted once, even if it itself contains a
        // device space. In particular DefaultRGB /DeviceRGB is a valid no-op.
        let (replacement, _) = parse_image_color_space(object, cache, resolve, depth + 1, false)
            .map_err(|error| match error {
                ImageColorSpaceError::NestingLimit => error,
                _ => ImageColorSpaceError::InvalidDefault(label),
            })?;
        if replacement.component_count() != components
            || matches!(
                replacement.kind(),
                ColorSpaceKind::Lab | ColorSpaceKind::Indexed | ColorSpaceKind::Pattern
            )
        {
            return Err(ImageColorSpaceError::InvalidDefault(label));
        }
        space = replacement;
        overridden = true;
    }
    let properties = ImageColorSpaceProperties {
        has_color_space: true,
        declared_color_space_kind: Some(declared),
        color_space_kind: Some(space.kind()),
        color_space_components: Some(space.component_count()),
        color_space_default_overridden: overridden,
    };
    Ok((space, properties))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::color::ToRgb;
    use hayro_syntax::Pdf;

    fn document(space: &str, defaults: &str, extra: &str) -> Pdf {
        Pdf::new(
            format!(
                "%PDF-1.7\n\
             1 0 obj <</Type/Catalog/Pages 2 0 R>> endobj\n\
             2 0 obj <</Type/Pages/Kids[3 0 R]/Count 1>> endobj\n\
             3 0 obj <</Type/Page/Parent 2 0 R/MediaBox[0 0 100 100]\
             /Resources<</ColorSpace<</Source {space} {defaults}>> >> >> endobj\n\
             {extra}\ntrailer <</Root 1 0 R>>\n%%EOF"
            )
            .into_bytes(),
        )
        .expect("color-space document")
    }

    fn parse(
        pdf: &Pdf,
        cache: &Cache,
    ) -> Result<(ColorSpace, ImageColorSpaceProperties), ImageColorSpaceError> {
        let resources = pdf.pages()[0].resources();
        ColorSpace::new_image(
            Object::Name(Name::new_unescaped(b"Source")),
            cache,
            resources,
        )
    }

    #[test]
    fn nested_image_defaults_match_explicit_spaces_and_preserve_outer_samples() {
        let cases = [
            (
                "DeviceGray",
                "DefaultGray",
                "[/CalGray <</WhitePoint[0.9505 1 1.089]/Gamma 2>>]",
                "4080",
                "[0]",
                "[1]",
                "{ pop }",
            ),
            (
                "DeviceRGB",
                "DefaultRGB",
                "[/CalRGB <</WhitePoint[0.9505 1 1.089]/Gamma[2 2 2]>>]",
                "4080C080C040",
                "[0 0 0]",
                "[1 1 1]",
                "{ pop dup dup }",
            ),
            (
                "DeviceCMYK",
                "DefaultCMYK",
                "[/DeviceN [/A /B /C /D] /DeviceRGB 5 0 R]",
                "4080C02080C04020",
                "[0 0 0 0]",
                "[1 1 1 1]",
                "{ pop dup dup dup }",
            ),
        ];
        let cache = Cache::new();
        for (device, default, replacement, palette, c0, c1, function) in cases {
            let wrappers = [
                (
                    format!("[/Indexed /{device} 1 <{palette}>]"),
                    vec![0, 1],
                    ColorSpaceKind::Indexed,
                ),
                (
                    format!(
                        "[/Separation /Spot /{device} <</FunctionType 2/Domain[0 1]/C0{c0}/C1{c1}/N 1>>]"
                    ),
                    vec![0, 64, 128, 255],
                    ColorSpaceKind::Separation,
                ),
                (
                    format!("[/DeviceN [/SpotA /SpotB] /{device} 4 0 R]"),
                    vec![0, 255, 64, 128, 128, 64, 255, 0],
                    ColorSpaceKind::DeviceN,
                ),
            ];
            let cmyk_function = "{ pop pop pop dup dup }";
            let range = "0 1 ".repeat(c0.split_whitespace().count());
            let extra = format!(
                "4 0 obj <</FunctionType 4/Domain[0 1 0 1]/Range[{range}]/Length {}>> stream\n{function}\nendstream endobj\n5 0 obj <</FunctionType 4/Domain[0 1 0 1 0 1 0 1]/Range[0 1 0 1 0 1]/Length {}>> stream\n{cmyk_function}\nendstream endobj",
                function.len(),
                cmyk_function.len(),
            );
            for (source, input, kind) in wrappers {
                let explicit = source.replace(&format!("/{device}"), replacement);
                let remapped_pdf = document(&source, &format!("/{default} {replacement}"), &extra);
                let explicit_pdf = document(&explicit, "", &extra);
                let (remapped, properties) = parse(&remapped_pdf, &cache).expect("remapped space");
                let (explicit, explicit_properties) =
                    parse(&explicit_pdf, &cache).expect("explicit space");
                assert_eq!(properties.declared_color_space_kind(), Some(kind));
                assert_eq!(properties.color_space_kind(), Some(kind));
                assert!(properties.color_space_is_default_overridden());
                assert!(!explicit_properties.color_space_is_default_overridden());
                let count = input.len() / remapped.component_count();
                let mut actual = vec![0; count * 3];
                let mut expected = vec![0; count * 3];
                remapped
                    .convert(&input, &mut actual)
                    .expect("remapped pixels");
                explicit
                    .convert(&input, &mut expected)
                    .expect("explicit pixels");
                assert_eq!(actual, expected, "{default}: {source}");
            }
        }
    }

    #[test]
    fn image_defaults_reject_invalid_entries_and_bound_aliases() {
        let cache = Cache::new();
        for replacement in [
            "/DeviceGray",
            "null",
            "999 0 R",
            "[/Lab <</WhitePoint[1 1 1]>>]",
            "[/Indexed /DeviceRGB 0 <000000>]",
            "/Pattern",
        ] {
            let pdf = document(
                "[/Indexed /DeviceRGB 0 <4080C0>]",
                &format!("/DefaultRGB {replacement}"),
                "",
            );
            assert!(
                matches!(
                    parse(&pdf, &cache),
                    Err(ImageColorSpaceError::InvalidDefault("DefaultRGB"))
                ),
                "{replacement}"
            );
        }
        let pdf = document("/Cycle", "/Cycle /Source", "");
        assert!(matches!(
            parse(&pdf, &cache),
            Err(ImageColorSpaceError::NestingLimit)
        ));
        let pdf = document("/DeviceRGB", "/DefaultRGB /DeviceRGB", "");
        assert!(
            parse(&pdf, &cache)
                .expect("identity default")
                .1
                .color_space_is_default_overridden()
        );
        for count in [6, 7] {
            let aliases = (0..count)
                .map(|index| {
                    let next = if index + 1 == count {
                        "/DeviceRGB".to_owned()
                    } else {
                        format!("/A{}", index + 1)
                    };
                    format!("/A{index} {next} ")
                })
                .collect::<String>();
            let pdf = document("/A0", &aliases, "");
            assert_eq!(parse(&pdf, &cache).is_ok(), count == 6);
        }
    }

    #[test]
    fn image_lookup_tables_and_invalid_defaults_respect_resource_scope() {
        use hayro_syntax::object::{Dict, FromBytes};
        let pdf = document(
            "[/Indexed /DeviceRGB 0 <4080C0>]",
            "/DefaultRGB [/CalRGB <</WhitePoint[0.9505 1 1.089]/Gamma[2 2 2]>>]",
            "",
        );
        let cache = Cache::new();
        let (parent, _) = parse(&pdf, &cache).expect("parent color space");
        let mut expected = [0; 3];
        parent
            .convert(&[0], &mut expected)
            .expect("warm parent lookup");
        assert_ne!(expected, [0x40, 0x80, 0xc0]);
        let child = Resources::from_parent(
            Dict::from_bytes(b"<</ColorSpace<</DefaultRGB/DeviceRGB>>>>").expect("child resources"),
            pdf.pages()[0].resources().clone(),
        );
        let source = Object::Name(Name::new_unescaped(b"Source"));
        let (child_space, _) =
            ColorSpace::new_image(source.clone(), &cache, &child).expect("child color space");
        let mut actual = [0; 3];
        child_space
            .convert(&[0], &mut actual)
            .expect("child pixels");
        assert_eq!(actual, [0x40, 0x80, 0xc0]);
        parent
            .convert(&[0], &mut actual)
            .expect("parent pixels after child");
        assert_eq!(actual, expected);
        let broken = Resources::from_parent(
            Dict::from_bytes(b"<</ColorSpace<</DefaultRGB 999 0 R>>>>")
                .expect("broken child resources"),
            child,
        );
        assert!(matches!(
            ColorSpace::new_image(source, &cache, &broken),
            Err(ImageColorSpaceError::InvalidDefault("DefaultRGB"))
        ));
    }
}
