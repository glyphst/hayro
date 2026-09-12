use super::form::{
    FormGroupProperties, FormXObject, resources_cache_key, resources_contain_color_space,
};
use crate::color::{Color, ColorComponents, ColorSpace, ColorSpaceKind};
use crate::context::{Context, InterpreterCache};
use crate::device::Device;
use crate::function::{Function, TransferFunction};
use crate::interpret::state::State;
use crate::util::hash128;
use crate::{CacheKey, InterpreterSettings};
use hayro_syntax::object::Name;
use hayro_syntax::object::ObjectIdentifier;
use hayro_syntax::object::Stream;
use hayro_syntax::object::dict::keys::*;
use hayro_syntax::object::{Dict, Object};
use hayro_syntax::page::Resources;
use hayro_syntax::xref::XRef;
use kurbo::Affine;
use smallvec::smallvec;
use std::fmt::Debug;
use std::hash::{Hash, Hasher};
use std::ops::Deref;
use std::rc::Rc;

/// Type type of mask.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MaskType {
    /// A luminosity mask.
    Luminosity,
    /// An alpha mask.
    Alpha,
}

struct Repr<'a> {
    obj_id: ObjectIdentifier,
    retained_key: u128,
    group: FormXObject<'a>,
    mask_type: MaskType,
    parent_resources: Resources<'a>,
    root_transform: Affine,
    bbox: kurbo::Rect,
    interpreter_cache: InterpreterCache<'a>,
    transfer_function: Option<TransferFunction>,
    settings: InterpreterSettings,
    background: Color,
    group_color_space: ColorSpace,
    group_color_space_default_overridden: bool,
    xref: &'a XRef,
    nesting_depth: u32,
}

impl Hash for Repr<'_> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.retained_key.hash(state);
    }
}

/// A soft mask.
#[derive(Clone, Hash)]
pub struct SoftMask<'a>(Rc<Repr<'a>>);

impl Debug for SoftMask<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "SoftMask({:?}, {})", self.0.obj_id, self.0.retained_key)
    }
}

impl PartialEq for SoftMask<'_> {
    fn eq(&self, other: &Self) -> bool {
        self.0.retained_key == other.0.retained_key
    }
}

impl Eq for SoftMask<'_> {}

impl CacheKey for SoftMask<'_> {
    fn cache_key(&self) -> u128 {
        self.0.retained_key
    }
}

impl<'a> SoftMask<'a> {
    pub(crate) fn new(
        dict: &Dict<'a>,
        context: &Context<'a>,
        parent_resources: Resources<'a>,
    ) -> Option<Self> {
        let obj_id = dict.get_ref(G)?.into();
        let group_stream = dict.get::<Stream<'_>>(G)?;
        let group = FormXObject::new(&group_stream)?;
        let properties = group.dict.get::<Dict<'_>>(GROUP)?;
        let mask_type = match dict.get::<Name<'_>>(S)?.deref() {
            LUMINOSITY => MaskType::Luminosity,
            ALPHA => MaskType::Alpha,
            _ => return None,
        };
        let group_resources = Resources::from_parent(
            group.dict.get::<Dict<'_>>(RESOURCES).unwrap_or_default(),
            parent_resources.clone(),
        );
        let cs = if mask_type == MaskType::Alpha && properties.is_null_or_absent(CS) {
            // Table 144 requires CS only for luminosity. This placeholder is
            // never consulted when extracting the source group's alpha.
            ColorSpace::device_gray()
        } else {
            let object = properties.get::<Object<'_>>(CS)?;
            ColorSpace::new(object.clone(), &context.interpreter_cache.object_cache).or_else(
                || {
                    object
                        .into_name()
                        .and_then(|name| group_resources.get_color_space(&name))
                        .and_then(|resolved| {
                            ColorSpace::new(resolved, &context.interpreter_cache.object_cache)
                        })
                },
            )?
        };
        let default_name = match cs.kind() {
            ColorSpaceKind::DeviceGray => Some(DEFAULT_GRAY),
            ColorSpaceKind::DeviceRgb => Some(DEFAULT_RGB),
            ColorSpaceKind::DeviceCmyk => Some(DEFAULT_CMYK),
            _ => None,
        };
        let group_color_space_default_overridden =
            default_name.is_some_and(|name| resources_contain_color_space(&group_resources, name));
        let transfer_function = if dict.is_null_or_absent(TR)
            || dict
                .get::<Name<'_>>(TR)
                .is_some_and(|name| name.as_ref() == b"Identity")
        {
            None
        } else {
            Some(TransferFunction::new(Function::new(
                &dict.get::<Object<'_>>(TR)?,
            )?)?)
        };
        let background = match mask_type {
            MaskType::Luminosity => dict
                .get::<ColorComponents>(BC)
                .map(|c| Color::new(cs.clone(), c, 1.0))
                .unwrap_or_else(|| Color::new(cs.clone(), cs.initial_color(), 1.0)),
            MaskType::Alpha => {
                // Background color attribute should only be used with luminosity masks.
                Color::new(ColorSpace::device_gray(), smallvec![0.0], 1.0)
            }
        };
        let nesting_depth = context.nesting_depth() + 1;
        // G alone does not identify a mask: S, BC, TR, inherited resources,
        // and the transform at gs all affect the result. Keep equality,
        // hashing, and inherited-state debug keys on the same identity.
        let retained_key = hash128(&(
            dict.cache_key(),
            resources_cache_key(&group_resources),
            context.get().ctm.cache_key(),
            context.bbox().cache_key(),
            context.settings.defer_transfer_functions,
        ));

