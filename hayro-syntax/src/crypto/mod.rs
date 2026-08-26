//! Cryptographic implementations for hayro, ported from pdf.js.
//!
//! **Important note**: Please keep in mind that these haven't been
//! audited and should not be used for security-critical purposes, like creating new
//! encrypted PDFs. They solely serve the purpose of being able to decrypt and read
//! _already_ encrypted documents, where security isn't really relevant.

use crate::crypto::DecryptionError::InvalidEncryption;
use crate::crypto::aes::{AES128Cipher, AES256Cipher};
use crate::crypto::rc4::Rc4;
use crate::object;
use crate::object::dict::keys::{
    CF, CFM, ENCRYPT_META_DATA, FILTER, LENGTH, O, OE, P, PERMS, R, STM_F, STR_F, U, UE, V,
};
use crate::object::{Dict, Name, ObjectIdentifier};
use crate::sync::HashMap;
use alloc::string::ToString;
use alloc::vec::Vec;
use core::cmp;
use core::fmt;
use core::ops::Deref;
use zeroize::Zeroizing;

mod aes;
mod md5;
mod rc4;
mod sha256;
mod sha384;
mod sha512;

const PASSWORD_PADDING: [u8; 32] = [
    0x28, 0xBF, 0x4E, 0x5E, 0x4E, 0x75, 0x8A, 0x41, 0x64, 0x00, 0x4E, 0x56, 0xFF, 0xFA, 0x01, 0x08,
    0x2E, 0x2E, 0x00, 0xB6, 0xD0, 0x68, 0x3E, 0x80, 0x2F, 0x0C, 0xA9, 0xFE, 0x64, 0x53, 0x69, 0x7A,
];

/// An error that occurred during decryption.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum DecryptionError {
    /// The ID entry is missing in the PDF.
    MissingIDEntry,
    /// The PDF is password-protected and no password (or a wrong one) was
    /// provided.
    PasswordProtected,
    /// The PDF has invalid encryption.
    InvalidEncryption,
    /// The PDF uses an unsupported encryption algorithm.
    UnsupportedAlgorithm,
}

/// The password role that authenticated a Standard-security-handler document.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum PasswordAuthentication {
    /// The supplied password authenticated as the user password.
    User,
    /// The supplied password authenticated as the owner password.
    Owner,
}

/// Validated encryption metadata for a Standard-security-handler document.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub struct EncryptionInfo {
    revision: u8,
    permissions: u32,
    authentication: PasswordAuthentication,
    encrypt_metadata: bool,
}

impl EncryptionInfo {
    fn new(
        revision: u8,
        permissions: u32,
        authentication: PasswordAuthentication,
        encrypt_metadata: bool,
    ) -> Self {
        Self {
            revision,
            permissions,
            authentication,
            encrypt_metadata,
        }
    }

    /// Return the Standard security handler revision.
    pub fn revision(self) -> u8 {
        self.revision
    }

    /// Return the validated raw permission bit field from the encryption dictionary.
    pub fn permissions(self) -> u32 {
        self.permissions
    }

    /// Return the password role that authenticated this document.
    pub fn authentication(self) -> PasswordAuthentication {
        self.authentication
    }

    /// Return whether document metadata is encrypted.
    pub fn encrypt_metadata(self) -> bool {
        self.encrypt_metadata
    }
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
enum DecryptorTag {
    None,
    Rc4,
    Aes128,
    Aes256,
}

impl DecryptorTag {
    fn from_name(name: &Name<'_>) -> Option<Self> {
        match name.as_str() {
            "None" | "Identity" => Some(Self::None),
            "V2" => Some(Self::Rc4),
            "AESV2" => Some(Self::Aes128),
            "AESV3" => Some(Self::Aes256),
            _ => None,
        }
    }
}
#[derive(Clone)]
pub(crate) enum Decryptor {
    None,
    Rc4 {
        key: Zeroizing<Vec<u8>>,
    },
    Aes128 {
        key: Zeroizing<Vec<u8>>,
        dict: DecryptorData,
    },
    Aes256 {
        key: Zeroizing<Vec<u8>>,
        dict: DecryptorData,
    },
}

impl fmt::Debug for Decryptor {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::None => formatter.write_str("Decryptor::None"),
            Self::Rc4 { .. } => formatter.write_str("Decryptor::Rc4 { key: <redacted> }"),
            Self::Aes128 { dict, .. } => formatter
                .debug_struct("Decryptor::Aes128")
                .field("key", &"<redacted>")
                .field("dict", dict)
                .finish(),
            Self::Aes256 { dict, .. } => formatter
                .debug_struct("Decryptor::Aes256")
                .field("key", &"<redacted>")
                .field("dict", dict)
                .finish(),
        }
    }
}

