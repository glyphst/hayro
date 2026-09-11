//! The starting point for reading PDF files.

use crate::PdfData;
use crate::object::Object;
use crate::page::Pages;
use crate::page::cached::CachedPages;
use crate::reader::Reader;
use crate::sync::Arc;
use crate::xref::{XRef, XRefError, fallback, root_xref};

pub use crate::crypto::{DecryptionError, EncryptionInfo, PasswordAuthentication};
use crate::metadata::Metadata;

#[cfg(test)]
mod password_tests;

#[cfg(test)]
mod crypt_tests;

/// A PDF file.
pub struct Pdf {
    xref: Arc<XRef>,
    header_version: PdfVersion,
    pages: CachedPages,
    data: PdfData,
}

/// An error that occurred while loading a PDF file.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum LoadPdfError {
    /// An error occurred while processing an encrypted document.
    Decryption(DecryptionError),
    /// The PDF was invalid or could not be parsed due to some other unknown reason.
    Invalid,
}

#[allow(clippy::len_without_is_empty)]
impl Pdf {
    /// Try to read the given PDF file.
    ///
    /// Returns `Err` if it was unable to read it.
    pub fn new(data: impl Into<PdfData>) -> Result<Self, LoadPdfError> {
        Self::new_with_password(data, "")
    }

    /// Try to read the given PDF file using the UTF-8 bytes of a password.
    ///
    /// This is a convenience wrapper for [`Self::new_with_password_bytes`]. It
    /// does not transcode legacy passwords to `PDFDocEncoding` or try other encodings.
    ///
    /// Returns `Err` if it was unable to read it or if the password is incorrect.
    pub fn new_with_password(
        data: impl Into<PdfData>,
        password: &str,
    ) -> Result<Self, LoadPdfError> {
        Self::new_with_password_bytes(data, password.as_bytes())
    }

    /// Try to read the given PDF file with explicitly encoded password bytes.
    ///
    /// Standard security revisions 2–4 use the supplied bytes unchanged before
    /// their 32-byte padding/truncation. The caller chooses the legacy encoding;
    /// no UTF-8 validation, transcoding or encoding recovery occurs for them.
    /// Revisions 5–6 require a valid UTF-8 representation before 127-byte
    /// truncation. Revision-6 `SASLprep` normalization is not performed, so callers
    /// must supply the prepared representation. Unencrypted files ignore passwords.
    ///
    /// Returns `Err` if parsing, password validation or authentication fails.
    pub fn new_with_password_bytes(
        data: impl Into<PdfData>,
        password: &[u8],
    ) -> Result<Self, LoadPdfError> {
        let data = data.into();
        let version = find_version(data.as_ref()).unwrap_or(PdfVersion::Pdf10);
        let xref = match root_xref(data.clone(), password) {
            Ok(x) => x,
            Err(e) => match e {
                XRefError::Unknown => {
                    fallback(data.clone(), password).ok_or(LoadPdfError::Invalid)?
                }
                XRefError::Encryption(e) => return Err(LoadPdfError::Decryption(e)),
            },
        };
        let xref = Arc::new(xref);

        let pages = CachedPages::new(xref.clone()).ok_or(LoadPdfError::Invalid)?;

        Ok(Self {
            xref,
            header_version: version,
            pages,
            data,
        })
    }

    /// Return the number of objects present in the PDF file.
    pub fn len(&self) -> usize {
        self.xref.len()
    }

    /// Return an iterator over all objects defined in the PDF file.
    pub fn objects(&self) -> impl IntoIterator<Item = Object<'_>> {
        self.xref.objects()
    }

    /// Return the version of the PDF file.
    pub fn version(&self) -> PdfVersion {
        self.xref
            .trailer_data()
            .version
            .unwrap_or(self.header_version)
    }

    /// Return the underlying data of the PDF file.
    pub fn data(&self) -> &PdfData {
        &self.data
    }

    /// Return the pages of the PDF file.
    pub fn pages(&self) -> &Pages<'_> {
        self.pages.get()
    }

    /// Return the xref of the PDF file.
    pub fn xref(&self) -> &XRef {
        &self.xref
    }

    /// Return the metadata in the document information dictionary of the document.
    pub fn metadata(&self) -> &Metadata {
        self.xref.metadata()
    }

    /// Return validated Standard-security-handler metadata for an encrypted document.
    ///
    /// Unencrypted documents return `None`.
    pub fn encryption_info(&self) -> Option<EncryptionInfo> {
        self.xref.encryption_info()
    }
}

