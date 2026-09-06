mod resolve;

pub use resolve::{OptionalContentBudget, OptionalContentError, resolve_optional_content};

use crate::OptionalContentExpression;
use hayro_syntax::object::dict::keys::{BASE_STATE, D, OCGS, OCPROPERTIES, OFF, ON};
use hayro_syntax::object::{Array, Dict, Name, ObjectIdentifier};
use hayro_syntax::xref::XRef;
use std::collections::HashSet;

const MAX_VISIBILITY_EXPRESSION_DEPTH: u32 = 64;

struct VisibilityFrame {
    visible: bool,
    emit_optional_callback: bool,
}

pub(crate) struct OcgState {
    inactive_ocgs: HashSet<ObjectIdentifier>,
    visibility_stack: Vec<VisibilityFrame>,
    preserve_optional_content: bool,
}

impl OcgState {
    fn dummy(preserve_optional_content: bool) -> Self {
        Self {
            inactive_ocgs: HashSet::default(),
            visibility_stack: vec![],
            preserve_optional_content,
        }
    }

    pub(crate) fn from_catalog(catalog: &Dict<'_>, preserve_optional_content: bool) -> Self {
        let Some(oc_properties) = catalog.get::<Dict<'_>>(OCPROPERTIES) else {
            return Self::dummy(preserve_optional_content);
        };

        let Some(config) = oc_properties.get::<Dict<'_>>(D) else {
            return Self::dummy(preserve_optional_content);
        };

        let mut inactive = HashSet::new();

        let base_state = config
            .get::<Name<'_>>(BASE_STATE)
            .and_then(|b| BaseState::from_name(b.as_ref()));

        if base_state.unwrap_or(BaseState::On) == BaseState::Off
            && let Some(ocgs) = oc_properties.get::<Array<'_>>(OCGS)
        {
            for item in ocgs.raw_iter() {
                if let Some(ref_) = item.as_obj_ref() {
                    let id: ObjectIdentifier = ref_.into();
                    inactive.insert(id);
                }
            }
        }

        let mut read_ocg_array = |key, insert_active: bool| {
            if let Some(arr) = config.get::<Array<'_>>(key) {
                for item in arr.raw_iter() {
                    if let Some(ref_) = item.as_obj_ref() {
                        let id: ObjectIdentifier = ref_.into();
                        if insert_active {
                            inactive.remove(&id);
                        } else {
                            inactive.insert(id);
                        }
                    }
                }
            }
        };

        read_ocg_array(ON, true);
        read_ocg_array(OFF, false);

        Self {
            inactive_ocgs: inactive,
            visibility_stack: Vec::new(),
            preserve_optional_content,
        }
    }

    pub(crate) fn begin_ocmd(
        &mut self,
        ocmd: &Dict<'_>,
        xref: &XRef,
    ) -> Option<OptionalContentExpression> {
        self.begin_membership(ocmd, None, xref)
    }

    pub(crate) fn begin_ocg(
        &mut self,
        props: &Dict<'_>,
        ref_id: ObjectIdentifier,
        xref: &XRef,
    ) -> Option<OptionalContentExpression> {
        self.begin_membership(props, Some(ref_id), xref)
    }

    fn begin_membership(
        &mut self,
        props: &Dict<'_>,
        ref_id: Option<ObjectIdentifier>,
        xref: &XRef,
    ) -> Option<OptionalContentExpression> {
        match resolve::resolve_membership(
            props,
            ref_id,
            xref,
            &mut OptionalContentBudget::default(),
            &|| false,
        ) {
            Ok(expression) => Some(self.begin_resolved(expression)),
            Err(_) => {
                self.begin_unresolved_optional_content();
                None
            }
        }
    }

    pub(crate) fn begin_resolved(
        &mut self,
        mut expression: OptionalContentExpression,
    ) -> OptionalContentExpression {
        fn apply_defaults(
            expression: &mut OptionalContentExpression,
            inactive: &HashSet<ObjectIdentifier>,
        ) {
            match expression {
                OptionalContentExpression::Group(group) => {
                    group.initially_visible = !inactive.contains(&ObjectIdentifier {
                        obj_number: group.object_number,
                        gen_number: group.generation_number,
                    });
                }
                OptionalContentExpression::All(operands)
                | OptionalContentExpression::Any(operands) => {
                    for operand in operands {
                        apply_defaults(operand, inactive);
                    }
                }
                OptionalContentExpression::Not(operand) => apply_defaults(operand, inactive),
            }
        }
        apply_defaults(&mut expression, &self.inactive_ocgs);
        self.push_optional(expression_is_visible(&expression), true);
        expression
    }

    pub(crate) fn begin_marked_content(&mut self) {
        let visible = self.effective_visibility();
        self.visibility_stack.push(VisibilityFrame {
            visible,
            emit_optional_callback: false,
        });
    }

    pub(crate) fn begin_unresolved_optional_content(&mut self) {
        self.push_optional(true, false);
    }

    pub(crate) fn end_marked_content(&mut self) -> bool {
        self.visibility_stack
            .pop()
            .is_some_and(|frame| frame.emit_optional_callback)
    }

    pub(crate) fn marked_content_depth(&self) -> usize {
        self.visibility_stack.len()
    }

    pub(crate) fn is_visible(&self) -> bool {
        self.preserve_optional_content || self.effective_visibility()
    }

    fn effective_visibility(&self) -> bool {
        self.visibility_stack
            .last()
            .is_none_or(|frame| frame.visible)
    }

    fn push_optional(&mut self, expression_visible: bool, expression_is_owned: bool) {
        let visible = self.effective_visibility() && expression_visible;
        self.visibility_stack.push(VisibilityFrame {
            visible,
            emit_optional_callback: self.preserve_optional_content && expression_is_owned,
        });
    }
}

