//! Explicit bounded local image capture, independent from model/tool execution.
use aicoder_core::{
    AgentInput, TurnExecutionContext,
    model::{ContentBlock, ImageSource, Message, MessageRole},
    tools::ToolContext,
};
use anyhow::{Context, Result, ensure};
use base64::Engine;
use image::{AnimationDecoder, DynamicImage, ImageDecoder, ImageFormat};
use serde::Serialize;
use std::{
    fs::OpenOptions,
    io::{self, Cursor, Read, Write},
    os::unix::fs::{MetadataExt, OpenOptionsExt},
};
#[derive(Debug, Clone, Copy)]
pub struct ImageLimits {
    pub max_source_bytes: usize,
    pub max_output_bytes: usize,
    pub max_pixels: u64,
    pub max_side: u32,
}
impl Default for ImageLimits {
    fn default() -> Self {
        Self {
            max_source_bytes: 8 * 1024 * 1024,
            max_output_bytes: 512 * 1024,
            max_pixels: 16 * 1024 * 1024,
            max_side: 1600,
        }
    }
}
impl ImageLimits {
    pub fn validate(self) -> Result<Self> {
        ensure!(
            (1..=8 * 1024 * 1024).contains(&self.max_source_bytes)
                && (8192..=512 * 1024).contains(&self.max_output_bytes)
                && (1..=16 * 1024 * 1024).contains(&self.max_pixels)
                && (1..=1600).contains(&self.max_side),
            "invalid image limits"
        );
        Ok(self)
    }
}
#[derive(Debug, Clone, Serialize)]
pub struct ImageInfo {
    pub path: String,
    pub source_bytes: usize,
    pub source_fingerprint: String,
    pub source_dimensions: (u32, u32),
    pub orientation: String,
    pub dimensions: (u32, u32),
    pub normalized_bytes: usize,
    pub media_type: String,
    pub quality: u8,
    pub normalized: bool,
}
#[derive(Clone)]
pub struct ImageAttachment {
    source: ImageSource,
    info: ImageInfo,
}
impl ImageAttachment {
    pub fn source(&self) -> &ImageSource {
        &self.source
    }
    pub fn info(&self) -> &ImageInfo {
        &self.info
    }
    pub fn label(&self) -> String {
        format!(
            "{}: {}x{} JPEG q{} ({} bytes, normalized from {} bytes)",
            self.info.path,
            self.info.dimensions.0,
            self.info.dimensions.1,
            self.info.quality,
            self.info.normalized_bytes,
            self.info.source_bytes
        )
    }
}
/// Build exactly the captured user input; never reload a file or fabricate a tool result.
pub fn input(text: String, images: &[ImageAttachment]) -> Result<AgentInput> {
    ensure!(
        images.len() <= 4
            && images
                .iter()
                .map(|i| i.info.normalized_bytes)
                .sum::<usize>()
                <= 1024 * 1024,
        "image count/aggregate normalized budget exceeded"
    );
    let mut content = Vec::new();
    if !text.is_empty() {
        content.push(ContentBlock::Text { text });
    }
    for image in images {
        content.push(ContentBlock::Text {
            text: format!("[User-selected normalized static image: {}]", image.label()),
        });
        content.push(ContentBlock::Image {
            source: image.source.clone(),
        });
    }
    AgentInput::try_from(Message {
        role: MessageRole::User,
        content,
    })
}
struct Cancel(TurnExecutionContext);
impl Drop for Cancel {
    fn drop(&mut self) {
        self.0.cancel("Image capture future dropped");
    }
}
impl crate::Runtime {
    pub async fn read_image(
        &self,
        path: &str,
        limits: ImageLimits,
        context: TurnExecutionContext,
    ) -> Result<ImageAttachment> {
        let limits = limits.validate()?;
        let context = context.child();
        let _guard = Cancel(context.clone());
        let path = path.to_string();
        let workspace = self.config().workspace.clone();
        tokio::task::spawn_blocking(move || capture(&workspace, &path, limits, &context)).await?
    }
}
fn capture(
    workspace: &std::path::Path,
    argument: &str,
    limits: ImageLimits,
    context: &TurnExecutionContext,
) -> Result<ImageAttachment> {
    if context.is_cancelled() {
        return Err(context.error().into());
    }
    ensure!(argument.len() <= 4096, "image path exceeds 4096 bytes");
    let tool_context = ToolContext::new(workspace, std::time::Duration::from_secs(2), 16384)?;
    let paths = crate::tools::WorkspacePaths::from_context(&tool_context);
    let path = paths
        .resolve_existing(argument)
        .map_err(|e| anyhow::anyhow!("{}: {}", e.code, e.message))?;
    ensure!(path.to_str().is_some(), "image path requires UTF-8");
    let mut file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_NOCTTY)
        .open(&path)?;
    let before = file.metadata()?;
    ensure!(before.is_file(), "image must be a regular file");
    ensure!(
        before.len() <= limits.max_source_bytes as u64,
        "image source byte budget exceeded"
    );
    let mut bytes = Vec::new();
    let mut chunk = [0; 16384];
    loop {
        if context.is_cancelled() {
            return Err(context.error().into());
        }
        let n = file.read(&mut chunk)?;
        if n == 0 {
            break;
        }
        ensure!(
            n <= limits.max_source_bytes.saturating_sub(bytes.len()),
            "image source byte budget exceeded"
        );
        bytes.extend_from_slice(&chunk[..n]);
    }
    let stamp = |m: &std::fs::Metadata| {
        (
            m.dev(),
            m.ino(),
            m.len(),
            m.mtime(),
            m.mtime_nsec(),
            m.ctime(),
            m.ctime_nsec(),
        )
    };
    let named = std::fs::symlink_metadata(&path)?;
    ensure!(
        stamp(&before) == stamp(&file.metadata()?)
            && stamp(&before) == stamp(&named)
            && !named.file_type().is_symlink(),
        "image changed during capture"
    );
    let format = image::guess_format(&bytes).context("unsupported or invalid image signature")?;
    ensure!(
        matches!(
            format,
            ImageFormat::Png | ImageFormat::Jpeg | ImageFormat::Gif | ImageFormat::WebP
        ),
        "only PNG/JPEG/GIF/WebP images are supported"
    );
    let mut decoder_limits = image::Limits::default();
    decoder_limits.max_image_width = Some(8192);
    decoder_limits.max_image_height = Some(8192);
    decoder_limits.max_alloc = Some(64 * 1024 * 1024);
    let (mut decoded, source_dimensions, orientation) = match format {
        ImageFormat::Png => {
            let decoder = image::codecs::png::PngDecoder::with_limits(
                Cursor::new(&bytes),
                decoder_limits.clone(),
            )?;
            ensure!(!decoder.is_apng()?, "animated PNG is unsupported");
            decode(decoder, &decoder_limits, limits.max_pixels)?
        }
        ImageFormat::WebP => {
            let decoder = image::codecs::webp::WebPDecoder::new(Cursor::new(&bytes))?;
            ensure!(!decoder.has_animation(), "animated WebP is unsupported");
            decode(decoder, &decoder_limits, limits.max_pixels)?
        }
        ImageFormat::Gif => {
            let mut decoder = image::codecs::gif::GifDecoder::new(Cursor::new(&bytes))?;
            decoder.set_limits(decoder_limits.clone())?;
            let dims = decoder.dimensions();
            pixels(dims, limits.max_pixels)?;
            let mut frames = decoder.into_frames();
            let frame = frames.next().transpose()?.context("empty GIF")?;
            ensure!(
                frames.next().transpose()?.is_none(),
                "animated GIF is unsupported"
            );
            (
                DynamicImage::ImageRgba8(frame.into_buffer()),
                dims,
                image::metadata::Orientation::NoTransforms,
            )
        }
        _ => {
            let mut reader = image::ImageReader::with_format(Cursor::new(&bytes), format);
            reader.limits(decoder_limits.clone());
            decode(reader.into_decoder()?, &decoder_limits, limits.max_pixels)?
        }
    };
    if context.is_cancelled() {
        return Err(context.error().into());
    }
    decoded.apply_orientation(orientation);
    let small = {
        let image = decoded;
        if image.width() > limits.max_side || image.height() > limits.max_side {
            image.thumbnail(limits.max_side, limits.max_side)
        } else {
            image
        }
    };
    let rgba = small.into_rgba8();
    let mut rgb = image::RgbImage::new(rgba.width(), rgba.height());
    for (src, dst) in rgba.pixels().zip(rgb.pixels_mut()) {
        let a = u32::from(src[3]);
        for channel in 0..3 {
            dst[channel] = ((u32::from(src[channel]) * a + 255 * (255 - a) + 127) / 255) as u8;
        }
    }
    let mut final_data = None;
    for (side, quality) in [
        (limits.max_side, 80),
        (limits.max_side, 60),
        (limits.max_side, 40),
        (limits.max_side / 2, 60),
        (limits.max_side / 4, 60),
    ] {
        if context.is_cancelled() {
            return Err(context.error().into());
        }
        let resized = if side > 0 && (rgb.width() > side || rgb.height() > side) {
            image::imageops::thumbnail(&rgb, side, side)
        } else {
            rgb.clone()
        };
        let mut out = Bounded {
            bytes: Vec::new(),
            limit: limits.max_output_bytes,
            full: false,
            context,
        };
        let encoded = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, quality)
            .encode_image(&resized);
        match encoded {
            Ok(()) => {
                final_data = Some((out.bytes, resized.dimensions(), quality));
                break;
            }
            Err(_) if out.full => {}
            Err(error) => return Err(error.into()),
        }
    }
    let (jpeg, dimensions, quality) =
        final_data.context("image cannot fit normalized output budget")?;
    if context.is_cancelled() {
        return Err(context.error().into());
    }
    let fingerprint = format!(
        "fnv1a64:{:016x}",
        bytes
            .iter()
            .fold(0xcbf29ce484222325u64, |h, b| (h ^ u64::from(*b))
                .wrapping_mul(0x100000001b3))
    );
    let info = ImageInfo {
        path: paths.relative_path(&path),
        source_bytes: bytes.len(),
        source_fingerprint: fingerprint,
        source_dimensions,
        orientation: format!("{orientation:?}"),
        dimensions,
        normalized_bytes: jpeg.len(),
        media_type: "image/jpeg".into(),
        quality,
        normalized: true,
    };
    let source = ImageSource::Base64 {
        media_type: info.media_type.clone(),
        data: base64::engine::general_purpose::STANDARD.encode(jpeg),
    };
    Ok(ImageAttachment { source, info })
}
fn pixels((w, h): (u32, u32), limit: u64) -> Result<()> {
    ensure!(
        w > 0 && h > 0 && w <= 8192 && h <= 8192 && u64::from(w) * u64::from(h) <= limit,
        "image dimension/pixel budget exceeded"
    );
    Ok(())
}
fn decode(
    mut decoder: impl ImageDecoder,
    limits: &image::Limits,
    max_pixels: u64,
) -> Result<(DynamicImage, (u32, u32), image::metadata::Orientation)> {
    decoder.set_limits(limits.clone())?;
    let dims = decoder.dimensions();
    pixels(dims, max_pixels)?;
    ensure!(
        decoder.total_bytes() <= 64 * 1024 * 1024,
        "decoded image byte budget exceeded"
    );
    let orientation = decoder.orientation()?;
    let image = DynamicImage::from_decoder(decoder)?;
    Ok((image, dims, orientation))
}
struct Bounded<'a> {
    bytes: Vec<u8>,
    limit: usize,
    full: bool,
    context: &'a TurnExecutionContext,
}
impl Write for Bounded<'_> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if self.context.is_cancelled() {
            return Err(io::Error::other("image capture cancelled"));
        }
        if bytes.len() > self.limit.saturating_sub(self.bytes.len()) {
            self.full = true;
            return Err(io::Error::other("normalized image byte budget exceeded"));
        }
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
