use super::{DecryptionError, LoadPdfError, PasswordAuthentication, Pdf};

fn hex(value: &str) -> Vec<u8> {
    value
        .as_bytes()
        .chunks_exact(2)
        .map(|pair| u8::from_str_radix(core::str::from_utf8(pair).unwrap(), 16).unwrap())
        .collect()
}

// Independently authored PDF 1.7 Algorithm 2/3/4 vector: Python hashlib MD5
// and a separate RC4 implementation, not Hayro's crypto routines. The synthetic
// user password is byte E9; the owner password is the ASCII "audit-owner".
fn legacy_pdf(encrypted: bool) -> Vec<u8> {
    let content = hex("9852f9e693f2c0f70e7a6bc7c32514ae0ee8e7a77b62ce1986");
    let objects = [
        b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(),
        b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec(),
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] /Resources << >> /Contents 4 0 R >>".to_vec(),
        [format!("<< /Length {} >>\nstream\n",content.len()).as_bytes(), &content, b"\nendstream"].concat(),
        b"<< /Filter /Standard /V 1 /R 2 /P -4 /O <1ec11268f89373270f22e3840ba792936cb48bfa0656d4da33ab25200c7d24e8> /U <de348b4dc7b60120419dd17092b4f114655d8a48eefe1bfdec1fbfdf634207be> >>".to_vec(),
    ];
    let mut bytes = b"%PDF-1.7\n".to_vec();
    let mut offsets = Vec::new();
    for (index, object) in objects.iter().enumerate() {
        offsets.push(bytes.len());
        bytes.extend_from_slice(format!("{} 0 obj\n", index + 1).as_bytes());
        bytes.extend_from_slice(object);
        bytes.extend_from_slice(b"\nendobj\n");
    }
    let xref = bytes.len();
    bytes.extend_from_slice(b"xref\n0 6\n0000000000 65535 f \n");
    for offset in offsets {
        bytes.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
    }
    let encryption = if encrypted { "/Encrypt 5 0 R" } else { "" };
    bytes.extend_from_slice(format!("trailer\n<< /Size 6 /Root 1 0 R {encryption} /ID [<ddcb687a2e6057369cdcdfb8292da31b><ddcb687a2e6057369cdcdfb8292da31b>] >>\nstartxref\n{xref}\n%%EOF\n").as_bytes());
    bytes
}

#[test]
fn encoded_legacy_passwords_cross_the_public_api_without_transcoding() {
    for (password, role) in [
        (&[0xe9][..], PasswordAuthentication::User),
        (&b"audit-owner"[..], PasswordAuthentication::Owner),
    ] {
        let pdf = Pdf::new_with_password_bytes(legacy_pdf(true), password).unwrap();
        assert_eq!(pdf.encryption_info().unwrap().authentication(), role);
        assert_eq!(
            pdf.pages()[0].page_stream_checked().unwrap().unwrap(),
            b"1 0 0 rg 10 10 30 30 re f"
        );
    }
    for password in [&b"wrong"[..], &[0xff], "é".as_bytes()] {
        assert!(matches!(
            Pdf::new_with_password_bytes(legacy_pdf(true), password),
            Err(LoadPdfError::Decryption(DecryptionError::PasswordProtected))
        ));
    }
    // The convenience string API intentionally keeps its UTF-8-byte behavior.
    assert!(matches!(
        Pdf::new_with_password(legacy_pdf(true), "é"),
        Err(LoadPdfError::Decryption(DecryptionError::PasswordProtected))
    ));
    let mut padded = vec![0xe9];
    padded.extend_from_slice(
        &hex("28bf4e5e4e758a4164004e56fffa01082e2e00b6d0683e802f0ca9fe6453697a")[..31],
    );
    padded.extend_from_slice(&[0xff; 8]);
    assert!(Pdf::new_with_password_bytes(legacy_pdf(true), &padded).is_ok());
    assert!(Pdf::new_with_password_bytes(legacy_pdf(false), &[0xff, 0]).is_ok());
}

#[test]
fn modern_revisions_validate_utf8_before_byte_truncation() {
    for bytes in [
        &include_bytes!("../../../hayro-tests/pdfs/custom/encrypted_aes_256.pdf")[..],
        &include_bytes!("../../../hayro-tests/pdfs/custom/encrypted_aes_256_hardened.pdf")[..],
    ] {
        assert!(Pdf::new_with_password_bytes(bytes.to_vec(), b"").is_ok());
        for password in [vec![0xe9], [vec![b'a'; 127], vec![0xff]].concat()] {
            assert!(matches!(
                Pdf::new_with_password_bytes(bytes.to_vec(), &password),
                Err(LoadPdfError::Decryption(DecryptionError::PasswordEncoding))
            ));
        }
        assert!(matches!(
            Pdf::new_with_password_bytes(bytes.to_vec(), "é".as_bytes()),
            Err(LoadPdfError::Decryption(DecryptionError::PasswordProtected))
        ));
    }
}
