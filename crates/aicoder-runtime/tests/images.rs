#![cfg(feature = "images")]
use aicoder_core::{TurnExecutionContext, model::ImageSource, session::*};
use aicoder_runtime::{images::*, resources::ResourceSettings, session::JsonlSessionRepository, *};
use base64::Engine;
use image::{DynamicImage, ImageFormat, Rgba, RgbaImage};
use std::{collections::BTreeMap, fs, io::Cursor, os::unix::fs::symlink, sync::Arc};

fn runtime(root: &std::path::Path) -> Runtime {
    Runtime::load_with_environment(
        root,
        RuntimeOverrides {
            settings: Settings {
                resources: ResourceSettings {
                    enabled: Setting::Value(false),
                    ..Default::default()
                },
                ..Default::default()
            },
            session_dir: Setting::Clear,
            ..Default::default()
        },
        ConfigDiscovery {
            enabled: false,
            ..Default::default()
        },
        Arc::new(BTreeMap::new()),
    )
    .unwrap()
}
fn encoded(width: u32, height: u32, format: ImageFormat) -> Vec<u8> {
    let mut bytes = Cursor::new(Vec::new());
    DynamicImage::ImageRgba8(RgbaImage::from_pixel(width, height, Rgba([200, 50, 20, 0])))
        .write_to(&mut bytes, format)
        .unwrap();
    bytes.into_inner()
}
fn jpeg(attachment: &ImageAttachment) -> Vec<u8> {
    let ImageSource::Base64 { media_type, data } = attachment.source() else {
        panic!("not inline")
    };
    assert_eq!(media_type, "image/jpeg");
    base64::engine::general_purpose::STANDARD
        .decode(data)
        .unwrap()
}
#[tokio::test]
async fn static_codecs_are_detected_by_signature_normalized_and_snapshotted() {
    let root = tempfile::tempdir().unwrap();
    let runtime = runtime(root.path());
    for format in [
        ImageFormat::Png,
        ImageFormat::Jpeg,
        ImageFormat::Gif,
        ImageFormat::WebP,
    ] {
        let original = encoded(30, 20, format);
        fs::write(root.path().join("wrong-extension.txt"), &original).unwrap();
        let attachment = runtime
            .read_image(
                "wrong-extension.txt",
                ImageLimits {
                    max_side: 10,
                    ..Default::default()
                },
                TurnExecutionContext::new(),
            )
            .await
            .unwrap();
        assert_eq!(attachment.info().source_dimensions, (30, 20));
        assert_eq!(attachment.info().dimensions, (10, 7));
        assert_eq!(attachment.info().source_bytes, original.len());
        assert!(attachment.info().normalized);
        assert!(attachment.info().normalized_bytes <= 512 * 1024);
        assert_eq!(
            fs::read(root.path().join("wrong-extension.txt")).unwrap(),
            original
        );
        let snapshot = jpeg(&attachment);
        let decoded = image::load_from_memory(&snapshot).unwrap().into_rgb8();
        assert_eq!(decoded.dimensions(), attachment.info().dimensions);
        if format != ImageFormat::Jpeg {
            assert!(decoded.get_pixel(0, 0).0.iter().all(|c| *c >= 250));
        }
        fs::write(
            root.path().join("wrong-extension.txt"),
            "changed after capture",
        )
        .unwrap();
        assert_eq!(jpeg(&attachment), snapshot);
        let input = input("explain".into(), &[attachment])
            .unwrap()
            .into_message();
        let repo = JsonlSessionRepository::new(root.path().join("sessions")).unwrap();
        let mut session = repo.create(root.path()).unwrap();
        repo.append_entries(
            &mut session,
            vec![SessionEntryData::Message {
                message: input.clone(),
            }],
        )
        .unwrap();
        assert_eq!(
            repo.open(&session.metadata().id).unwrap().messages()[0].message,
            &input
        );
    }
}
#[tokio::test]
async fn dimensions_source_and_encoded_budgets_are_enforced_and_attempts_can_resize() {
    let root = tempfile::tempdir().unwrap();
    let runtime = runtime(root.path());
    fs::write(root.path().join("small"), encoded(30, 20, ImageFormat::Png)).unwrap();
    for limits in [
        ImageLimits {
            max_source_bytes: 1,
            ..Default::default()
        },
        ImageLimits {
            max_pixels: 599,
            ..Default::default()
        },
        ImageLimits {
            max_side: 0,
            ..Default::default()
        },
        ImageLimits {
            max_output_bytes: 8191,
            ..Default::default()
        },
    ] {
        assert!(
            runtime
                .read_image("small", limits, TurnExecutionContext::new())
                .await
                .is_err()
        );
    }
    fs::write(root.path().join("wide"), encoded(8193, 1, ImageFormat::Png)).unwrap();
    assert!(
        runtime
            .read_image("wide", Default::default(), TurnExecutionContext::new())
            .await
            .is_err()
    );
    let mut rng = 1u32;
    let noise = image::RgbImage::from_fn(1600, 1000, |_, _| {
        let mut pixel = [0; 3];
        for p in &mut pixel {
            rng ^= rng << 13;
            rng ^= rng >> 17;
            rng ^= rng << 5;
            *p = rng as u8;
        }
        image::Rgb(pixel)
    });
    noise
        .save_with_format(root.path().join("noise"), ImageFormat::Png)
        .unwrap();
    let result = runtime
        .read_image(
            "noise",
            ImageLimits {
                max_output_bytes: 64 * 1024,
                ..Default::default()
            },
            TurnExecutionContext::new(),
        )
        .await
        .unwrap();
    assert!(result.info().dimensions.0 < 1600);
    assert!(jpeg(&result).len() <= 64 * 1024);
}
#[tokio::test]
async fn local_capture_rejects_animation_malformed_paths_and_cancellation() {
    let root = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let runtime = runtime(root.path());
    let mut animated = Vec::new();
    {
        let mut encoder = image::codecs::gif::GifEncoder::new(&mut animated);
        let frame = image::Frame::new(RgbaImage::from_pixel(4, 4, Rgba([255, 0, 0, 255])));
        encoder.encode_frames([frame.clone(), frame]).unwrap();
    }
    fs::write(root.path().join("animated"), animated).unwrap();
    let error = runtime
        .read_image("animated", Default::default(), TurnExecutionContext::new())
        .await
        .err()
        .unwrap();
    assert!(error.to_string().contains("animated GIF"));
    fs::write(root.path().join("broken"), b"\x89PNG\r\n\x1a\ninvalid").unwrap();
    fs::write(
        outside.path().join("private"),
        encoded(2, 2, ImageFormat::Png),
    )
    .unwrap();
    symlink(outside.path().join("private"), root.path().join("escape")).unwrap();
    for path in ["broken", "escape", ".", "missing"] {
        assert!(
            runtime
                .read_image(path, Default::default(), TurnExecutionContext::new())
                .await
                .is_err()
        );
    }
    let cancelled = TurnExecutionContext::new();
    cancelled.cancel("stop");
    assert!(
        runtime
            .read_image("broken", Default::default(), cancelled)
            .await
            .is_err()
    );
}
#[tokio::test]
async fn exif_orientation_is_applied_without_retaining_private_metadata() {
    let root = tempfile::tempdir().unwrap();
    let original = encoded(30, 20, ImageFormat::Jpeg);
    let mut exif =
        b"Exif\0\0II\x2a\0\x08\0\0\0\x01\0\x12\x01\x03\0\x01\0\0\0\x06\0\0\0\0\0\0\0".to_vec();
    exif.extend_from_slice(b"private camera metadata");
    let mut source = original[..2].to_vec();
    source.extend_from_slice(&[0xff, 0xe1]);
    source.extend_from_slice(&((exif.len() + 2) as u16).to_be_bytes());
    source.extend(exif);
    source.extend_from_slice(&original[2..]);
    fs::write(root.path().join("oriented"), source).unwrap();
    let attachment = runtime(root.path())
        .read_image("oriented", Default::default(), TurnExecutionContext::new())
        .await
        .unwrap();
    assert_eq!(attachment.info().source_dimensions, (30, 20));
    assert_eq!(attachment.info().dimensions, (20, 30));
    assert!(!jpeg(&attachment).windows(4).any(|w| w == b"Exif"));
}
#[tokio::test]
async fn attachment_count_is_bounded_and_text_only_input_remains_compatible() {
    let root = tempfile::tempdir().unwrap();
    fs::write(root.path().join("image"), encoded(4, 4, ImageFormat::Png)).unwrap();
    let attachment = runtime(root.path())
        .read_image("image", Default::default(), TurnExecutionContext::new())
        .await
        .unwrap();
    assert!(input("".into(), &vec![attachment.clone(); 4]).is_ok());
    assert!(input("".into(), &vec![attachment; 5]).is_err());
    assert_eq!(
        input("text".into(), &[]).unwrap().message().text_content(),
        "text"
    );
}
