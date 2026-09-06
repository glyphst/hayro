use super::{MAX_VISIBILITY_EXPRESSION_DEPTH, OcmdPolicy};
use crate::{OptionalContentExpression, OptionalContentGroup};
use hayro_syntax::object::dict::keys::OC;
use hayro_syntax::object::dict::keys::{OCG, OCGS, OCMD, OCPROPERTIES, P, TYPE, VE};
use hayro_syntax::object::{Array, Dict, MaybeRef, Name, Object, ObjectIdentifier};
use hayro_syntax::xref::XRef;
use std::mem::size_of;

#[cfg(test)]
mod tests;

/// Remaining work for owned optional-content expressions. Reuse one budget
/// across annotations so repeated references cannot multiply the allowance.
#[derive(Clone, Copy, Debug)]
pub struct OptionalContentBudget {
    /// Remaining visited expression nodes, including repeated references.
    pub nodes: u64,
    /// Remaining temporary and owned expression bytes.
    pub bytes: u64,
    /// Maximum expression depth, additionally capped at 64 for stack safety.
    pub max_depth: u32,
}

impl Default for OptionalContentBudget {
    fn default() -> Self {
        Self {
            nodes: 65_536,
            bytes: 64 * 1024 * 1024,
            max_depth: 64,
        }
    }
}

/// An optional-content entry could not be retained exactly.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum OptionalContentError {
    /// Missing, unresolved, wrongly typed, or cyclic membership data.
    InvalidMembership,
    /// Invalid policy name or visibility-expression operator/operands.
    InvalidExpression,
    /// The expression exceeds the host's nesting allowance.
    DepthLimit,
    /// Aggregate expression work exceeds the host's node allowance.
    NodeLimit,
    /// Aggregate storage or a fallible allocation exceeds its allowance.
    ByteLimit,
    /// The host cancelled resolution.
    Cancelled,
}

/// Resolve a dictionary's `/OC` into an owned expression. Group leaves have
/// `initially_visible = true`; the host applies its effective catalog state.
/// With no catalog `/OCProperties`, PDF requires ignoring the entire entry,
/// even if it is malformed. A present invalid catalog is the host's error.
pub fn resolve_optional_content(
    dictionary: &Dict<'_>,
    xref: &XRef,
    budget: &mut OptionalContentBudget,
    cancelled: &dyn Fn() -> bool,
) -> Result<Option<OptionalContentExpression>, OptionalContentError> {
    if cancelled() {
        return Err(OptionalContentError::Cancelled);
    }
    if !dictionary.contains_key(OC)
        || xref
            .get::<Dict<'_>>(xref.root_id())
            .is_none_or(|c| !c.contains_key(OCPROPERTIES))
    {
        return Ok(None);
    }
    let membership = dictionary
        .get::<Dict<'_>>(OC)
        .ok_or(OptionalContentError::InvalidMembership)?;
    resolve_membership(
        &membership,
        dictionary.get_ref(OC).map(Into::into),
        xref,
        budget,
        cancelled,
    )
    .map(Some)
}

pub(super) fn resolve_membership(
    dictionary: &Dict<'_>,
    reference: Option<ObjectIdentifier>,
    xref: &XRef,
    budget: &mut OptionalContentBudget,
    cancelled: &dyn Fn() -> bool,
) -> Result<OptionalContentExpression, OptionalContentError> {
    let mut resolver = Resolver {
        xref,
        budget,
        cancelled,
        active: Vec::new(),
    };
    if let Some(id) = reference {
        resolver.enter_reference(id)?;
    }
    resolver.membership(dictionary, reference, 0)
}

struct Resolver<'a> {
    xref: &'a XRef,
    budget: &'a mut OptionalContentBudget,
    cancelled: &'a dyn Fn() -> bool,
    active: Vec<ObjectIdentifier>,
}

type Error = OptionalContentError;
type Expression = OptionalContentExpression;