        Some(Self(Rc::new(Repr {
            obj_id,
            retained_key,
            group,
            mask_type,
            root_transform: context.get().ctm,
            transfer_function,
            bbox: context.bbox(),
            interpreter_cache: context.interpreter_cache.clone(),
            settings: context.settings.clone(),
            xref: context.xref,
            background,
            group_color_space: cs,
            group_color_space_default_overridden,
            parent_resources,
            nesting_depth,
        })))
    }

    /// Interpret the contents of the mask into the given device.
    pub fn interpret(&self, device: &mut impl Device<'a>) {
        let state = State::new(self.0.root_transform);
        let mut ctx = Context::new_with(
            self.0.root_transform,
            self.0.bbox,
            &self.0.interpreter_cache,
            self.0.xref,
            self.0.settings.clone(),
            state,
            self.0.nesting_depth,
        );
        self.0
            .group
            .draw(&self.0.parent_resources, &mut ctx, device);
    }

    /// Return the object identifier of the mask's source Form.
    ///
    /// Different masks can share this Form. Use [`CacheKey::cache_key`] for
    /// identity that includes mask properties and invocation state.
    pub fn id(&self) -> ObjectIdentifier {
        self.0.obj_id
    }

    /// Return the underlying mask type.
    pub fn mask_type(&self) -> MaskType {
        self.0.mask_type
    }

    /// Typed properties of the transparency-group Form used as the mask source.
    pub fn group_properties(&self) -> Option<FormGroupProperties> {
        self.0.group.group_properties
    }

    /// The background color against which the mask should be composited.
    pub fn background_color(&self) -> Color {
        self.0.background.clone()
    }

    /// Return the soft-mask transparency group's blending color-space family.
    pub fn group_color_space_kind(&self) -> ColorSpaceKind {
        self.0.group_color_space.kind()
    }

    /// Parsed blending color space of the soft-mask source group.
    pub fn group_color_space(&self) -> &ColorSpace {
        &self.0.group_color_space
    }

    /// Whether a default device-space resource remaps the resolved mask group
    /// color space, including a device space reached through a resource name.
    pub fn group_color_space_is_default_overridden(&self) -> bool {
        self.0.group_color_space_default_overridden
    }

    /// Return the transfer function that should be used for the mask.
    pub fn transfer_function(&self) -> Option<&TransferFunction> {
        self.0.transfer_function.as_ref()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hayro_syntax::Pdf;
    use kurbo::Rect;

    #[test]
    fn soft_mask_identity_and_null_graphics_state_preserve_invocation_semantics() {
        let pdf = Pdf::new(
            b"%PDF-1.7
1 0 obj << /Type /Catalog /Pages 2 0 R >> endobj
2 0 obj << /Type /Pages /Kids [3 0 R] /Count 1 >> endobj
3 0 obj << /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100]
/Resources << /ColorSpace << /Blend /DeviceGray >> /ExtGState <<
/A << /SMask << /S /Alpha /G 4 0 R >> /BM /Multiply /RI /Perceptual >>
/B << /SMask << /S /Luminosity /G 4 0 R >> >>
/N << /SMask null /BM null /RI null >>
/U << /SMask 999 0 R /BM 999 0 R /RI 999 0 R >>
>> >> /Other << /ColorSpace << /Blend /DeviceRGB >> >> >> endobj
4 0 obj << /Type /XObject /Subtype /Form /BBox [0 0 100 100]
/Group << /S /Transparency /CS /Blend >> /Length 0 >> stream

endstream endobj
trailer << /Root 1 0 R >>
%%EOF"
                .to_vec(),
        )
        .expect("PDF");
        let resources = pdf.pages()[0].resources().clone();
        let states = &resources.ext_g_states;
        let cache = InterpreterCache::new();
        let mut context = Context::new(
            Affine::IDENTITY,
            Rect::new(0.0, 0.0, 100.0, 100.0),
            &cache,
            pdf.xref(),
            InterpreterSettings::default(),
        );
        let dict = states.get::<Dict<'_>>(b"A").unwrap();
        crate::interpret::state::handle_gs(&dict, &mut context, &resources);
        let first = context.get().graphics_state.soft_mask.clone().unwrap();
        for name in [b"N", b"U"] {
            let dict = states.get::<Dict<'_>>(name).unwrap();
            crate::interpret::state::handle_gs(&dict, &mut context, &resources);
            let state = &context.get().graphics_state;
            assert_eq!(state.soft_mask.as_ref(), Some(&first));
            assert_eq!(state.blend_mode, crate::BlendMode::Multiply);
            assert_eq!(
                state.rendering_intent,
                crate::color::RenderingIntent::Perceptual
            );
        }
        let a = dict.get::<Dict<'_>>(SMASK).unwrap();
        let same = SoftMask::new(&a, &context, resources.clone()).unwrap();
        assert_eq!(first, same);
        assert_eq!(hash128(&first), hash128(&same));
        assert_eq!(format!("{first:?}"), format!("{same:?}"));
        let b = states
            .get::<Dict<'_>>(b"B")
            .unwrap()
            .get::<Dict<'_>>(SMASK)
            .unwrap();
        let different_mode = SoftMask::new(&b, &context, resources.clone()).unwrap();
        assert_eq!(first.id(), different_mode.id());
        assert_ne!(first, different_mode);
        assert_ne!(first.cache_key(), different_mode.cache_key());
        let other = Resources::from_parent(
            pdf.pages()[0].raw().get::<Dict<'_>>(b"Other").unwrap(),
            resources.clone(),
        );
        let different_resources = SoftMask::new(&a, &context, other).unwrap();
        assert_ne!(first, different_resources);
        context.get_mut().ctm = Affine::translate((10.0, 20.0));
        let translated = SoftMask::new(&a, &context, resources).unwrap();
        assert_ne!(first, translated);
        assert_ne!(first.cache_key(), translated.cache_key());
    }
}
