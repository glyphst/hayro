//! Standard-handler crypt-filter selection (PDF 1.7 §§7.4.10 and 7.6.5).
use super::{DecryptionError, DecryptionTarget, DecryptorTag};
use crate::object::dict::keys::{CF, CFM, LENGTH, STM_F, STR_F, TYPE};
use crate::object::stream::optional_entry;
use crate::object::{Dict, Name, Object};
use crate::sync::HashMap;
use alloc::vec::Vec;

#[derive(Debug, Clone)]
pub(crate) struct DecryptorData {
    stream_filter: DecryptorTag,
    string_filter: DecryptorTag,
    embedded_file_filter: DecryptorTag,
    filters: HashMap<Vec<u8>, DecryptorTag>,
}

fn name<'a>(dict: &Dict<'a>, key: &[u8]) -> Result<Option<Name<'a>>, DecryptionError> {
    match optional_entry(dict, key).map_err(|_| DecryptionError::InvalidEncryption)? {
        None => Ok(None),
        Some(Object::Name(value)) => Ok(Some(value)),
        _ => Err(DecryptionError::InvalidEncryption),
    }
}

impl DecryptorData {
    pub(super) fn from_dict(
        dict: &Dict<'_>,
        default_bits: u16,
        version: u8,
    ) -> Result<Self, DecryptionError> {
        let stream_name = name(dict, STM_F)?;
        let string_name = name(dict, STR_F)?;
        let mut filters = HashMap::default();
        match optional_entry(dict, CF).map_err(|_| DecryptionError::InvalidEncryption)? {
            None => {}
            Some(Object::Dict(definitions)) => {
                for key in definitions.keys() {
                    // Table 20: a CF entry cannot redefine a standard filter.
                    if key.as_ref() == b"Identity" {
                        continue;
                    }
                    let Some(Object::Dict(value)) = optional_entry(&definitions, key.as_ref())
                        .map_err(|_| DecryptionError::InvalidEncryption)?
                    else {
                        return Err(DecryptionError::InvalidEncryption);
                    };
                    let document_default = [&stream_name, &string_name].iter().any(|name| {
                        name.as_ref()
                            .is_some_and(|name| name.as_ref() == key.as_ref())
                    });
                    let method = crypt_method(&value, default_bits, version, document_default)?;
                    filters.insert(key.as_ref().to_vec(), method);
                }
            }
            _ => return Err(DecryptionError::InvalidEncryption),
        }
        let resolve = |key, default| -> Result<DecryptorTag, DecryptionError> {
            match name(dict, key)? {
                None => Ok(default),
                Some(value) if value.as_ref() == b"Identity" => Ok(DecryptorTag::None),
                Some(value) => filters
                    .get(value.as_ref())
                    .copied()
                    .ok_or(DecryptionError::InvalidEncryption),
            }
        };
        let stream_filter = resolve(STM_F, DecryptorTag::None)?;
        let string_filter = resolve(STR_F, DecryptorTag::None)?;
        let embedded_file_filter = resolve(b"EFF", stream_filter)?;
        Ok(Self {
            stream_filter,
            string_filter,
            embedded_file_filter,
            filters,
        })
    }

    pub(super) fn select(&self, target: DecryptionTarget<'_>) -> Option<DecryptorTag> {
        Some(match target {
            DecryptionTarget::String => self.string_filter,
            DecryptionTarget::Stream => self.stream_filter,
            DecryptionTarget::EmbeddedFile => self.embedded_file_filter,
            DecryptionTarget::Named(b"Identity") => DecryptorTag::None,
            DecryptionTarget::Named(name) => *self.filters.get(name)?,
        })
    }
}

fn crypt_method(
    dict: &Dict<'_>,
    default_bits: u16,
    version: u8,
    document_default: bool,
) -> Result<DecryptorTag, DecryptionError> {
    if name(dict, TYPE)?.is_some_and(|value| value.as_ref() != b"CryptFilter") {
        return Err(DecryptionError::InvalidEncryption);
    }
    // Table 25 requires default string/stream filters to ignore AuthEvent
    // and authorize at document open. Other filters still require a known event;
    // this read-only handler authenticates all keys when the document is opened.
    if !document_default
        && name(dict, b"AuthEvent")?
            .is_some_and(|value| !matches!(value.as_ref(), b"DocOpen" | b"EFOpen"))
    {
        return Err(DecryptionError::InvalidEncryption);
    }
    let method = match name(dict, CFM)? {
        None => DecryptorTag::None,
        Some(value) => {
            DecryptorTag::from_name(&value).ok_or(DecryptionError::UnsupportedAlgorithm)?
        }
    };
    if (version == 4 && method == DecryptorTag::Aes256)
        || (version == 5 && !matches!(method, DecryptorTag::None | DecryptorTag::Aes256))
    {
        return Err(DecryptionError::UnsupportedAlgorithm);
    }
    let length =
        match optional_entry(dict, LENGTH).map_err(|_| DecryptionError::InvalidEncryption)? {
            None => match method {
                DecryptorTag::Aes128 => 16,
                DecryptorTag::Aes256 => 32,
                _ => i64::from(default_bits / 8),
            },
            Some(Object::Number(value)) => value
                .as_i64_exact()
                .ok_or(DecryptionError::InvalidEncryption)?,
            _ => return Err(DecryptionError::InvalidEncryption),
        };
    let valid = match method {
        DecryptorTag::None => true,
        // The Standard handler derives one file key. A conflicting filter
        // length cannot silently select a different cipher key.
        DecryptorTag::Rc4 => (5..=16).contains(&length) && length == i64::from(default_bits / 8),
        DecryptorTag::Aes128 => length == 16,
        DecryptorTag::Aes256 => length == 32,
    };
    if !valid {
        return Err(DecryptionError::InvalidEncryption);
    }
    Ok(method)
}
