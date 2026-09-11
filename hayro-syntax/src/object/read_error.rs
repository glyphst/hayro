use core::fmt;

/// A syntactically indexed container value could not be decoded.
///
/// Syntax scanning does not validate decryption or conversion of lazy values.
/// This error does not distinguish those causes or expose the value's bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ObjectReadError {
    pub(crate) offset: usize,
}

impl ObjectReadError {
    /// The failed value's byte offset in the container's `data()` slice.
    pub fn offset(self) -> usize {
        self.offset
    }
}

impl fmt::Display for ObjectReadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "unreadable object at byte {}", self.offset)
    }
}

impl core::error::Error for ObjectReadError {}