impl Resolver<'_> {
    fn charge(&mut self, depth: u32) -> Result<(), Error> {
        if (self.cancelled)() {
            return Err(Error::Cancelled);
        }
        if depth >= self.budget.max_depth.min(MAX_VISIBILITY_EXPRESSION_DEPTH) {
            return Err(Error::DepthLimit);
        }
        self.budget.nodes = self.budget.nodes.checked_sub(1).ok_or(Error::NodeLimit)?;
        // Covers geometric vector growth, boxes, the reference stack, and
        // the host's simultaneously retained converted expression.
        self.budget.bytes = self
            .budget
            .bytes
            .checked_sub(4 * size_of::<Expression>() as u64 + size_of::<ObjectIdentifier>() as u64)
            .ok_or(Error::ByteLimit)?;
        Ok(())
    }

    fn enter_reference(&mut self, id: ObjectIdentifier) -> Result<(), Error> {
        if (self.cancelled)() {
            return Err(Error::Cancelled);
        }
        if self.budget.nodes == 0 {
            return Err(Error::NodeLimit);
        }
        if self.budget.bytes < size_of::<ObjectIdentifier>() as u64 {
            return Err(Error::ByteLimit);
        }
        if self.active.contains(&id) {
            return Err(Error::InvalidMembership);
        }
        self.active
            .try_reserve_exact(1)
            .map_err(|_| Error::ByteLimit)?;
        self.active.push(id);
        Ok(())
    }

    fn append(values: &mut Vec<Expression>, value: Expression) -> Result<(), Error> {
        if values.len() == values.capacity() {
            values
                .try_reserve_exact(values.capacity().max(1))
                .map_err(|_| Error::ByteLimit)?;
        }
        values.push(value);
        Ok(())
    }

    fn group(&mut self, id: ObjectIdentifier, depth: u32) -> Result<Expression, Error> {
        self.charge(depth)?;
        if id.obj_number <= 0 || id.gen_number < 0 {
            return Err(Error::InvalidMembership);
        }
        Ok(Expression::Group(OptionalContentGroup {
            object_number: id.obj_number,
            generation_number: id.gen_number,
            initially_visible: true,
        }))
    }

    fn membership(
        &mut self,
        dict: &Dict<'_>,
        id: Option<ObjectIdentifier>,
        depth: u32,
    ) -> Result<Expression, Error> {
        match dict.get::<Name<'_>>(TYPE).as_deref() {
            Some(OCG) => self.group(id.ok_or(Error::InvalidMembership)?, depth),
            Some(OCMD) => {
                if dict.contains_key(VE) {
                    let array = dict.get::<Array<'_>>(VE).ok_or(Error::InvalidExpression)?;
                    self.expression(&array, depth)
                } else {
                    self.policy(dict, depth)
                }
            }
            _ => Err(Error::InvalidMembership),
        }
    }

    fn policy(&mut self, dict: &Dict<'_>, depth: u32) -> Result<Expression, Error> {
        self.charge(depth)?;
        let policy = match dict.get::<Name<'_>>(P) {
            Some(name) => OcmdPolicy::from_name(name.as_ref()).ok_or(Error::InvalidExpression)?,
            None if dict.contains_key(P) => return Err(Error::InvalidExpression),
            None => OcmdPolicy::AnyOn,
        };
        let mut groups = Vec::new();
        let mut add = |id: ObjectIdentifier| -> Result<(), Error> {
            let dict = self
                .xref
                .get::<Dict<'_>>(id)
                .ok_or(Error::InvalidMembership)?;
            if dict.get::<Name<'_>>(TYPE).as_deref() != Some(OCG) {
                return Err(Error::InvalidMembership);
            }
            let invert = matches!(policy, OcmdPolicy::AnyOff | OcmdPolicy::AllOff);
            if invert {
                self.charge(depth + 1)?;
            }
            let group = self.group(id, depth + 1 + u32::from(invert))?;
            Self::append(
                &mut groups,
                if invert {
                    Expression::Not(Box::new(group))
                } else {
                    group
                },
            )
        };
        if let Some(array) = dict.get::<Array<'_>>(OCGS) {
            for item in array.raw_iter() {
                add(item.as_obj_ref().ok_or(Error::InvalidMembership)?.into())?;
            }
        } else if let Some(id) = dict.get_ref(OCGS) {
            add(id.into())?;
        } else if dict.contains_key(OCGS) {
            return Err(Error::InvalidMembership);
        }
        if groups.is_empty() {
            return Ok(Expression::All(groups));
        }
        Ok(match policy {
            OcmdPolicy::AllOn | OcmdPolicy::AllOff => Expression::All(groups),
            OcmdPolicy::AnyOn | OcmdPolicy::AnyOff => Expression::Any(groups),
        })
    }

    fn expression(&mut self, array: &Array<'_>, depth: u32) -> Result<Expression, Error> {
        self.charge(depth)?;
        let mut items = array.raw_iter();
        let operator = match items.next() {
            Some(MaybeRef::NotRef(Object::Name(name))) => name,
            _ => return Err(Error::InvalidExpression),
        };
        if !matches!(operator.as_ref(), b"And" | b"Or" | b"Not") {
            return Err(Error::InvalidExpression);
        }
        let mut operands = Vec::new();
        for item in items {
            if operator.as_ref() == b"Not" && !operands.is_empty() {
                return Err(Error::InvalidExpression);
            }
            let operand = self.operand(item, depth + 1)?;
            Self::append(&mut operands, operand)?;
        }
        if operands.is_empty() {
            return Err(Error::InvalidExpression);
        }
        Ok(match operator.as_ref() {
            b"And" => Expression::All(operands),
            b"Or" => Expression::Any(operands),
            _ => Expression::Not(Box::new(operands.pop().ok_or(Error::InvalidExpression)?)),
        })
    }

    fn operand(&mut self, value: MaybeRef<Object<'_>>, depth: u32) -> Result<Expression, Error> {
        match value {
            MaybeRef::NotRef(Object::Array(array)) => self.expression(&array, depth),
            MaybeRef::Ref(reference) => {
                // Check depth before following references, including aliases.
                if depth >= self.budget.max_depth.min(MAX_VISIBILITY_EXPRESSION_DEPTH) {
                    return Err(Error::DepthLimit);
                }
                let id = reference.into();
                self.enter_reference(id)?;
                let result = match self.xref.get::<Object<'_>>(id) {
                    Some(Object::Array(array)) => self.expression(&array, depth),
                    Some(Object::Dict(dict)) => self.membership(&dict, Some(id), depth),
                    _ => Err(Error::InvalidMembership),
                };
                self.active.pop();
                result
            }
            _ => Err(Error::InvalidExpression),
        }
    }
}
