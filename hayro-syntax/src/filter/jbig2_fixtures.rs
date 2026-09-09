//! Authored T.88 page/region wrappers around independently encoded MMR bytes.
//! `ImageMagick` 7.1.2-29 Group4 TIFF strips, verified by jbig2dec 0.20 against
//! original packed samples. External provenance: pdf-test-suite/reports/
//! jbig2-contracts-2026-09-09/{pattern,empty-symbol-dictionary}-provenance.json.
// JBIG2 SHA-256: 7e1f6e6dcc19f9946bcad37dedb71193515edf526132e1c6a953444ca7f57f38
pub(super) const PATTERN: &[u8] = &[
    0, 0, 0, 1, 48, 1, 1, 0, 0, 0, 19, 0, 0, 0, 9, 0, 0, 0, 2, 0, 0, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0,
    0, 0, 2, 39, 1, 1, 0, 0, 0, 31, 0, 0, 0, 9, 0, 0, 0, 2, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 35, 162,
    58, 35, 160, 149, 36, 146, 73, 64, 4, 0, 64,
];
// JBIG2 SHA-256: a0201962ab0887b1191bec512df70e2577dff6341f650731ca542455bf1756fc
pub(super) const ENCODED_GLOBAL: &[u8] = &[
    0, 0, 0, 1, 48, 1, 1, 0, 0, 0, 19, 0, 0, 0, 168, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 0, 1, 0, 0,
    0, 0, 0, 2, 39, 1, 1, 0, 0, 0, 34, 0, 0, 0, 168, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 38,
    160, 174, 71, 13, 66, 58, 35, 130, 228, 112, 60, 55, 0, 16, 1,
];
