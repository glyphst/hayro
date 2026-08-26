use crate::{OptionalContentExpression, OptionalContentGroup};
use hayro_syntax::object::dict::keys::{
    BASE_STATE, D, OCG, OCGS, OCMD, OCPROPERTIES, OFF, ON, P, TYPE, VE,
};
use hayro_syntax::object::{Array, Dict, MaybeRef, Name, Object, ObjectIdentifier};
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

    pub(crate) fn begin_single_oc(
        &mut self,
        ocg_id: ObjectIdentifier,
    ) -> OptionalContentExpression {
        let expression = self.group_expression(ocg_id);
        self.push_optional(expression_is_visible(&expression), true);
        expression
    }

    pub(crate) fn begin_ocmd(
        &mut self,
        ocmd: &Dict<'_>,
        xref: &XRef,
    ) -> Option<OptionalContentExpression> {
        let expression = if let Some(visibility_expression) = ocmd.get::<Array<'_>>(VE) {
            self.parse_visibility_expression(&visibility_expression, xref, 0)
        } else {
            self.policy_expression(ocmd)
        };
        let visible = expression.as_ref().is_none_or(expression_is_visible);
        self.push_optional(visible, expression.is_some());
        expression
    }

    pub(crate) fn begin_ocg(
        &mut self,
        props: &Dict<'_>,
        ref_id: ObjectIdentifier,
        xref: &XRef,
    ) -> Option<OptionalContentExpression> {
        match props.get::<Name<'_>>(TYPE).as_deref() {
            Some(OCMD) => self.begin_ocmd(props, xref),
            _ => Some(self.begin_single_oc(ref_id)),
        }
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

    fn group_expression(&self, id: ObjectIdentifier) -> OptionalContentExpression {
        OptionalContentExpression::Group(OptionalContentGroup {
            object_number: id.obj_number,
            generation_number: id.gen_number,
            initially_visible: !self.inactive_ocgs.contains(&id),
        })
    }

    fn policy_expression(&self, ocmd: &Dict<'_>) -> Option<OptionalContentExpression> {
        let policy = ocmd
            .get::<Name<'_>>(P)
            .and_then(|n| OcmdPolicy::from_name(n.as_ref()))
            .unwrap_or(OcmdPolicy::AnyOn);
        let groups = if let Some(arr) = ocmd.get::<Array<'_>>(OCGS) {
            let mut groups = Vec::new();
            for item in arr.raw_iter() {
                groups.push(self.group_expression(item.as_obj_ref()?.into()));
            }
            groups
        } else if let Some(ref_) = ocmd.get_ref(OCGS) {
            vec![self.group_expression(ref_.into())]
        } else if ocmd.contains_key(OCGS) {
            return None;
        } else {
            Vec::new()
        };

        if groups.is_empty() {
            return Some(OptionalContentExpression::All(Vec::new()));
        }
        Some(match policy {
            OcmdPolicy::AllOn => OptionalContentExpression::All(groups),
            OcmdPolicy::AnyOn => OptionalContentExpression::Any(groups),
            OcmdPolicy::AnyOff => OptionalContentExpression::Any(
                groups
                    .into_iter()
                    .map(|group| OptionalContentExpression::Not(Box::new(group)))
                    .collect(),
            ),
            OcmdPolicy::AllOff => OptionalContentExpression::All(
                groups
                    .into_iter()
                    .map(|group| OptionalContentExpression::Not(Box::new(group)))
                    .collect(),
            ),
        })
    }

    fn parse_visibility_expression(
        &self,
        array: &Array<'_>,
        xref: &XRef,
        depth: u32,
    ) -> Option<OptionalContentExpression> {
        if depth >= MAX_VISIBILITY_EXPRESSION_DEPTH {
            return None;
        }
        let mut items = array.raw_iter();
        let operator = match items.next()? {
            MaybeRef::NotRef(Object::Name(name)) => name,
            _ => return None,
        };
        let operands = items
            .map(|item| self.parse_visibility_operand(item, xref, depth + 1))
            .collect::<Option<Vec<_>>>()?;
        match operator.as_ref() {
            b"And" => Some(OptionalContentExpression::All(operands)),
            b"Or" => Some(OptionalContentExpression::Any(operands)),
            b"Not" if operands.len() == 1 => operands
                .into_iter()
                .next()
                .map(|operand| OptionalContentExpression::Not(Box::new(operand))),
            _ => None,
        }
    }

    fn parse_visibility_operand(
        &self,
        operand: MaybeRef<Object<'_>>,
        xref: &XRef,
        depth: u32,
    ) -> Option<OptionalContentExpression> {
        if depth >= MAX_VISIBILITY_EXPRESSION_DEPTH {
            return None;
        }
        match operand {
            MaybeRef::Ref(reference) => {
                let id: ObjectIdentifier = reference.into();
                match xref.get::<Object<'_>>(id)? {
                    Object::Array(array) => self.parse_visibility_expression(&array, xref, depth),
                    Object::Dict(dict)
                        if dict
                            .get::<Name<'_>>(TYPE)
                            .is_some_and(|name| name.as_ref() == OCMD) =>
                    {
                        if let Some(array) = dict.get::<Array<'_>>(VE) {
                            self.parse_visibility_expression(&array, xref, depth)
                        } else {
                            self.policy_expression(&dict)
                        }
                    }
                    Object::Dict(dict)
                        if dict
                            .get::<Name<'_>>(TYPE)
                            .is_none_or(|name| name.as_ref() == OCG) =>
                    {
                        Some(self.group_expression(id))
                    }
                    _ => None,
                }
            }
            MaybeRef::NotRef(Object::Array(array)) => {
                self.parse_visibility_expression(&array, xref, depth)
            }
            _ => None,
        }
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
    use hayro_syntax::object::FromBytes;

    fn id(number: i32) -> ObjectIdentifier {
        ObjectIdentifier::new(number, 0)
    }

    #[test]
    fn retained_mode_emits_hidden_groups_without_losing_the_default() {
        let mut filtered = OcgState::dummy(false);
        filtered.inactive_ocgs.insert(id(7));
        let filtered_expression = filtered.begin_single_oc(id(7));
        assert!(!filtered.is_visible());
        assert!(!expression_is_visible(&filtered_expression));

        let mut retained = OcgState::dummy(true);
        retained.inactive_ocgs.insert(id(7));
        let retained_expression = retained.begin_single_oc(id(7));
        assert!(retained.is_visible());
        assert!(!expression_is_visible(&retained_expression));
        assert!(retained.end_marked_content());
    }

    #[test]
    fn ocmd_policies_become_owned_boolean_expressions() {
        let dict = Dict::from_bytes(b"<< /Type /OCMD /OCGs [4 0 R 5 0 R] /P /AllOff >>")
            .expect("valid OCMD dictionary");
        let mut state = OcgState::dummy(true);
        state.inactive_ocgs.insert(id(4));
        state.inactive_ocgs.insert(id(5));
        let expression = state
            .policy_expression(&dict)
            .expect("owned policy expression");
        assert!(expression_is_visible(&expression));
        assert!(matches!(expression, OptionalContentExpression::All(_)));
    }
}
