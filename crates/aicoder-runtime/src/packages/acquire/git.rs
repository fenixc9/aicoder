use super::*;

pub(super) async fn git_archive(
    url: &Url,
    revision: &str,
    limits: PackageAcquireLimits,
    token: Option<&str>,
    context: TurnExecutionContext,
) -> Result<Vec<u8>> {
    use crate::{
        process::{ProcessCaptureOptions, capture},
        tools::ShellLimits,
    };
    use aicoder_core::{
        artifacts::{ArtifactLimits, ArtifactStore, MemoryArtifactStore},
        tools::ToolContext,
    };
    use std::{process::Command, sync::Arc};
    let stage = super::super::store::Stage(
        std::env::temp_dir().join(format!("aicoder-git-acquire-{}", uuid::Uuid::new_v4())),
    );
    let mut builder = fs::DirBuilder::new();
    use std::os::unix::fs::DirBuilderExt;
    builder.mode(0o700).create(&stage.0)?;
    let capacity = limits.max_download_bytes as u64 + 128;
    let artifacts = Arc::new(MemoryArtifactStore::new(ArtifactLimits {
        max_artifact_bytes: capacity,
        max_total_bytes: capacity * 2,
        max_artifacts: 2,
    })?);
    let tool_context =
        ToolContext::new(&stage.0, limits.timeout, 1024)?.with_artifact_store(artifacts.clone());
    let mut command = Command::new("bwrap");
    command
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_ATTR_NOSYSTEM", "1")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_CONFIG_COUNT", "9")
        .env("GIT_CONFIG_KEY_0", "core.hooksPath")
        .env("GIT_CONFIG_VALUE_0", "/dev/null")
        .env("GIT_CONFIG_KEY_1", "core.fsmonitor")
        .env("GIT_CONFIG_VALUE_1", "false")
        .env("GIT_CONFIG_KEY_2", "protocol.allow")
        .env("GIT_CONFIG_VALUE_2", "never")
        .env("GIT_CONFIG_KEY_3", "protocol.https.allow")
        .env("GIT_CONFIG_VALUE_3", "always")
        .env("GIT_CONFIG_KEY_4", "protocol.http.allow")
        .env("GIT_CONFIG_VALUE_4", "always")
        .env("GIT_CONFIG_KEY_5", "credential.helper")
        .env("GIT_CONFIG_VALUE_5", "")
        .env("GIT_CONFIG_KEY_6", "http.followRedirects")
        .env("GIT_CONFIG_VALUE_6", "false")
        .env("GIT_CONFIG_KEY_7", "transfer.fsckObjects")
        .env("GIT_CONFIG_VALUE_7", "true")
        .env("GIT_CONFIG_KEY_8", "fetch.recurseSubmodules")
        .env("GIT_CONFIG_VALUE_8", "false");
    if let Some(token) = token {
        // Environment only: no credential value appears in argv, metadata or diagnostics.
        command
            .env("GIT_CONFIG_COUNT", "10")
            .env(
                "GIT_CONFIG_KEY_9",
                format!("http.{}.extraHeader", url.as_str()),
            )
            .env(
                "GIT_CONFIG_VALUE_9",
                format!("Authorization: Bearer {token}"),
            );
    }
    command.args([
        "--unshare-all",
        "--share-net",
        "--uid",
        "0",
        "--gid",
        "0",
        "--cap-add",
        "CAP_SYS_ADMIN",
        "--cap-add",
        "CAP_SETPCAP",
        "--die-with-parent",
        "--new-session",
    ]);
    for path in [
        "/usr",
        "/bin",
        "/lib",
        "/lib64",
        "/etc/ssl",
        "/etc/resolv.conf",
        "/etc/nsswitch.conf",
        "/etc/hosts",
    ] {
        if Path::new(path).exists() {
            command.args(["--ro-bind", path, path]);
        }
    }
    command.args([
        "--proc",
        "/proc",
        "--dev",
        "/dev",
        "--tmpfs",
        "/tmp",
        "--remount-ro",
        "/",
        "--chdir",
        "/tmp",
        "--",
        "/bin/sh",
        "-c",
        GIT_SETUP,
        "git-acquire",
        url.as_str(),
        revision,
        &limits.git_disk_bytes.to_string(),
    ]);
    let options = ProcessCaptureOptions {
        limits: ShellLimits {
            max_command_bytes: 65536,
            max_output_bytes: capacity * 2,
        },
        ..Default::default()
    };
    let result = capture(command, &tool_context, options, context).await?;
    ensure!(
        result.exit_code == Some(0) && result.reaped && result.cleanup_ok && result.output_complete,
        "Git acquisition failed or cleanup/capture is incomplete; package code was not installed"
    );
    let reference = result
        .stdout_artifact
        .ok_or_else(|| anyhow::anyhow!("Git archive capture missing"))?;
    ensure!(
        reference.complete && reference.bytes <= capacity,
        "Git archive capture is incomplete"
    );
    let mut bytes = Vec::new();
    let mut offset = 0;
    while offset < reference.bytes {
        let page = artifacts.read(&reference.id, offset, 65536).await?;
        ensure!(
            page.artifact == reference && page.offset == offset && !page.bytes.is_empty(),
            "Git archive capture changed"
        );
        offset += page.bytes.len() as u64;
        ensure!(
            offset <= reference.bytes,
            "Git archive capture byte mismatch"
        );
        bytes.extend_from_slice(&page.bytes);
    }
    Ok(bytes)
}

// Only fixed host code is interpreted. URL/revision are positional arguments, never shell text.
const GIT_SETUP: &str = r#"set -eu
mount -t tmpfs -o "remount,size=$3,nr_inodes=32768" tmpfs /tmp
exec /usr/bin/setpriv --bounding-set=-all --inh-caps=-all --ambient-caps=-all /bin/sh -c '
set -eu
git init --bare --template= /tmp/repository >/dev/null 2>&1
git -C /tmp/repository fetch --quiet --depth=1 --no-tags --no-recurse-submodules -- "$1" "$2"
commit=$(git -C /tmp/repository rev-parse --verify "FETCH_HEAD^{commit}")
printf "%s\n" "$commit"
exec git -C /tmp/repository archive --format=tar --prefix=package/ "$commit"
' git-fetch "$1" "$2"
"#;
