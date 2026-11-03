//! Codec-free immutable image acquisition. Pixel validation belongs to model projection.
use super::{ToolContext, ToolFailure, ToolSuccess, WorkspacePaths, io::*};
use aicoder_core::{artifacts::store_bytes, tool_images::MAX_TOOL_IMAGE_SOURCE_BYTES};
use serde_json::json;
use std::{fs::Metadata, path::Path, time::Duration};
use tokio::{
    fs::File,
    io::{AsyncReadExt, BufReader},
};

/// Magic bytes only; a recognized header is not a claim that pixels can be decoded.
pub(super) fn media_type(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some("image/png")
    } else if bytes.starts_with(b"\xff\xd8\xff") {
        Some("image/jpeg")
    } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        Some("image/gif")
    } else if bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(b"WEBP") {
        Some("image/webp")
    }
    // BMP's reserved header bytes distinguish ordinary text beginning with BM.
    else if bytes.starts_with(b"BM") && bytes.len() >= 14 && bytes[6..10] == [0; 4] {
        Some("image/bmp")
    } else {
        None
    }
}

fn same_source(a: &Metadata, b: &Metadata) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let stamp = |m: &Metadata| {
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
        stamp(a) == stamp(b)
    }
    #[cfg(not(unix))]
    {
        a.len() == b.len()
            && a.modified().ok() == b.modified().ok()
            && a.created().ok() == b.created().ok()
    }
}

fn delivery_failure(code: &'static str) -> ToolFailure {
    failure("image_capture_failed", "Image bytes were captured but artifact delivery failed; inspect host storage before repeating")
        .with_details(json!({"operation_completed":true,"delivery_complete":false,"capture_error":code}))
}

pub(super) async fn capture(
    context: &ToolContext,
    mut reader: BufReader<File>,
    path: &Path,
    before: Metadata,
    mime: &'static str,
    output_limit: usize,
) -> Result<ToolSuccess, ToolFailure> {
    let store = context.artifact_store().ok_or_else(|| {
        failure(
            "artifact_store_unavailable",
            "Image reads require explicit artifact storage",
        )
    })?;
    let maximum = MAX_TOOL_IMAGE_SOURCE_BYTES.min(store.limits().max_artifact_bytes);
    if before.len() > maximum {
        return Err(failure(
            "image_source_limit",
            "Image exceeds the source or artifact byte budget",
        ));
    }
    let output = json!({"kind":"image","path":WorkspacePaths::from_context(context).relative_path(path),
        "media_type":mime,"source_bytes":before.len(),"revision":super::atomic_file::revision(&FileStamp::from(&before)?),
        "operation_completed":true,"delivery_complete":true,"has_more":false});
    // Admit the complete metadata before writing a raw artifact; long paths cannot leave
    // an unreportable successful capture behind a truncated textual envelope.
    let mut result = success(output, output_limit)?;
    let mut bytes = Vec::with_capacity(before.len() as usize);
    (&mut reader)
        .take(maximum + 1)
        .read_to_end(&mut bytes)
        .await
        .map_err(io_error)?;
    if bytes.len() as u64 > maximum {
        return Err(failure(
            "image_source_limit",
            "Image exceeds the source or artifact byte budget",
        ));
    }
    let after = reader.get_ref().metadata().await.map_err(io_error)?;
    let named = tokio::fs::symlink_metadata(path).await.map_err(io_error)?;
    if bytes.len() as u64 != before.len()
        || !same_source(&before, &after)
        || !same_source(&before, &named)
        || named.file_type().is_symlink()
    {
        return Err(failure(
            "image_source_changed",
            "Image changed during capture; no snapshot was published",
        ));
    }
    let reference = tokio::time::timeout(
        Duration::from_millis(250),
        store_bytes(store.as_ref(), mime, &bytes, true),
    )
    .await
    .map_err(|_| delivery_failure("storage_timeout"))?
    .map_err(|_| delivery_failure("storage_failed"))?;
    if reference.validate().is_err()
        || !reference.complete
        || reference.bytes != bytes.len() as u64
        || reference.media_type != mime
    {
        return Err(delivery_failure("invalid_host_reference"));
    }
    result.artifacts.push(reference);
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn mutation_or_name_replacement_after_open_cannot_publish_a_snapshot() {
        use aicoder_core::artifacts::{ArtifactLimits, ArtifactStore, MemoryArtifactStore};
        use std::sync::Arc;
        for replace in [false, true] {
            let root = tempfile::tempdir().unwrap();
            let path = root.path().join("source");
            std::fs::write(&path, b"GIF89aoriginal").unwrap();
            let (file, _, before) = open_regular_with_metadata(&path).await.unwrap();
            if replace {
                std::fs::rename(&path, root.path().join("old")).unwrap();
            }
            std::fs::write(&path, b"GIF89amodified!").unwrap();
            let store = Arc::new(
                MemoryArtifactStore::new(ArtifactLimits {
                    max_artifact_bytes: 1024,
                    max_total_bytes: 1024,
                    max_artifacts: 1,
                })
                .unwrap(),
            );
            let context = ToolContext::new(root.path(), Duration::from_secs(3), 4096)
                .unwrap()
                .with_artifact_store(store.clone());
            let error = capture(
                &context,
                BufReader::new(file),
                &path,
                before,
                "image/gif",
                4096,
            )
            .await
            .unwrap_err();
            assert_eq!(error.code, "image_source_changed");
            assert!(store.begin("image/gif").await.is_ok());
        }
    }
    #[test]
    fn source_identity_detects_content_changes_even_when_mtime_is_restored() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("source");
        std::fs::write(&path, "before").unwrap();
        let before = std::fs::metadata(&path).unwrap();
        // Let the local timestamp clock advance before an equal-length replacement.
        std::thread::sleep(Duration::from_millis(10));
        std::fs::write(&path, "after!").unwrap();
        let file = std::fs::File::options().write(true).open(&path).unwrap();
        file.set_times(std::fs::FileTimes::new().set_modified(before.modified().unwrap()))
            .unwrap();
        let after = file.metadata().unwrap();
        #[cfg(unix)]
        assert!(!same_source(&before, &after));
        assert!(same_source(&after, &std::fs::metadata(&path).unwrap()));
    }
}
