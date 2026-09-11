//! Arrays.

use crate::object::macros::object;
use crate::object::r#ref::MaybeRef;
use crate::object::{FromBytes, Object, ObjectLike, ObjectReadError};
use crate::reader::Reader;
use crate::reader::{Readable, ReaderContext, ReaderExt, Skippable};
use alloc::vec::Vec;
use core::fmt::{Debug, Display, Formatter};
use core::marker::PhantomData;
use smallvec::SmallVec;

/// An array of PDF objects.
#[derive(Clone)]
pub struct Array<'a> {
    data: &'a [u8],
    ctx: ReaderContext<'a>,
}

// Note that this is not structural equality, i.e. two arrays with the same
// items are still considered different if they have different whitespaces.
impl PartialEq for Array<'_> {
    fn eq(&self, other: &Self) -> bool {
        self.data == other.data
    }
}

impl<'a> Array<'a> {
    /// Returns an iterator over the objects of the array.
    ///
    /// Unreadable values yield errors and retain their positions. Iteration
    /// continues with the following value; nested containers remain lazy.
    pub fn raw_iter(&self) -> ArrayIter<'a> {
        ArrayIter::new(self.data, &self.ctx)
    }

    /// Returns an iterator over the resolved objects of the array.
    #[allow(
        private_bounds,
        reason = "users shouldn't be able to implement `ObjectLike` for custom objects."
    )]
    pub fn iter<T>(&self) -> ResolvedArrayIter<'a, T>
    where
        T: ObjectLike<'a>,
    {
        ResolvedArrayIter::new(self.data, &self.ctx)
    }

    /// Return a flex iterator over the items in the array.
    pub fn flex_iter(&self) -> FlexArrayIter<'a> {
        FlexArrayIter::new(self.data, &self.ctx)
    }

    /// Return the raw data of the array.
    pub fn data(&self) -> &'a [u8] {
        self.data
    }
}

impl Debug for Array<'_> {
    fn fmt(&self, f: &mut Formatter<'_>) -> core::fmt::Result {
        let mut debug_list = f.debug_list();

        self.raw_iter().for_each(|i| {
            match i {
                Ok(value) => debug_list.entry(&value),
                Err(error) => debug_list.entry(&error),
            };
        });

        debug_list.finish()
    }
}

impl Display for Array<'_> {
    fn fmt(&self, f: &mut Formatter<'_>) -> core::fmt::Result {
        f.write_str("[")?;
        for (index, item) in self.raw_iter().enumerate() {
            if index > 0 {
                f.write_str(" ")?;
            }
            match item {
                Ok(value) => Display::fmt(&value, f)?,
                Err(error) => write!(f, "<{error}>")?,
            }
        }
        f.write_str("]")
    }
}

object!(Array<'a>, Array);

impl Skippable for Array<'_> {
    fn skip(r: &mut Reader<'_>, is_content_stream: bool) -> Option<()> {
        r.forward_tag(b"[")?;

        loop {
            r.skip_white_spaces_and_comments();

            if let Some(()) = r.forward_tag(b"]") {
                return Some(());
            } else if is_content_stream {
                r.skip::<Object<'_>>(true)?;
            } else {
                r.skip::<MaybeRef<Object<'_>>>(false)?;
            }
        }
    }
}

impl Default for Array<'_> {
    fn default() -> Self {
        Self::from_bytes(b"[]").unwrap()
    }
}

impl<'a> Readable<'a> for Array<'a> {
    fn read(r: &mut Reader<'a>, ctx: &ReaderContext<'a>) -> Option<Self> {
        let bytes = r.skip::<Array<'_>>(ctx.in_content_stream())?;

        Some(Self {
            data: &bytes[1..bytes.len() - 1],
            ctx: ctx.clone(),
        })
    }
}

/// An iterator over the items of an array.
pub struct ArrayIter<'a> {
    reader: Reader<'a>,
    ctx: ReaderContext<'a>,
}

impl<'a> ArrayIter<'a> {
    fn new(data: &'a [u8], ctx: &ReaderContext<'a>) -> Self {
        Self {
            reader: Reader::new(data),
            ctx: ctx.clone(),
        }
    }
}

