//! Bounded MIME interpretation for image routing; original declarations remain domain data.
use std::borrow::Cow;

/// Normalize the image type/subtype for a provider view, ignoring MIME parameters.
/// Known JPEG/BMP aliases map to their canonical names. Unknown valid image subtypes
/// remain identifiable so a resolver can report unsupported media instead of hiding it.
/// This classifies metadata only; it does not validate pixels or confer storage authority.
pub fn image_media_type(value: &str) -> Option<Cow<'_, str>> {
    if value.is_empty()
        || value.len() > 128
        || !value.is_ascii()
        || value.bytes().any(|b| b.is_ascii_control())
    {
        return None;
    }
    let base = value.split(';').next()?.trim();
    let (kind, subtype) = base.split_once('/')?;
    if !kind.eq_ignore_ascii_case("image")
        || subtype.is_empty()
        || !subtype
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"!#$&^_.+-".contains(&b))
    {
        return None;
    }
    let canonical = match subtype.to_ascii_lowercase().as_str() {
        "png" => "image/png",
        "jpeg" | "jpg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "bmp" | "x-bmp" | "x-ms-bmp" => "image/bmp",
        _ => {
            return Some(if base.bytes().any(|b| b.is_ascii_uppercase()) {
                Cow::Owned(base.to_ascii_lowercase())
            } else {
                Cow::Borrowed(base)
            });
        }
    };
    Some(Cow::Borrowed(canonical))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn aliases_case_and_parameters_are_views_over_unchanged_declarations() {
        for (source, expected) in [
            (" IMAGE/JpG ; profile=fixture", "image/jpeg"),
            ("IMAGE/PNG;name=foo", "image/png"),
            ("image/X-MS-BMP", "image/bmp"),
            ("image/x-bmp", "image/bmp"),
            ("image/svg+xml; charset=utf-8", "image/svg+xml"),
            ("IMAGE/VND.EXAMPLE; a=b", "image/vnd.example"),
        ] {
            let original = source.to_owned();
            assert_eq!(image_media_type(&original).unwrap(), expected);
            assert_eq!(original, source);
        }
    }
    #[test]
    fn nonimages_injection_invalid_subtypes_and_exhausted_metadata_bounds_are_rejected() {
        for source in [
            "text/plain; type=image/png",
            "image/",
            "image/*",
            "image//png",
            "image/p ng",
            "image/png\r\nHeader:value",
            "image/图像",
            "file:///image/png",
        ] {
            assert!(image_media_type(source).is_none(), "{source:?}");
        }
        assert!(image_media_type(&format!("image/png;{}", "x".repeat(128))).is_none());
    }
}