#[derive(Debug, Copy, Clone)]
pub(crate) enum DecryptionTarget {
    String,
    Stream,
}

impl Decryptor {
    pub(crate) fn decrypt(
        &self,
        id: ObjectIdentifier,
        data: &[u8],
        target: DecryptionTarget,
    ) -> Option<Vec<u8>> {
        match self {
            Self::None => Some(data.to_vec()),
            Self::Rc4 { key } => decrypt_rc4(key, data, id),
            Self::Aes128 { key, dict } | Self::Aes256 { key, dict } => {
                let crypt_dict = match target {
                    DecryptionTarget::String => dict.string_filter,
                    DecryptionTarget::Stream => dict.stream_filter,
                };

                match crypt_dict.cfm {
                    DecryptorTag::None => Some(data.to_vec()),
                    DecryptorTag::Rc4 => decrypt_rc4(key, data, id),
                    DecryptorTag::Aes128 => decrypt_aes128(key, data, id),
                    DecryptorTag::Aes256 => decrypt_aes256(key, data),
                }
            }
        }
    }
}

pub(crate) fn get(
    dict: &Dict<'_>,
    id: &[u8],
    password: &[u8],
) -> Result<(Decryptor, EncryptionInfo), DecryptionError> {
    let filter = dict.get::<Name<'_>>(FILTER).ok_or(InvalidEncryption)?;

    if filter.deref() != b"Standard" {
        return Err(DecryptionError::UnsupportedAlgorithm);
    }

    let encryption_v = dict.get::<u8>(V).ok_or(InvalidEncryption)?;
    let encrypt_metadata = dict.get::<bool>(ENCRYPT_META_DATA).unwrap_or(true);
    let revision = dict.get::<u8>(R).ok_or(InvalidEncryption)?;
    if !matches!(revision, 2..=6) {
        return Err(DecryptionError::UnsupportedAlgorithm);
    }
    if !matches!(
        (encryption_v, revision),
        (1, 2) | (2, 3) | (4, 4) | (5, 5 | 6)
    ) {
        return Err(InvalidEncryption);
    }
    let length = match encryption_v {
        1 => 40,
        2 => dict.get::<u16>(LENGTH).unwrap_or(40),
        4 => dict.get::<u16>(LENGTH).unwrap_or(128),
        5 => 256,
        _ => return Err(DecryptionError::UnsupportedAlgorithm),
    };

    let (algorithm, data) = match encryption_v {
        1 => (DecryptorTag::Rc4, None),
        2 => (DecryptorTag::Rc4, None),
        4 => (
            DecryptorTag::Aes128,
            Some(DecryptorData::from_dict(dict, length).ok_or(InvalidEncryption)?),
        ),
        5 | 6 => (
            DecryptorTag::Aes256,
            Some(DecryptorData::from_dict(dict, length).ok_or(InvalidEncryption)?),
        ),
        _ => {
            return Err(DecryptionError::UnsupportedAlgorithm);
        }
    };

    let byte_length = length / 8;
    if byte_length == 0 {
        return Err(InvalidEncryption);
    }

    let owner_string = dict.get::<object::String<'_>>(O).ok_or(InvalidEncryption)?;
    let user_string = dict.get::<object::String<'_>>(U).ok_or(InvalidEncryption)?;
    let raw_permissions = dict.get::<i64>(P).ok_or(InvalidEncryption)?;
    let permissions = if let Ok(value) = i32::try_from(raw_permissions) {
        value as u32
    } else {
        u32::try_from(raw_permissions).map_err(|_| InvalidEncryption)?
    };

    let (mut decryption_key, authentication) = if revision <= 4 {
        authenticate_password_rev234(
            password,
            encrypt_metadata,
            revision,
            byte_length,
            owner_string.as_ref(),
            permissions,
            id,
            user_string.as_ref(),
        )?
    } else {
        decryption_key_rev56(dict, revision, password, &owner_string, &user_string)?
    };

    if revision >= 5 {
        validate_permissions_rev56(dict, &decryption_key, permissions, encrypt_metadata)?;
    }

    // See pdf.js issue 19484.
    if encryption_v == 4 && decryption_key.len() < 16 {
        decryption_key.resize(16, 0);
    }

    let decryptor = match algorithm {
        DecryptorTag::None => Decryptor::None,
        DecryptorTag::Rc4 => Decryptor::Rc4 {
            key: decryption_key,
        },
        DecryptorTag::Aes128 => Decryptor::Aes128 {
            key: decryption_key,
            dict: data.unwrap(),
        },
        DecryptorTag::Aes256 => Decryptor::Aes256 {
            key: decryption_key,
            dict: data.unwrap(),
        },
    };

    Ok((
        decryptor,
        EncryptionInfo::new(revision, permissions, authentication, encrypt_metadata),
    ))
}