impl<'a> Iterator for ArrayIter<'a> {
    type Item = Result<MaybeRef<Object<'a>>, ObjectReadError>;

    fn next(&mut self) -> Option<Self::Item> {
        self.reader.skip_white_spaces_and_comments();

        if !self.reader.at_end() {
            let offset = self.reader.offset();
            if let Some(item) = self
                .reader
                .read_with_context::<MaybeRef<Object<'_>>>(&self.ctx)
            {
                return Some(Ok(item));
            }
            // The failed read restores the cursor. Skip the indexed lexical
            // object without decoding it so the error cannot hide the tail.
            if self
                .reader
                .skip::<MaybeRef<Object<'_>>>(self.ctx.in_content_stream())
                .is_none()
            {
                self.reader.jump_to_end();
            }
            return Some(Err(ObjectReadError { offset }));
        }

        None
    }
}

impl core::iter::FusedIterator for ArrayIter<'_> {}

/// An iterator over the array that resolves objects of a specific type.
pub struct ResolvedArrayIter<'a, T> {
    flex_iter: FlexArrayIter<'a>,
    phantom_data: PhantomData<T>,
}

impl<'a, T> ResolvedArrayIter<'a, T> {
    fn new(data: &'a [u8], ctx: &ReaderContext<'a>) -> Self {
        Self {
            flex_iter: FlexArrayIter::new(data, ctx),
            phantom_data: PhantomData,
        }
    }
}

impl<'a, T> Iterator for ResolvedArrayIter<'a, T>
where
    T: ObjectLike<'a>,
{
    type Item = T;

    fn next(&mut self) -> Option<Self::Item> {
        self.flex_iter.next::<T>()
    }
}

/// An iterator over the array that allows reading a different object each time.
pub struct FlexArrayIter<'a> {
    reader: Reader<'a>,
    ctx: ReaderContext<'a>,
}

impl<'a> FlexArrayIter<'a> {
    fn new(data: &'a [u8], ctx: &ReaderContext<'a>) -> Self {
        Self {
            reader: Reader::new(data),
            ctx: ctx.clone(),
        }
    }

    fn next_checked<T: ObjectLike<'a>>(&mut self) -> Option<Result<T, ()>> {
        self.reader.skip_white_spaces_and_comments();
        if self.reader.at_end() {
            None
        } else {
            Some(self.next::<T>().ok_or(()))
        }
    }

    #[allow(
        private_bounds,
        reason = "users shouldn't be able to implement `ObjectLike` for custom objects."
    )]
    #[allow(clippy::should_implement_trait)]
    /// Try reading the next item as a specific object from the array.
    pub fn next<T: ObjectLike<'a>>(&mut self) -> Option<T> {
        self.reader.skip_white_spaces_and_comments();

        if !self.reader.at_end() {
            return match self.reader.read_with_context::<MaybeRef<T>>(&self.ctx)? {
                MaybeRef::Ref(r) => self.ctx.xref().get_with::<T>(r.into(), &self.ctx),
                MaybeRef::NotRef(i) => Some(i),
            };
        }

        None
    }
}

impl<'a, T: ObjectLike<'a> + Copy + Default, const C: usize> TryFrom<Array<'a>> for [T; C] {
    type Error = ();

    fn try_from(value: Array<'a>) -> Result<Self, Self::Error> {
        let mut iter = value.flex_iter();

        let mut val = [T::default(); C];

        for i in 0..C {
            val[i] = iter.next_checked::<T>().ok_or(())??;
        }

        if iter.next_checked::<T>().is_some() {
            warn!("found excess elements in array");

            return Err(());
        }

        Ok(val)
    }
}

impl<'a, T: ObjectLike<'a> + Copy + Default, const C: usize> TryFrom<Object<'a>> for [T; C]
where
    [T; C]: TryFrom<Array<'a>, Error = ()>,
{
    type Error = ();

    fn try_from(value: Object<'a>) -> Result<Self, Self::Error> {
        match value {
            Object::Array(a) => a.try_into(),
            _ => Err(()),
        }
    }
}

impl<'a, T: ObjectLike<'a> + Copy + Default, const C: usize> Readable<'a> for [T; C] {
    fn read(r: &mut Reader<'a>, ctx: &ReaderContext<'a>) -> Option<Self> {
        let array = Array::read(r, ctx)?;
        array.try_into().ok()
    }
}

