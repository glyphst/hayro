//! Explicit and implicit stream decryption are mutually exclusive.
use super::{
    Cow, DecodeFailure, Dict, Filter, FiltersAndParams, Name, Object, Stream, optional_entry,
};
use crate::crypto::DecryptionTarget;
use crate::object::dict::keys::TYPE;
use alloc::vec::Vec;

impl<'a> Stream<'a> {
    /// Return bytes after implicit document decryption, preserving failure.
    /// Explicit filters, including `Crypt`, are applied only by the decoded APIs.
    pub fn raw_data_checked(&self) -> Result<Cow<'a, [u8]>, DecodeFailure> {
        let filters = self.filters_and_params()?;
        self.validate_crypt_placement(&filters)?;
        self.data_before_filters(&filters)
    }

    pub(super) fn validate_crypt_placement(
        &self,
        filters: &FiltersAndParams<'_>,
    ) -> Result<(), DecodeFailure> {
        for (index, filter) in filters.filters.iter().enumerate() {
            if *filter == Filter::Crypt
                && (index != 0
                    || self
                        .dict
                        .get::<Name<'_>>(TYPE)
                        .is_some_and(|name| name.as_ref() == b"XRef"))
            {
                return Err(DecodeFailure::InvalidFilterPlacement);
            }
        }
        Ok(())
    }

    pub(super) fn data_before_filters(
        &self,
        filters: &FiltersAndParams<'_>,
    ) -> Result<Cow<'a, [u8]>, DecodeFailure> {
        if filters.filters.contains(&Filter::Crypt) {
            return Ok(Cow::Borrowed(self.data));
        }
        let ctx = self.dict.ctx();
        let kind = self.dict.get::<Name<'_>>(TYPE);
        let kind = kind.as_ref().map(|value| value.as_ref());
        if !ctx.xref().needs_decryption(ctx)
            || kind == Some(b"XRef".as_slice())
            || (kind == Some(b"Metadata".as_slice())
                && ctx
                    .xref()
                    .encryption_info()
                    .is_some_and(|info| !info.encrypt_metadata()))
        {
            return Ok(Cow::Borrowed(self.data));
        }
        let target = if kind == Some(b"EmbeddedFile".as_slice()) {
            DecryptionTarget::EmbeddedFile
        } else {
            DecryptionTarget::Stream
        };
        ctx.xref()
            .decrypt(self.obj_id(), self.data, target)
            .map(Cow::Owned)
            .ok_or(DecodeFailure::Decryption)
    }

    pub(super) fn decrypt_explicit(
        &self,
        data: &[u8],
        params: &Dict<'_>,
    ) -> Result<Vec<u8>, DecodeFailure> {
        match optional_entry(params, TYPE)? {
            None => {}
            Some(Object::Name(name)) if name.as_ref() == b"CryptFilterDecodeParms" => {}
            _ => return Err(DecodeFailure::Decryption),
        }
        let name = match optional_entry(params, b"Name")? {
            None => None,
            Some(Object::Name(name)) => Some(name),
            _ => return Err(DecodeFailure::Decryption),
        };
        let name = name.as_ref().map_or(b"Identity".as_slice(), |n| n.as_ref());
        // The security handler selects and derives the object's cipher once.
        // The Crypt filter must not apply implicit stream decryption first.
        self.dict
            .ctx()
            .xref()
            .decrypt(self.obj_id(), data, DecryptionTarget::Named(name))
            .ok_or(DecodeFailure::Decryption)
    }
}
