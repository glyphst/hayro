//! Independently encrypted Standard crypt-filter fixtures; see `crypt_filters.py`.

pub(crate) struct Case {
    pub(crate) name: &'static str,
    pub(crate) encrypted: bool,
    pub(crate) valid: bool,
    pub(crate) doc_id: &'static str,
    pub(crate) title: &'static str,
    pub(crate) objects: &'static [&'static str],
    pub(crate) expected: &'static [&'static str],
}

include!("crypt_filter_vectors.rs");

pub(crate) fn hex(value: &str) -> Vec<u8> {
    value
        .as_bytes()
        .chunks_exact(2)
        .map(|pair| {
            u8::from_str_radix(std::str::from_utf8(pair).expect("fixture hex"), 16)
                .expect("fixture byte")
        })
        .collect()
}

pub(crate) fn pdf(case: &Case) -> Vec<u8> {
    document(case, case.objects.iter().map(|value| hex(value)).collect())
}

pub(crate) fn document(case: &Case, objects: Vec<Vec<u8>>) -> Vec<u8> {
    let mut bytes = b"%PDF-1.7\n".to_vec();
    let mut offsets = Vec::new();
    for (i, object) in objects.iter().enumerate() {
        offsets.push(bytes.len());
        bytes.extend_from_slice(format!("{} 0 obj\n", i + 1).as_bytes());
        bytes.extend_from_slice(object);
        bytes.extend_from_slice(b"\nendobj\n");
    }
    let xref = bytes.len();
    bytes.extend_from_slice(
        format!("xref\n0 {}\n0000000000 65535 f \n", objects.len() + 1).as_bytes(),
    );
    for offset in offsets {
        bytes.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
    }
    let encrypt = if case.encrypted { "/Encrypt 5 0 R" } else { "" };
    bytes.extend_from_slice(format!("trailer\n<< /Size {} /Root 1 0 R /Info 7 0 R /ID [<{}><{}>] {encrypt} >>\nstartxref\n{xref}\n%%EOF\n", objects.len() + 1, case.doc_id, case.doc_id).as_bytes());
    bytes
}

pub(crate) fn expected_rgba(case: &Case) -> Vec<u8> {
    let empty = hex(case.expected[0]).is_empty();
    let mut pixels = Vec::new();
    for y in 0..16 {
        for x in 0..16 {
            pixels.extend_from_slice(if empty {
                &[255, 255, 255, 255]
            } else if (4..12).contains(&x) && (4..12).contains(&y) {
                &[255, 0, 0, 255]
            } else {
                &[0, 255, 0, 255]
            });
        }
    }
    pixels
}