impl<'a, T: ObjectLike<'a> + Copy + Default, const C: usize> ObjectLike<'a> for [T; C] {}

impl<'a, T: ObjectLike<'a>> TryFrom<Array<'a>> for Vec<T> {
    type Error = ();

    fn try_from(value: Array<'a>) -> Result<Self, Self::Error> {
        let mut iter = value.flex_iter();
        core::iter::from_fn(|| iter.next_checked::<T>()).collect()
    }
}

impl<'a, T: ObjectLike<'a>> TryFrom<Object<'a>> for Vec<T> {
    type Error = ();

    fn try_from(value: Object<'a>) -> Result<Self, Self::Error> {
        match value {
            Object::Array(a) => a.try_into(),
            _ => Err(()),
        }
    }
}

impl<'a, T: ObjectLike<'a>> Readable<'a> for Vec<T> {
    fn read(r: &mut Reader<'a>, ctx: &ReaderContext<'a>) -> Option<Self> {
        let array = Array::read(r, ctx)?;
        array.try_into().ok()
    }
}

impl<'a, T: ObjectLike<'a>> ObjectLike<'a> for Vec<T> {}

impl<'a, U: ObjectLike<'a>, T: ObjectLike<'a> + smallvec::Array<Item = U>> TryFrom<Array<'a>>
    for SmallVec<T>
{
    type Error = ();

    fn try_from(value: Array<'a>) -> Result<Self, Self::Error> {
        let mut iter = value.flex_iter();
        core::iter::from_fn(|| iter.next_checked::<U>()).collect()
    }
}

impl<'a, U: ObjectLike<'a>, T: ObjectLike<'a> + smallvec::Array<Item = U>> TryFrom<Object<'a>>
    for SmallVec<T>
{
    type Error = ();

    fn try_from(value: Object<'a>) -> Result<Self, Self::Error> {
        match value {
            Object::Array(a) => a.try_into(),
            _ => Err(()),
        }
    }
}

impl<'a, U: ObjectLike<'a>, T: ObjectLike<'a> + smallvec::Array<Item = U>> Readable<'a>
    for SmallVec<T>
{
    fn read(r: &mut Reader<'a>, ctx: &ReaderContext<'a>) -> Option<Self> {
        let array = Array::read(r, ctx)?;
        array.try_into().ok()
    }
}

impl<'a, U: ObjectLike<'a>, T: ObjectLike<'a> + smallvec::Array<Item = U>> ObjectLike<'a>
    for SmallVec<T>
where
    U: Clone,
    U: Debug,
{
}

#[cfg(test)]
mod tests {
    use crate::object::Object;
    use crate::object::r#ref::{MaybeRef, ObjRef};
    use crate::object::{Array, FromBytes};
    use crate::reader::Reader;
    use crate::reader::{ReaderContext, ReaderExt};
    use crate::xref::XRef;

    fn array_impl(data: &[u8]) -> Option<Vec<Object<'_>>> {
        Reader::new(data)
            .read_with_context::<Array<'_>>(&ReaderContext::new(XRef::dummy(), false))
            .map(|a| a.iter::<Object<'_>>().collect::<Vec<_>>())
    }

