//! PDF 1.7 §§7.5.8.2/7.6.1, using independent `crypt_filters.py` cipher vectors.
use super::crypt_filters;

pub(crate) struct Case {
    pub(crate) name: String,
    pub(crate) bytes: Vec<u8>,
    pub(crate) expected: Vec<u8>,
    pub(crate) content: Vec<u8>,
    pub(crate) doc_id: Vec<u8>,
}

pub(crate) const PROBES: &[u8] = b" /Probe (clear\\040bytes) /Nested [<636c656172206279746573> << /Probe (clear bytes) /Info 7 0 R >>]";

pub(crate) fn cases() -> Vec<Case> {
    let mut cases = Vec::new();
    for name in [
        "v2-same-defaults",
        "aesv2-same-defaults",
        "r5-aes-padding-0",
        "r6-aes-padding-0",
    ] {
        let base = crypt_filters::CASES
            .iter()
            .find(|case| case.name == name)
            .unwrap();
        for layout in ["table", "stream", "hybrid"] {
            let mut objects: Vec<_> = base
                .objects
                .iter()
                .map(|value| crypt_filters::hex(value))
                .collect();
            let length = objects[4].len();
            objects[4].truncate(length - 2);
            objects[4].extend_from_slice(PROBES);
            objects[4].extend_from_slice(b" >>");
            let mut bytes = b"%PDF-1.7\n".to_vec();
            let mut offsets = vec![0];
            for (i, object) in objects.iter().enumerate() {
                offsets.push(bytes.len());
                bytes.extend_from_slice(format!("{} 0 obj\n", i + 1).as_bytes());
                bytes.extend_from_slice(object);
                bytes.extend_from_slice(b"\nendobj\n");
            }
            let xref_stream = bytes.len();
            offsets.push(xref_stream);
            let mut entries = vec![0, 0, 0, 0, 0, 255, 255];
            for offset in &offsets[1..] {
                entries.push(1);
                entries.extend_from_slice(&u32::try_from(*offset).unwrap().to_be_bytes());
                entries.extend_from_slice(&[0, 0]);
            }
            let trailer = format!(
                "/Size 11 /Root 1 0 R /Info 7 0 R /Encrypt 5 0 R /ID [<{}><{}>]",
                base.doc_id, base.doc_id
            );
            bytes.extend_from_slice(
                format!(
                    "10 0 obj\n<< /Type /XRef /W [1 4 2] /Index [0 11] /Length {} {trailer}",
                    entries.len()
                )
                .as_bytes(),
            );
            bytes.extend_from_slice(PROBES);
            bytes.extend_from_slice(b" >>\nstream\n");
            bytes.extend_from_slice(&entries);
            bytes.extend_from_slice(b"\nendstream\nendobj\n");
            let start = if layout == "stream" {
                xref_stream
            } else {
                let start = bytes.len();
                bytes.extend_from_slice(b"xref\n0 11\n0000000000 65535 f \n");
                for offset in &offsets[1..] {
                    bytes.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
                }
                let hybrid = if layout == "hybrid" {
                    format!(" /XRefStm {xref_stream}")
                } else {
                    String::new()
                };
                bytes.extend_from_slice(format!("trailer\n<< {trailer}{hybrid} >>\n").as_bytes());
                start
            };
            bytes.extend_from_slice(format!("startxref\n{start}\n%%EOF\n").as_bytes());
            cases.push(Case {
                name: format!("{name}-{layout}"),
                bytes,
                expected: crypt_filters::expected_rgba(base),
                content: crypt_filters::hex(base.expected[0]),
                doc_id: crypt_filters::hex(base.doc_id),
            });
        }
    }
    cases
}
