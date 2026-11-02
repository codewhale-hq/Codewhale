use base64::{Engine as _, engine::general_purpose::STANDARD};

/// A 1x1 PNG, as bytes rather than a fixture file so the encoding tests
/// have no filesystem dependency.
pub const PNG_1X1: &[u8] = &[
    0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44, 0x52,
    0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1f, 0x15, 0xc4,
    0x89, 0x00, 0x00, 0x00, 0x0a, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9c, 0x63, 0x00, 0x01, 0x00, 0x00,
    0x05, 0x00, 0x01, 0x0d, 0x0a, 0x2d, 0xb4, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4e, 0x44, 0xae,
    0x42, 0x60, 0x82,
];

pub fn runtime_image_fixture(color: u8) -> codewhale_protocol::runtime::RuntimeImageInput {
    let image = image::DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(
        2,
        2,
        image::Rgba([color, 31, 99, 255]),
    ));
    let mut bytes = std::io::Cursor::new(Vec::new());
    image.write_to(&mut bytes, image::ImageFormat::Png).unwrap();
    codewhale_protocol::runtime::RuntimeImageInput {
        mime: "image/png".into(),
        data_base64: STANDARD.encode(bytes.into_inner()),
    }
}

pub fn runtime_image_fixture_bytes(size: usize) -> codewhale_protocol::runtime::RuntimeImageInput {
    let mut image = runtime_image_fixture(42);
    let mut bytes = STANDARD.decode(&image.data_base64).unwrap();
    bytes.resize(size, 0);
    image.data_base64 = STANDARD.encode(bytes);
    image
}