fn find_version(data: &[u8]) -> Option<PdfVersion> {
    let data = &data[..data.len().min(2000)];
    let mut r = Reader::new(data);

    while r.forward_tag(b"%PDF-").is_none() {
        r.read_byte()?;
    }

    PdfVersion::from_bytes(r.tail()?)
}

/// The version of a PDF document.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum PdfVersion {
    /// PDF 1.0.
    Pdf10,
    /// PDF 1.1.
    Pdf11,
    /// PDF 1.2.
    Pdf12,
    /// PDF 1.3.
    Pdf13,
    /// PDF 1.4.
    Pdf14,
    /// PDF 1.5.
    Pdf15,
    /// PDF 1.6.
    Pdf16,
    /// PDF 1.7.
    Pdf17,
    /// PDF 2.0.
    Pdf20,
}

impl PdfVersion {
    pub(crate) fn from_bytes(bytes: &[u8]) -> Option<Self> {
        match bytes.get(..3)? {
            b"1.0" => Some(Self::Pdf10),
            b"1.1" => Some(Self::Pdf11),
            b"1.2" => Some(Self::Pdf12),
            b"1.3" => Some(Self::Pdf13),
            b"1.4" => Some(Self::Pdf14),
            b"1.5" => Some(Self::Pdf15),
            b"1.6" => Some(Self::Pdf16),
            b"1.7" => Some(Self::Pdf17),
            b"2.0" => Some(Self::Pdf20),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::pdf::{DecryptionError, LoadPdfError, PasswordAuthentication, Pdf, PdfVersion};

    #[test]
    fn issue_49() {
        let _ = Pdf::new(Vec::new());
    }

    #[test]
    fn pdf_version_header() {
        let data = std::fs::read("../hayro-tests/downloads/pdfjs/alphatrans.pdf").unwrap();
        let pdf = Pdf::new(data).unwrap();

        assert_eq!(pdf.version(), PdfVersion::Pdf17);
    }

    #[test]
    fn pdf_version_catalog() {
        let data = std::fs::read("../hayro-tests/downloads/pdfbox/2163.pdf").unwrap();
        let pdf = Pdf::new(data).unwrap();

        assert_eq!(pdf.version(), PdfVersion::Pdf14);
    }

    #[test]
    fn exposes_validated_standard_security_metadata() {
        let fixtures: [(&[u8], u8, PasswordAuthentication); 4] = [
            (
                include_bytes!("../../hayro-tests/pdfs/custom/password_encrypted_rc4_40.pdf"),
                2,
                PasswordAuthentication::User,
            ),
            (
                include_bytes!("../../hayro-tests/pdfs/custom/password_encrypted_rc4_128.pdf"),
                3,
                PasswordAuthentication::User,
            ),
            (
                include_bytes!("../../hayro-tests/pdfs/custom/password_encrypted_aes_128.pdf"),
                4,
                PasswordAuthentication::User,
            ),
            (
                include_bytes!("../../hayro-tests/pdfs/custom/password_encrypted_aes_256.pdf"),
                6,
                PasswordAuthentication::Owner,
            ),
        ];

        for (bytes, revision, authentication) in fixtures {
            let pdf = Pdf::new_with_password(bytes.to_vec(), "testpw").unwrap();
            let info = pdf.encryption_info().unwrap();
            assert_eq!(info.revision(), revision);
            assert_eq!(info.permissions(), 0xffff_fffc);
            assert_eq!(
                info.authentication(),
                authentication,
                "unexpected authentication role for revision {revision}"
            );
            assert!(info.encrypt_metadata());
        }
    }

    #[test]
    fn rejects_tampered_revision_6_permission_block() {
        let mut bytes =
            include_bytes!("../../hayro-tests/pdfs/custom/password_encrypted_aes_256.pdf").to_vec();
        let marker = b"/Perms <";
        let marker_offset = bytes
            .windows(marker.len())
            .position(|window| window == marker)
            .unwrap();
        let first_hex_digit = marker_offset + marker.len();
        bytes[first_hex_digit] = if bytes[first_hex_digit] == b'0' {
            b'1'
        } else {
            b'0'
        };

        match Pdf::new_with_password(bytes, "testpw") {
            Err(error) => assert_eq!(
                error,
                LoadPdfError::Decryption(DecryptionError::InvalidEncryption)
            ),
            Ok(_) => panic!("tampered encrypted permissions were accepted"),
        }
    }
}
