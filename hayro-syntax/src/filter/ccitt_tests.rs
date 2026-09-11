//! Packed CCITT streams, parameters and image consumers.
use crate::object::stream::ImageDecodeParams;
use crate::object::{Dict, FromBytes, Stream};
use crate::reader::{Reader, ReaderContext, ReaderExt};
use alloc::{format, string::String, vec::Vec};
#[path = "ccitt_test_vectors.rs"]
mod vectors;

fn expand(encoded: &str) -> Vec<u8> {
    let mut bytes = Vec::new();
    for token in encoded.split_ascii_whitespace() {
        let (hex, count) = token.split_once(':').unwrap_or((token, "1"));
        bytes.extend(core::iter::repeat_n(
            u8::from_str_radix(hex, 16).unwrap(),
            count.parse::<usize>().unwrap(),
        ));
    }
    bytes
}
fn object(entries: &str, params: &str, input: &[u8]) -> Vec<u8> {
    let mut bytes = format!(
        "<< {entries} /Length {} /Filter /CCITTFaxDecode /DecodeParms << {params} >> >> stream\n",
        input.len()
    )
    .into_bytes();
    bytes.extend_from_slice(input);
    bytes.extend_from_slice(b"\nendstream");
    bytes
}

#[test]
fn independent_fax_vectors_through_generic_and_image_consumers() {
    for &(name, valid, params, input, expected, width, height) in vectors::VECTORS {
        let input = expand(input);
        let expected = expand(expected);
        for indexed in [false, true] {
            let entries = format!(
                "/Subtype /Image /Width {width} /Height {height} /BitsPerComponent 1 /ColorSpace /DeviceGray"
            );
            let bytes = object(&entries, params, &input);
            let stream = Reader::new(&bytes)
                .read_with_context::<Stream<'_>>(&ReaderContext::dummy())
                .unwrap();
            let decoded = stream.decoded_image(&ImageDecodeParams {
                width,
                height,
                is_indexed: indexed,
                bpc: Some(1),
                num_components: Some(1),
                ..Default::default()
            });
            if valid {
                let decoded = decoded.unwrap_or_else(|e| panic!("{name}: {e:?}"));
                assert_eq!(decoded.data.as_ref(), expected, "{name}, indexed={indexed}");
                assert!(
                    decoded.image_data.is_none(),
                    "filter must not override image dimensions"
                );
            } else {
                assert!(decoded.is_err(), "{name}");
            }
        }
        let bytes = object("", params, &input);
        let stream = Reader::new(&bytes)
            .read_with_context::<Stream<'_>>(&ReaderContext::dummy())
            .unwrap();
        if valid {
            assert_eq!(
                stream.decoded().unwrap().as_ref(),
                expected,
                "{name} generic"
            );
        } else {
            assert!(stream.decoded().is_err(), "{name} generic");
        }
    }
}

#[test]
fn image_dictionary_and_complete_samples_are_required() {
    let input = [0x35, 0x14]; // MH black row, eight pixels.
    for entries in [
        "/Width 8 /Height 1 /BitsPerComponent 8 /ColorSpace /DeviceGray",
        "/Width 8 /Height 1 /BitsPerComponent 1.0 /ColorSpace /DeviceGray",
        "/Width 8 /Height 2 /BitsPerComponent 1 /ColorSpace /DeviceGray",
        "/Width 8 /Height 1 /ImageMask 1 /BitsPerComponent 1",
        "/Width 8 /Height 1 /ImageMask true /BitsPerComponent 1 /Decode [0 0.5]",
    ] {
        let bytes = object(
            &format!("/Subtype /Image {entries}"),
            "/Columns 8 /Rows 1 /EndOfBlock false",
            &input,
        );
        let stream = Reader::new(&bytes)
            .read_with_context::<Stream<'_>>(&ReaderContext::dummy())
            .unwrap();
        assert!(stream.decoded().is_err(), "{entries}");
    }
}

#[test]
fn null_parameters_select_defaults_and_positive_k_is_only_a_sign() {
    let input = [0x35, 0x14];
    let params = Dict::from_bytes(b"<< /K null /Columns 8 /Rows 1 /EndOfBlock false /BlackIs1 null /EndOfLine null /EncodedByteAlign null /DamagedRowsBeforeError null >>").unwrap();
    assert_eq!(
        super::ccitt::decode(&input, &params, &ImageDecodeParams::default())
            .unwrap()
            .data
            .as_ref(),
        &[0]
    );
    let (_, _, params, data, expected, _, _) = vectors::VECTORS
        .iter()
        .find(|v| v.0 == "k1-eol0-align0-black0")
        .unwrap();
    for k in [1, 2, 97, i64::MAX] {
        let params = params.replace("/K 1 ", &format!("/K {k} "));
        assert_eq!(
            super::ccitt::decode(
                &expand(data),
                &Dict::from_bytes(params_with_brackets(&params).as_bytes()).unwrap(),
                &ImageDecodeParams::default()
            )
            .unwrap()
            .data
            .as_ref(),
            expand(expected)
        );
    }
}
fn params_with_brackets(params: &str) -> String {
    format!("<< {params} >>")
}