impl Default for OcgState {
    fn default() -> Self {
        Self::dummy(false)
    }
}

fn expression_is_visible(expression: &OptionalContentExpression) -> bool {
    match expression {
        OptionalContentExpression::Group(group) => group.initially_visible,
        OptionalContentExpression::All(operands) => operands.iter().all(expression_is_visible),
        OptionalContentExpression::Any(operands) => operands.iter().any(expression_is_visible),
        OptionalContentExpression::Not(operand) => !expression_is_visible(operand),
    }
}

#[derive(Debug, PartialEq, Eq, Copy, Clone)]
enum BaseState {
    On,
    Off,
    Unchanged,
}

impl BaseState {
    fn from_name(name: &[u8]) -> Option<Self> {
        match name {
            b"ON" => Some(Self::On),
            b"OFF" => Some(Self::Off),
            b"Unchanged" => Some(Self::Unchanged),
            _ => None,
        }
    }
}

#[derive(Debug, PartialEq, Eq, Copy, Clone)]
enum OcmdPolicy {
    AllOn,
    AnyOn,
    AnyOff,
    AllOff,
}

impl OcmdPolicy {
    fn from_name(name: &[u8]) -> Option<Self> {
        match name {
            b"AllOn" => Some(Self::AllOn),
            b"AnyOn" => Some(Self::AnyOn),
            b"AnyOff" => Some(Self::AnyOff),
            b"AllOff" => Some(Self::AllOff),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn group(number: i32) -> OptionalContentExpression {
        OptionalContentExpression::Group(crate::OptionalContentGroup {
            object_number: number,
            generation_number: 0,
            initially_visible: true,
        })
    }

    fn id(number: i32) -> ObjectIdentifier {
        ObjectIdentifier::new(number, 0)
    }

    #[test]
    fn retained_mode_emits_hidden_groups_without_losing_the_default() {
        let mut filtered = OcgState::dummy(false);
        filtered.inactive_ocgs.insert(id(7));
        let filtered_expression = filtered.begin_resolved(group(7));
        assert!(!filtered.is_visible());
        assert!(!expression_is_visible(&filtered_expression));

        let mut retained = OcgState::dummy(true);
        retained.inactive_ocgs.insert(id(7));
        let retained_expression = retained.begin_resolved(group(7));
        assert!(retained.is_visible());
        assert!(!expression_is_visible(&retained_expression));
        assert!(retained.end_marked_content());
    }
}