    fn array_ref_impl(data: &[u8]) -> Option<Vec<MaybeRef<Object<'_>>>> {
        Reader::new(data)
            .read_with_context::<Array<'_>>(&ReaderContext::new(XRef::dummy(), false))
            .and_then(|a| a.raw_iter().collect::<Result<Vec<_>, _>>().ok())
    }

    #[test]
    fn empty_array_1() {
        let res = array_impl(b"[]").unwrap();
        assert!(res.is_empty());
    }

    #[test]
    fn empty_array_2() {
        let res = array_impl(b"[   \n]").unwrap();
        assert!(res.is_empty());
    }

    #[test]
    fn array_1() {
        let res = array_impl(b"[34]").unwrap();
        assert!(matches!(res[0], Object::Number(_)));
    }

    #[test]
    fn array_2() {
        let res = array_impl(b"[true  ]").unwrap();
        assert!(matches!(res[0], Object::Boolean(_)));
    }

    #[test]
    fn array_3() {
        let res = array_impl(b"[true \n false 34.564]").unwrap();
        assert!(matches!(res[0], Object::Boolean(_)));
        assert!(matches!(res[1], Object::Boolean(_)));
        assert!(matches!(res[2], Object::Number(_)));
    }

    #[test]
    fn array_4() {
        let res = array_impl(b"[(A string.) << /Hi 34.35 >>]").unwrap();
        assert!(matches!(res[0], Object::String(_)));
        assert!(matches!(res[1], Object::Dict(_)));
    }

    #[test]
    fn array_5() {
        let res = array_impl(b"[[32]  345.6]").unwrap();
        assert!(matches!(res[0], Object::Array(_)));
        assert!(matches!(res[1], Object::Number(_)));
    }

    #[test]
    fn array_with_ref() {
        let res = array_ref_impl(b"[345 34 5 R 34.0]").unwrap();
        assert!(matches!(res[0], MaybeRef::NotRef(Object::Number(_))));
        assert!(matches!(
            res[1],
            MaybeRef::Ref(ObjRef {
                obj_number: 34,
                gen_number: 5
            })
        ));
        assert!(matches!(res[2], MaybeRef::NotRef(Object::Number(_))));
    }

    #[test]
    fn malformed_ref_is_not_valid_array() {
        assert!(array_ref_impl(b"[8 0R]").is_none());
    }

    #[test]
    fn array_with_single_number_is_valid() {
        assert!(array_ref_impl(b"[345345345]").is_some());
    }

    #[test]
    fn array_with_comment() {
        let res = array_impl(b"[true % A comment \n false]").unwrap();
        assert!(matches!(res[0], Object::Boolean(_)));
        assert!(matches!(res[1], Object::Boolean(_)));
    }

    #[test]
    fn array_with_trailing() {
        let res = array_impl(b"[(Hi) /Test]trialing data").unwrap();
        assert!(matches!(res[0], Object::String(_)));
        assert!(matches!(res[1], Object::Name(_)));
    }

    #[test]
    fn array_repr() {
        let array = Array::from_bytes(b"[34]").unwrap();

        assert_eq!(format!("{array:?}"), "[Number(Number(Integer(34)))]");
    }

    #[test]
    fn display() {
        let array = Array::from_bytes(b"[  34 % comment\n /Test  [1   2] ]").unwrap();

        assert_eq!(format!("{array}"), "[34 /Test [1 2]]");
    }
    #[test]
    fn lazy_syntax_errors_retain_positions_and_fuse_at_end() {
        let array = Array::from_bytes(b"[1 + 2 + ]").unwrap();
        let mut iter = array.raw_iter();
        assert!(iter.next().unwrap().is_ok());
        assert_eq!(iter.next().unwrap().unwrap_err().offset(), 2);
        assert!(iter.next().unwrap().is_ok());
        assert_eq!(iter.next().unwrap().unwrap_err().offset(), 6);
        assert_eq!(iter.next(), None);
        assert_eq!(iter.next(), None);
        assert_eq!(
            format!("{array}"),
            "[1 <unreadable object at byte 2> 2 <unreadable object at byte 6>]"
        );
    }

    #[test]
    fn typed_container_conversion_rejects_wrong_types_failed_reads_and_references() {
        for bytes in [
            b"[1 + ]".as_slice(),
            b"[1 /Wrong]",
            b"[1 50 0 R]",
            b"[1 + 2]",
            b"[+ 1]",
        ] {
            let array = Array::from_bytes(bytes).unwrap();
            assert!(Vec::<i32>::try_from(array.clone()).is_err(), "{bytes:?}");
            assert!(
                smallvec::SmallVec::<[i32; 4]>::try_from(array.clone()).is_err(),
                "{bytes:?}"
            );
            assert!(<[i32; 1]>::try_from(array).is_err(), "{bytes:?}");
        }
        let array = Array::from_bytes(b"[1 % trailing comment\n ]").unwrap();
        assert_eq!(Vec::<i32>::try_from(array.clone()), Ok(vec![1]));
        assert_eq!(<[i32; 1]>::try_from(array), Ok([1]));
    }
}
