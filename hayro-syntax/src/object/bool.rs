//! Booleans.

use crate::object::Object;
use crate::object::macros::object;
use crate::reader::Reader;
use crate::reader::{Readable, ReaderContext, ReaderExt, Skippable};
use crate::trivia::is_regular_character;

impl Skippable for bool {
    fn skip(r: &mut Reader<'_>, _: bool) -> Option<()> {
        match r.peek_byte()? {
            b't' => r.forward_tag(b"true"),
            b'f' => r.forward_tag(b"false"),
            _ => None,
        }
    }
}

impl Readable<'_> for bool {
    fn read(r: &mut Reader<'_>, _: &ReaderContext<'_>) -> Option<Self> {
        let token = r.skip::<Self>(true)?;
        if r.peek_byte().is_some_and(is_regular_character) {
            return None;
        }
        match token {
            b"true" => Some(true),
            b"false" => Some(false),
            _ => None,
        }
    }
}

object!(bool, Boolean);

#[cfg(test)]
mod tests {
    use crate::reader::Reader;
    use crate::reader::ReaderExt;

    #[test]
    fn bool_true() {
        assert!(
            Reader::new("true".as_bytes())
                .read_without_context::<bool>()
                .unwrap()
        );
    }

    #[test]
    fn bool_false() {
        assert!(
            !Reader::new("false".as_bytes())
                .read_without_context::<bool>()
                .unwrap()
        );
    }

    #[test]
    fn bool_trailing() {
        for token in ["trueabdf", "falsejunk", "true0", "false#20", "true-false"] {
            assert!(
                Reader::new(token.as_bytes())
                    .read_without_context::<bool>()
                    .is_none()
            );
        }
        for suffix in b"\x00\t\n\x0c\r ()<>[]{}/%" {
            for token in ["true", "false"] {
                let mut bytes = token.as_bytes().to_vec();
                bytes.push(*suffix);
                assert_eq!(
                    Reader::new(&bytes).read_without_context::<bool>(),
                    Some(token == "true")
                );
            }
        }
    }
}
