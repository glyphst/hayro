# hayro-interpret

[![Crates.io](https://img.shields.io/crates/v/hayro-interpret.svg)](https://crates.io/crates/hayro-interpret)
[![Documentation](https://docs.rs/hayro-interpret/badge.svg)](https://docs.rs/hayro-interpret)

<!-- cargo-rdme start -->

A crate for interpreting PDF files.

This crate provides an abstraction to interpret the content of a PDF file and render them
into an abstract [`Device`], which clients can implement as needed. This allows you, for
example, to render PDF files to bitmaps (which is what the `hayro` crate does), or other formats
such as SVG.

It should be noted that this crate is still very much in development. Therefore it currently
lacks pretty much any documentation on how to use it. It's current API also only really makes it
useful for rendering to PNG or SVG, though this will be improved upon in the future.

## Examples
See the `examples` folder on the GitHub repository. Apart from that, you can also consult
the implementation of `hayro` and `hayro-svg` to get an idea on how to use this crate.

## Safety
This crate forbids unsafe code via a crate-level attribute.

## Cargo features
This crate has one optional feature:
- `embed-fonts`: PDF processors are required to support 14 predefined fonts that do not need to be
  embedded into a PDF file. If you enable this feature, hayro will embed a (permissively-licensed)
  substitute for each font, so that you don't have to implement your custom font loading logic. This
  will add around ~240KB to your binary.

<!-- cargo-rdme end -->

## Retained transfer state

A retaining device can set `InterpreterSettings::defer_transfer_functions` to
receive untransferred source colors and selected functions separately.
`DrawProps` and `ImageDrawProps` expose an unapplied function; patterns retain
their own inherited or selecting state. Encoded shading samples and gradient
stops also remain raw in this mode. Group callbacks continue to carry invocation
opacity, blend mode, soft masks and AIS independently of elementary transfer.

The device must implement PDF 11.7.5.2 region selection, including zero-opacity
objects with positive shape. Deferral alone does not render transparent pages
correctly. The float image decoder retains 1–16-bit JPX components for explicit
PDF DeviceGray/DeviceRGB and CalGray/CalRGB spaces, without an intermediate byte
conversion. JPX dictionary BitsPerComponent and non-mask Decode are ignored.
Embedded color-space conversion, mixed component depths, greater depths and
alpha-bearing images remain outside this raw transfer path. JPEG2000 core
wavelet/color transforms retain their existing floating-point arithmetic; this
is not an exactness guarantee for every codestream. Ordinary JPX decoding also
resolves palettes when the PDF omits ColorSpace.

`TransferFunction::function` and `Function::definition` expose executable data
for retained storage. Large sample tables and stitching children are borrowed
so the caller can check its own copy budget. Preserve their clamps, encoding,
interpolation, integer exponent parity and typed calculator instructions.
The default interpretation mode continues applying transfer to source colors;
direct Gray/RGB color conversion retains real component and opacity values.


## License
Licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE) or <http://www.apache.org/licenses/LICENSE-2.0>)
- MIT license ([LICENSE-MIT](LICENSE-MIT) or <http://opensource.org/licenses/MIT>)

at your option. 
