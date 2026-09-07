//! The null object.

use crate::object::Object;
use crate::object::macros::object;
use crate::reader::Reader;
use crate::reader::{Readable, ReaderContext, Skippable};
use crate::trivia::is_regular_character;
use core::fmt::{Display, Formatter};

/// The null object.
#[derive(Debug, Eq, PartialEq, Clone, Copy, Hash)]
pub struct Null;

impl Display for Null {
    fn fmt(&self, f: &mut Formatter<'_>) -> core::fmt::Result {
        f.write_str("null")
    }
}

object!(Null, Null);

impl Skippable for Null {
    fn skip(r: &mut Reader<'_>, _: bool) -> Option<()> {
        r.forward_tag(b"null")
    }
}

impl Readable<'_> for Null {
    fn read(r: &mut Reader<'_>, ctx: &ReaderContext<'_>) -> Option<Self> {
        Self::skip(r, ctx.in_content_stream())?;
        if r.peek_byte().is_some_and(is_regular_character) {
            return None;
        }

        Some(Self)
    }
}

#[cfg(test)]
mod tests {
    use crate::object::Null;
    use crate::reader::Reader;
    use crate::reader::ReaderExt;

    #[test]
    fn display() {
        assert_eq!(format!("{}", Null), "null");
    }

    #[test]
    fn null() {
        assert_eq!(
            Reader::new("null".as_bytes())
                .read_without_context::<Null>()
                .unwrap(),
            Null
        );
    }

    #[test]
    fn null_trailing() {
        for token in ["nullabs", "null0", "null#20", "null-null"] {
            assert!(
                Reader::new(token.as_bytes())
                    .read_without_context::<Null>()
                    .is_none()
            );
        }
        for suffix in b"\x00\t\n\x0c\r ()<>[]{}/%" {
            let mut bytes = b"null".to_vec();
            bytes.push(*suffix);
            assert!(Reader::new(&bytes).read_without_context::<Null>().is_some());
        }
    }
}