/// Algorithm 1.A: Encryption of data using the AES algorithms
fn decrypt_aes256(key: &[u8], data: &[u8]) -> Option<Vec<u8>> {
    // a) Use the 32-byte file encryption key for the AES-256 symmetric key algorithm,
    // along with the string or stream data to be encrypted.
    // Use the AES algorithm in Cipher Block Chaining (CBC) mode, which requires an initialization
    // vector. The block size parameter is set to 16 bytes, and the initialization
    // vector is a 16-byte random number that is stored as the first 16 bytes of the
    // encrypted stream or string.
    let (iv, data) = data.split_at_checked(16)?;
    let iv: [u8; 16] = iv.try_into().ok()?;
    let cipher = AES256Cipher::new(key)?;
    Some(cipher.decrypt_cbc(data, &iv, true))
}

fn decrypt_aes128(key: &[u8], data: &[u8], id: ObjectIdentifier) -> Option<Vec<u8>> {
    decrypt_rc_aes(key, id, true, |key| {
        // If using the AES algorithm, the Cipher Block Chaining (CBC) mode, which requires an initialization
        // vector, is used. The block size parameter is set to 16 bytes, and the initialization vector is a 16-byte
        // random number that is stored as the first 16 bytes of the encrypted stream or string.
        let cipher = AES128Cipher::new(key)?;
        let (iv, data) = data.split_at_checked(16)?;
        let iv: [u8; 16] = iv.try_into().ok()?;

        Some(cipher.decrypt_cbc(data, &iv, true))
    })
}

fn decrypt_rc4(key: &[u8], data: &[u8], id: ObjectIdentifier) -> Option<Vec<u8>> {
    decrypt_rc_aes(key, id, false, |key| {
        let mut rc = Rc4::new(key);
        Some(rc.decrypt(data))
    })
}

/// Algorithm 1: Encryption of data using the RC4 or AES algorithms
fn decrypt_rc_aes(
    key: &[u8],
    id: ObjectIdentifier,
    aes: bool,
    with_key: impl FnOnce(&[u8]) -> Option<Vec<u8>>,
) -> Option<Vec<u8>> {
    let n = key.len();
    // a) Obtain the object number and generation number from the object identifier of
    // the string or stream to be encrypted (see 7.3.10, "Indirect objects"). If the
    // string is a direct object, use the identifier of the indirect object containing
    // it.
    let mut key = Zeroizing::new(key.to_vec());

    // b) For all strings and streams without crypt filter specifier; treating the
    // object number and generation number as binary integers, extend the original
    // n-byte file encryption key to n + 5 bytes by appending the low-order 3 bytes of
    // the object number and the low-order 2 bytes of the generation number in that
    // order, low-order byte first.
    key.extend(&id.obj_number.to_le_bytes()[..3]);
    key.extend(&id.gen_number.to_le_bytes()[..2]);

    // If using the AES algorithm, extend the file encryption key an additional 4 bytes by adding the value
    // "sAlT", which corresponds to the hexadecimal values 0x73, 0x41, 0x6C, 0x54. (This addition is done
    // for backward compatibility and is not intended to provide additional security.)
    if aes {
        key.extend(b"sAlT");
    }

    // c) Initialise the MD5 hash function and pass the result of step (b) as input
    // to this function.
    let hash = Zeroizing::new(md5::calculate(&key));

    // d) Use the first (n + 5) bytes, up to a maximum of 16, of the output
    // from the MD5 hash as the key for the RC4 or AES symmetric key algorithms,
    // along with the string or stream data to be encrypted.
    let final_key = &hash[..cmp::min(16, n + 5)];

    with_key(final_key)
}

#[derive(Debug, Copy, Clone)]
pub(crate) struct DecryptorData {
    stream_filter: CryptDictionary,
    string_filter: CryptDictionary,
}

impl DecryptorData {
    fn from_dict(dict: &Dict<'_>, default_length: u16) -> Option<Self> {
        let mut mappings = HashMap::new();

        if let Some(dict) = dict.get::<Dict<'_>>(CF) {
            for key in dict.keys() {
                if let Some(dict) = dict.get::<Dict<'_>>(key.as_ref())
                    && let Some(crypt_dict) = CryptDictionary::from_dict(&dict, default_length)
                {
                    mappings.insert(key.as_str().to_string(), crypt_dict);
                }
            }
        }

