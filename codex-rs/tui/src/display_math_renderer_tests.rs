use std::io::Cursor;

use image::DynamicImage;
use image::ImageFormat;
use image::Rgba;
use image::RgbaImage;
use pretty_assertions::assert_eq;

use super::postprocess_png;
use super::tex_document;
use crate::display_math::MathStyle;

#[test]
fn postprocess_keeps_a_two_x_raster_at_the_original_logical_size() {
    let raw = RgbaImage::from_pixel(30, 15, Rgba([255, 255, 255, 255]));
    let mut encoded = Vec::new();
    DynamicImage::ImageRgba8(raw)
        .write_to(&mut Cursor::new(&mut encoded), ImageFormat::Png)
        .expect("encode source PNG");

    let rendered =
        postprocess_png(&encoded, (235, 235, 235), (0, 0, 0), [7; 32]).expect("postprocess PNG");
    let raster = image::load_from_memory_with_format(rendered.png.as_ref(), ImageFormat::Png)
        .expect("decode rendered PNG");

    assert_eq!(
        (
            (raster.width(), raster.height()),
            (rendered.width_px, rendered.height_px),
        ),
        ((18, 13), (9, 7)),
    );
}

#[test]
fn postprocess_uses_scale_independent_grayscale_alpha() {
    let raw = RgbaImage::from_fn(30, 15, |x, _| {
        if (8..22).contains(&x) {
            Rgba([255, 255, 255, 255])
        } else {
            Rgba([255, 255, 255, 0])
        }
    });
    let mut encoded = Vec::new();
    DynamicImage::ImageRgba8(raw)
        .write_to(&mut Cursor::new(&mut encoded), ImageFormat::Png)
        .expect("encode source PNG");

    let foreground = [197, 211, 223];
    let rendered = postprocess_png(
        &encoded,
        (foreground[0], foreground[1], foreground[2]),
        (17, 19, 23),
        [9; 32],
    )
    .expect("postprocess PNG");
    let raster = image::load_from_memory_with_format(rendered.png.as_ref(), ImageFormat::Png)
        .expect("decode rendered PNG")
        .to_rgba8();

    for pixel in raster.pixels() {
        if pixel.0[3] == 0 {
            assert_eq!(pixel.0, [0, 0, 0, 0]);
        } else {
            assert_eq!(&pixel.0[..3], &foreground);
        }
    }
}

#[test]
fn tex_document_selects_display_or_inline_style() {
    assert!(tex_document("x", MathStyle::Display).contains(r"\(\displaystyle x\)"));
    assert!(tex_document("x", MathStyle::Inline).contains(r"\(\textstyle x\)"));
}
