use super::*;
use pretty_assertions::assert_eq;

#[test]
fn windows_clipboard_payload_is_an_owned_png_data_url() {
    let mut png = std::io::Cursor::new(Vec::new());
    image::RgbaImage::new(/*width*/ 3, /*height*/ 2)
        .write_to(&mut png, image::ImageFormat::Png)
        .unwrap();
    let encoded = base64::engine::general_purpose::STANDARD.encode(png.into_inner());

    assert_eq!(
        decode_windows_clipboard_image(&format!("\r\n{encoded}\r\n")).unwrap(),
        (
            format!("data:image/png;base64,{encoded}"),
            PastedImageInfo {
                width: 3,
                height: 2,
                encoded_format: EncodedImageFormat::Png,
            },
        )
    );
}

#[test]
fn windows_clipboard_rejects_malformed_payloads() {
    for encoded in ["not base64!", "", "bm90IGEgcG5n"] {
        assert!(matches!(
            decode_windows_clipboard_image(encoded),
            Err(PasteImageError::EncodeFailed(_))
        ));
    }
}

#[test]
fn windows_clipboard_rejects_oversized_payloads_before_decoding() {
    let encoded = "A".repeat(MAX_CLIPBOARD_IMAGE_DATA_URL_BYTES);

    assert!(matches!(
        decode_windows_clipboard_image(&encoded),
        Err(PasteImageError::EncodeFailed(error)) if error.contains("32 MiB transport limit")
    ));
}