        let stm_f = *mappings
            .get(dict.get::<Name<'_>>(STM_F)?.as_str())
            .unwrap_or(&CryptDictionary::identity(default_length));
        let str_f = *mappings
            .get(dict.get::<Name<'_>>(STR_F)?.as_str())
            .unwrap_or(&CryptDictionary::identity(default_length));

        Some(Self {
            stream_filter: stm_f,
            string_filter: str_f,
        })
    }
}

#[derive(Debug, Copy, Clone)]
struct CryptDictionary {
    cfm: DecryptorTag,
    _length: u16,
}

impl CryptDictionary {
    fn from_dict(dict: &Dict<'_>, default_length: u16) -> Option<Self> {
        let cfm = DecryptorTag::from_name(&dict.get::<Name<'_>>(CFM)?)?;
        // The standard security handler expresses the Length entry in bytes (e.g., 32 means a
        // length of 256 bits) and public-key security handlers express it as is (e.g., 256 means a
        // length of 256 bits).
        // Note: We only support the standard security handler.
        let mut length = dict.get::<u16>(LENGTH).unwrap_or(default_length / 8);

        // When CFM is AESV2, the Length key shall have the value of 128. When
        // CFM is AESV3, the Length key shall have a value of 256.
        if cfm == DecryptorTag::Aes128 {
            length = 16;
        } else if cfm == DecryptorTag::Aes256 {
            length = 32;
        }

        Some(Self {
            cfm,
            _length: length,
        })
    }

    fn identity(default_length: u16) -> Self {
        Self {
            cfm: DecryptorTag::None,
            _length: default_length,
        }
    }
}

/// Algorithm 2.B: Computing a hash (revision 6 and later)
fn compute_hash_rev56(
    password: &[u8],
    validation_salt: &[u8],
    user_string: Option<&[u8]>,
    revision: u8,
) -> Result<Zeroizing<[u8; 32]>, DecryptionError> {
    // Take the SHA-256 hash of the original input to the algorithm and name the resulting
    // 32 bytes, K.
    let mut k = {
        let mut input = Zeroizing::new(Vec::new());
        input.extend_from_slice(password);
        input.extend_from_slice(validation_salt);

        if let Some(user_string) = user_string {
            input.extend_from_slice(user_string);
        }

        let hash = sha256::calculate(&input);

        // Apparently revision 5 only uses this hash.
        if revision == 5 {
            return Ok(Zeroizing::new(hash));
        }

        Zeroizing::new(hash.to_vec())
    };

    let mut round: u16 = 0;

    // Perform the following steps (a)-(d) 64 times:
    loop {
        // a) Make a new string, K1, consisting of 64 repetitions of the sequence:
        // input password, K, the 48-byte user key. The 48 byte user key is only used when
        // checking the owner password or creating the owner key. If checking the user
        // password or creating the user key, K1 is the concatenation of the input
        // password and K.
        let k1 = {
            let mut single = Zeroizing::new(Vec::new());
            single.extend(password);
            single.extend_from_slice(&k);

            if let Some(user_string) = user_string {
                single.extend(user_string);
            }

            Zeroizing::new(single.repeat(64))
        };

        // b) Encrypt K1 with the AES-128 (CBC, no padding) algorithm,
        // using the first 16 bytes of K as the key and the second 16 bytes of K as the
        // initialization vector. The result of this encryption is E.
        let e = {
            let aes = AES128Cipher::new(&k[..16]).ok_or(InvalidEncryption)?;
            let mut res = aes.encrypt_cbc(&k1, &k[16..32].try_into().unwrap());

            // Remove padding that was added by `encrypt_cbc`.
            res.truncate(k1.len());

            Zeroizing::new(res)
        };

        // c) Taking the first 16 bytes of E as an unsigned big-endian integer,
        // compute the remainder, modulo 3. If the result is 0, the next hash used is
        // SHA-256, if the result is 1, the next hash used is SHA-384, if the result is
        // 2, the next hash used is SHA-512.
        let num = u128::from_be_bytes(e[..16].try_into().unwrap()) % 3;

        // d) Using the hash algorithm determined in step c, take the hash of E.
        // The result is a new value of K, which will be 32, 48, or 64 bytes in length.
        k = Zeroizing::new(match num {
            0 => sha256::calculate(&e).to_vec(),
            1 => sha384::calculate(&e).to_vec(),
            2 => sha512::calculate(&e).to_vec(),
            _ => unreachable!(),
        });

        round += 1;

        // Repeat the process (a-d) with this new value for K. Following 64 rounds
        // (round number 0 to round number 63), do the following, starting with round
        // number 64:
        if round > 63 {
            // e) Look at the very last byte of E. If the value of that byte
            // (taken as an unsigned integer) is greater than the round number - 32,
            // repeat steps (a-d) again.
            let last_byte = *e.last().unwrap();

            // f) Repeat from steps (a-e) until the value of the last byte
            // is < (round number) - 32.
            // For some reason we need to use <= here?
            if (last_byte as u16) <= round - 32 {
                break;
            }
        }
    }

    // The first 32 bytes of the final K are the output of the algorithm.
    let mut result = Zeroizing::new([0_u8; 32]);
    result.copy_from_slice(&k[..32]);
    Ok(result)
}

fn padded_password(password: &[u8]) -> Zeroizing<[u8; 32]> {
    let mut padded = Zeroizing::new([0_u8; 32]);
    let copy_len = password.len().min(32);
    padded[..copy_len].copy_from_slice(&password[..copy_len]);
    if copy_len < 32 {
        padded[copy_len..].copy_from_slice(&PASSWORD_PADDING[..(32 - copy_len)]);
    }

    padded
}

fn authenticate_password_rev234(
    password: &[u8],
    encrypt_metadata: bool,
    revision: u8,
    byte_length: u16,
    owner_string: &[u8],
    permissions: u32,
    id: &[u8],
    user_string: &[u8],
) -> Result<(Zeroizing<Vec<u8>>, PasswordAuthentication), DecryptionError> {
    let user_key = decryption_key_rev1234(
        password,
        encrypt_metadata,
        revision,
        byte_length,
        owner_string,
        permissions,
        id,
    )?;

    match authenticate_user_password_rev234(revision, &user_key, id, user_string) {
        Ok(()) => return Ok((user_key, PasswordAuthentication::User)),
        Err(DecryptionError::PasswordProtected) => {}
        Err(error) => return Err(error),
    }

    let recovered_user_password =
        recover_user_password_rev234(password, revision, byte_length, owner_string)?;
    let owner_key = decryption_key_rev1234(
        &recovered_user_password,
        encrypt_metadata,
        revision,
        byte_length,
        owner_string,
        permissions,
        id,
    )?;
    authenticate_user_password_rev234(revision, &owner_key, id, user_string)?;

    Ok((owner_key, PasswordAuthentication::Owner))
}

/// Algorithm 3.7: recover the padded user password using a candidate owner password.
fn recover_user_password_rev234(
    owner_password: &[u8],
    revision: u8,
    byte_length: u16,
    owner_string: &[u8],
) -> Result<Zeroizing<Vec<u8>>, DecryptionError> {
    if !matches!(revision, 2..=4) {
        return Err(InvalidEncryption);
    }

    let key_len = usize::from(byte_length);
    if key_len == 0 || key_len > 16 {
        return Err(InvalidEncryption);
    }

    let owner_entry = owner_string.get(..32).ok_or(InvalidEncryption)?;
    let padded_owner_password = padded_password(owner_password);
    let mut hash = Zeroizing::new(md5::calculate(&padded_owner_password[..]));
    if revision >= 3 {
        for _ in 0..50 {
            *hash = md5::calculate(&hash[..]);
        }
    }

    let key = Zeroizing::new(hash[..key_len].to_vec());
    let mut recovered = Zeroizing::new(owner_entry.to_vec());
    if revision == 2 {
        let mut rc = Rc4::new(&key);
        *recovered = rc.decrypt(&recovered);
    } else {
        for round in (0_u8..=19).rev() {
            let mut round_key = Zeroizing::new(key.to_vec());
            for byte in round_key.iter_mut() {
                *byte ^= round;
            }

            let mut rc = Rc4::new(&round_key);
            *recovered = rc.decrypt(&recovered);
        }
    }

    Ok(recovered)
}

/// Algorithm 2: Computing a file encryption key in order to encrypt a document (revision 4 and earlier)
fn decryption_key_rev1234(
    password: &[u8],
    encrypt_metadata: bool,
    revision: u8,
    byte_length: u16,
    owner_string: &[u8],
    permissions: u32,
    id: &[u8],
) -> Result<Zeroizing<Vec<u8>>, DecryptionError> {
    let mut md5_input = Zeroizing::new(Vec::new());

    // TODO: Convert to PDFDocEncoding.
    // a) Pad or truncate password to 32 bytes using PASSWORD_PADDING.
    let padded_password = padded_password(password);

    // b) Initialise the MD5 hash function and pass the
    // result of step a) as input to this function.
    md5_input.extend_from_slice(&padded_password[..]);

    // c) Pass the value of the encryption dictionary's O entry
    // to the MD5 hash function.
    md5_input.extend(owner_string);

    // d) Convert the integer value of the P entry to a 32-bit unsigned
    // binary number and pass these bytes to the MD5 hash function, low-order byte first.
    md5_input.extend(permissions.to_le_bytes());

    // e) Pass the first element of the file's file identifier array to the MD5 hash function.
    md5_input.extend(id);

    // f) (Security handlers of revision 4 or greater) If document metadata
    // is not being encrypted, pass 4 bytes with the value 0xFFFFFFFF to the MD5 hash function.
    if !encrypt_metadata && revision >= 4 {
        md5_input.extend(&[0xff, 0xff, 0xff, 0xff]);
    }

    // g) Finish the hash.
    let mut hash = Zeroizing::new(md5::calculate(&md5_input));

    if byte_length as usize > hash.len() {
        return Err(InvalidEncryption);
    }

    // h) For revisions >= 3, do the following 50 times: Take the output from the previous
    // MD5 hash and pass the first n bytes of the output as input into a new MD5 hash,
    // where n is the number of bytes of the file encryption key as defined by the value
    // of the encryption dictionary's `Length` entry.
    if revision >= 3 {
        for _ in 0..50 {
            *hash = md5::calculate(&hash[..byte_length as usize]);
        }
    }

    let decryption_key = Zeroizing::new(hash[..byte_length as usize].to_vec());
    Ok(decryption_key)
}

/// Algorithm 6: Authenticating the user password
fn authenticate_user_password_rev234(
    revision: u8,
    decryption_key: &[u8],
    id: &[u8],
    user_string: &[u8],
) -> Result<(), DecryptionError> {
    // a) Perform all but the last step of Algorithm 4 (revision 2) or Algorithm 5 (revision 3 + 4).
    let result = match revision {
        2 => user_password_rev2(decryption_key),
        3 | 4 => user_password_rev34(decryption_key, id),
        _ => return Err(InvalidEncryption),
    };

    // b) If the result of step (a) is equal to the value of the encryption dictionary's
    // U entry (comparing on the first 16 bytes in the case of security handlers of
    // revision 3 or greater), the password supplied is the correct user password.
    match revision {
        2 => {
            if result.as_slice() != user_string {
                return Err(DecryptionError::PasswordProtected);
            }
        }
        3 | 4 => {
            if Some(&result[..16]) != user_string.get(0..16) {
                return Err(DecryptionError::PasswordProtected);
            }
        }
        _ => unreachable!(),
    }

    Ok(())
}

/// Algorithm 4: Computing the encryption dictionary’s U-entry value
/// (Security handlers of revision 2).
fn user_password_rev2(decryption_key: &[u8]) -> Zeroizing<Vec<u8>> {
    // a) Create a file encryption key based on the user password string.
    // b) Encrypt the 32-byte padding string using an RC4 encryption
    // function with the file encryption key from the preceding step.
    let mut rc = Rc4::new(decryption_key);
    Zeroizing::new(rc.decrypt(&PASSWORD_PADDING))
}

/// Algorithm 5: Computing the encryption dictionary’s U (user password)
/// value (Security handlers of revision 3 or 4).
fn user_password_rev34(decryption_key: &[u8], id: &[u8]) -> Zeroizing<Vec<u8>> {
    // a) Create a file encryption key based on the user password string.
    let mut rc = Rc4::new(decryption_key);

    let mut input = Zeroizing::new(Vec::new());
    // b) Initialise the MD5 hash function and pass the 32-byte padding string.
    input.extend(PASSWORD_PADDING);

    // c) Pass the first element of the file's file identifier array to the hash function
    // and finish the hash.
    input.extend(id);
    let hash = Zeroizing::new(md5::calculate(&input));

    // d) Encrypt the 16-byte result of the hash, using an RC4 encryption function with
    // the encryption key from step (a).
    let mut encrypted = Zeroizing::new(rc.encrypt(&hash[..]));

    // e) Do the following 19 times: Take the output from the previous invocation of the
    // RC4 function and pass it as input to a new invocation of the function; use a file
    // encryption key generated by taking each byte of the original file encryption key
    // obtained in step (a) and performing an XOR (exclusive or) operation between that
    // byte and the single-byte value of the iteration counter (from 1 to 19).
    for i in 1..=19 {
        let mut key = Zeroizing::new(decryption_key.to_vec());
        for byte in key.iter_mut() {
            *byte ^= i;
        }

        let mut rc = Rc4::new(&key);
        *encrypted = rc.encrypt(&encrypted);
    }

    encrypted.resize(32, 0);
    encrypted
}

/// Algorithm 2.A: Retrieving the file encryption key from an encrypted document in order to decrypt it (revision 6 and later)
fn decryption_key_rev56(
    dict: &Dict<'_>,
    revision: u8,
    password: &[u8],
    owner_string: &object::String<'_>,
    user_string: &object::String<'_>,
) -> Result<(Zeroizing<Vec<u8>>, PasswordAuthentication), DecryptionError> {
    // a) The UTF-8 password string shall be generated from Unicode input by processing the input string with
    // the SASLprep (Internet RFC 4013) profile of stringprep (Internet RFC 3454) using the Normalize and BiDi
    // options, and then converting to a UTF-8 representation.
    // TODO: Do the above.
    // b) Truncate the UTF-8 representation to 127 bytes if it is longer than 127 bytes.
    let password = &password[..password.len().min(127)];

    let string_len = if revision <= 4 { 32 } else { 48 };

    let trimmed_os = owner_string.get(..string_len).ok_or(InvalidEncryption)?;

    let (owner_hash, owner_tail) = trimmed_os.split_at_checked(32).ok_or(InvalidEncryption)?;
    let (owner_validation_salt, owner_key_salt) =
        owner_tail.split_at_checked(8).ok_or(InvalidEncryption)?;

    let trimmed_us = user_string.get(..string_len).ok_or(InvalidEncryption)?;
    let (user_hash, user_tail) = trimmed_us.split_at_checked(32).ok_or(InvalidEncryption)?;
    let (user_validation_salt, user_key_salt) =
        user_tail.split_at_checked(8).ok_or(InvalidEncryption)?;

    // c) Test the password against the owner key by computing a hash using algorithm 2.B
    // with an input string consisting of the UTF-8 password concatenated with the 8 bytes of
    // owner Validation Salt, concatenated with the 48-byte U string. If the 32-byte result
    // matches the first 32 bytes of the O string, this is the owner password.
    let owner_validation_hash =
        compute_hash_rev56(password, owner_validation_salt, Some(trimmed_us), revision)?;
    if &owner_validation_hash[..] == owner_hash {
        // d) Compute an intermediate owner key by computing a hash using algorithm 2.B with an input string
        // consisting of the UTF-8 owner password concatenated with the 8 bytes of owner Key Salt,
        // concatenated with the 48-byte U string. The 32-byte result is the key used to decrypt the 32-byte OE string
        // using AES-256 in CBC mode with no padding and an initialization vector of zero. The 32-byte result is the file encryption key.
        let intermediate_owner_key =
            compute_hash_rev56(password, owner_key_salt, Some(trimmed_us), revision)?;

        let oe_string = dict
            .get::<object::String<'_>>(OE)
            .ok_or(InvalidEncryption)?;

        if oe_string.len() != 32 {
            return Err(InvalidEncryption);
        }

        let cipher = AES256Cipher::new(&intermediate_owner_key[..]).ok_or(InvalidEncryption)?;
        let zero_iv = [0_u8; 16];

        Ok((
            Zeroizing::new(cipher.decrypt_cbc(&oe_string, &zero_iv, false)),
            PasswordAuthentication::Owner,
        ))
    } else {
        let user_validation_hash =
            compute_hash_rev56(password, user_validation_salt, None, revision)?;
        if &user_validation_hash[..] != user_hash {
            return Err(DecryptionError::PasswordProtected);
        }

        // e) Compute an intermediate user key by computing a hash using algorithm 2.B with an input string
        // consisting of the UTF-8 user password concatenated with the 8 bytes of user Key Salt. The 32-byte result
        // is the key used to decrypt the 32-byte UE string using AES-256 in CBC mode with no padding and an
        // initialization vector of zero. The 32-byte result is the file encryption key.
        let intermediate_key = compute_hash_rev56(password, user_key_salt, None, revision)?;

        let ue_string = dict
            .get::<object::String<'_>>(UE)
            .ok_or(InvalidEncryption)?;

        if ue_string.len() != 32 {
            return Err(InvalidEncryption);
        }

        let cipher = AES256Cipher::new(&intermediate_key[..]).ok_or(InvalidEncryption)?;
        let zero_iv = [0_u8; 16];

        Ok((
            Zeroizing::new(cipher.decrypt_cbc(&ue_string, &zero_iv, false)),
            PasswordAuthentication::User,
        ))
    }
}

/// Algorithm 2.A, step f: authenticate the encrypted permission block.
fn validate_permissions_rev56(
    dict: &Dict<'_>,
    decryption_key: &[u8],
    permissions: u32,
    encrypt_metadata: bool,
) -> Result<(), DecryptionError> {
    let encrypted_permissions = dict
        .get::<object::String<'_>>(PERMS)
        .ok_or(InvalidEncryption)?;
    let encrypted_permissions: &[u8; 16] = encrypted_permissions
        .as_ref()
        .try_into()
        .map_err(|_| InvalidEncryption)?;
    validate_permissions_block(
        encrypted_permissions,
        decryption_key,
        permissions,
        encrypt_metadata,
    )
}

fn validate_permissions_block(
    encrypted_permissions: &[u8; 16],
    decryption_key: &[u8],
    permissions: u32,
    encrypt_metadata: bool,
) -> Result<(), DecryptionError> {
    let cipher = AES256Cipher::new(decryption_key).ok_or(InvalidEncryption)?;
    let decrypted = Zeroizing::new(cipher.decrypt_block(encrypted_permissions));

    let stored_permissions =
        u32::from_le_bytes(decrypted[..4].try_into().map_err(|_| InvalidEncryption)?);
    let metadata_flag = if encrypt_metadata { b'T' } else { b'F' };
    if stored_permissions != permissions
        || decrypted[4..8] != [0xff; 4]
        || decrypted[8] != metadata_flag
        || &decrypted[9..12] != b"adb"
    {
        return Err(InvalidEncryption);
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn owner_entry(
        owner_password: &[u8],
        user_password: &[u8],
        revision: u8,
        key_len: usize,
    ) -> Zeroizing<Vec<u8>> {
        let padded_owner = padded_password(owner_password);
        let mut hash = Zeroizing::new(md5::calculate(&padded_owner[..]));
        if revision >= 3 {
            for _ in 0..50 {
                *hash = md5::calculate(&hash[..]);
            }
        }

        let key = Zeroizing::new(hash[..key_len].to_vec());
        let mut encrypted = Zeroizing::new(padded_password(user_password).to_vec());
        if revision == 2 {
            let mut rc = Rc4::new(&key);
            *encrypted = rc.encrypt(&encrypted);
        } else {
            for round in 0_u8..=19 {
                let mut round_key = Zeroizing::new(key.to_vec());
                for byte in round_key.iter_mut() {
                    *byte ^= round;
                }

                let mut rc = Rc4::new(&round_key);
                *encrypted = rc.encrypt(&encrypted);
            }
        }

        encrypted
    }

    #[test]
    fn authenticates_distinct_owner_and_user_passwords_for_legacy_revisions() {
        let id = b"0123456789abcdef";
        let permissions = 0xffff_fffc;
        for (revision, byte_length) in [(2, 5), (3, 16), (4, 16)] {
            let owner = owner_entry(b"owner-secret", b"user-secret", revision, byte_length);
            let user_key = decryption_key_rev1234(
                b"user-secret",
                true,
                revision,
                byte_length as u16,
                &owner,
                permissions,
                id,
            )
            .unwrap();
            let user = match revision {
                2 => user_password_rev2(&user_key),
                3 | 4 => user_password_rev34(&user_key, id),
                _ => unreachable!(),
            };

            let (_, user_authentication) = authenticate_password_rev234(
                b"user-secret",
                true,
                revision,
                byte_length as u16,
                &owner,
                permissions,
                id,
                &user,
            )
            .unwrap();
            assert_eq!(user_authentication, PasswordAuthentication::User);

            let (_, owner_authentication) = authenticate_password_rev234(
                b"owner-secret",
                true,
                revision,
                byte_length as u16,
                &owner,
                permissions,
                id,
                &user,
            )
            .unwrap();
            assert_eq!(owner_authentication, PasswordAuthentication::Owner);

            assert_eq!(
                authenticate_password_rev234(
                    b"wrong",
                    true,
                    revision,
                    byte_length as u16,
                    &owner,
                    permissions,
                    id,
                    &user,
                )
                .unwrap_err(),
                DecryptionError::PasswordProtected
            );
        }
    }

    #[test]
    fn validates_revision_56_permission_block() {
        let key = [0x42; 32];
        let permissions = 0xffff_f2c4_u32;
        let mut clear = [0x91; 16];
        clear[..4].copy_from_slice(&permissions.to_le_bytes());
        clear[4..8].fill(0xff);
        clear[8] = b'F';
        clear[9..12].copy_from_slice(b"adb");

        let cipher = AES256Cipher::new(&key).unwrap();
        let encrypted = cipher.encrypt_block(&clear);
        assert_eq!(
            validate_permissions_block(&encrypted, &key, permissions, false),
            Ok(())
        );
        assert_eq!(
            validate_permissions_block(&encrypted, &key, permissions ^ 0x10, false),
            Err(InvalidEncryption)
        );
        assert_eq!(
            validate_permissions_block(&encrypted, &key, permissions, true),
            Err(InvalidEncryption)
        );

        let mut corrupted = encrypted;
        corrupted[0] ^= 1;
        assert_eq!(
            validate_permissions_block(&corrupted, &key, permissions, false),
            Err(InvalidEncryption)
        );
    }
}
