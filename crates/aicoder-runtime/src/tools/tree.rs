//! Bounded asynchronous enumeration, independent of OS directory order.
use super::{ToolContext, ToolFailure, WorkspacePaths, io::*};
use globset::{GlobBuilder, GlobMatcher};
use ignore::gitignore::{Gitignore, GitignoreBuilder};
use serde::Serialize;
use std::{
    path::{Path, PathBuf},
    sync::Arc,
};
use tokio::{
    fs::{self, ReadDir},
    io::AsyncReadExt,
};

#[derive(Clone, Serialize)]
pub(super) struct Entry {
    pub path: String,
    pub kind: &'static str,
}
pub(super) struct Tree {
    pub entries: Vec<Entry>,
    pub visited: usize,
}
struct Frame {
    directory: ReadDir,
    ignores: Vec<Arc<Gitignore>>,
}
struct Bounds {
    entries: usize,
    paths: usize,
    ignore_bytes: usize,
    patterns: usize,
}
impl Bounds {
    fn entry(&mut self, path: &str) -> Result<(), ToolFailure> {
        self.entries += 1;
        self.paths += path.len();
        if self.entries > MAX_ENTRIES || self.paths > 4 * 1024 * 1024 || path.len() > MAX_PATH_BYTES
        {
            return Err(failure(
                "scope_limit",
                "Search scope exceeds 20000 entries, 4 MiB of paths or 4096-byte paths; narrow path",
            ));
        }
        Ok(())
    }
    async fn ignores(
        &mut self,
        path: &Path,
        mut inherited: Vec<Arc<Gitignore>>,
        enabled: bool,
    ) -> Result<Vec<Arc<Gitignore>>, ToolFailure> {
        if !enabled {
            return Ok(inherited);
        }
        let source = path.join(".gitignore");
        match fs::symlink_metadata(&source).await {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(inherited),
            Err(error) => return Err(io_error(error)),
            Ok(metadata) if !metadata.is_file() => {
                return Err(failure(
                    "invalid_ignore",
                    ".gitignore must be a regular file, not a symlink or device",
                ));
            }
            Ok(_) => {}
        }
        let (file, _) = open_regular(&source).await?;
        let mut bytes = Vec::new();
        file.take(64 * 1024 + 1)
            .read_to_end(&mut bytes)
            .await
            .map_err(io_error)?;
        self.ignore_bytes += bytes.len();
        if bytes.len() > 64 * 1024 || self.ignore_bytes > 1024 * 1024 {
            return Err(failure(
                "scope_limit",
                "Ignore files exceed 64 KiB per file or 1 MiB per query; narrow scope or disable respect_gitignore",
            ));
        }
        let text = std::str::from_utf8(&bytes)
            .map_err(|_| failure("invalid_ignore", ".gitignore requires UTF-8"))?;
        let mut builder = GitignoreBuilder::new(path);
        for line in text.lines() {
            self.patterns += 1;
            if self.patterns > 4096 {
                return Err(failure(
                    "scope_limit",
                    "More than 4096 ignore lines; narrow scope or disable respect_gitignore",
                ));
            }
            builder
                .add_line(Some(source.clone()), line)
                .map_err(|error| failure("invalid_ignore", error.to_string()))?;
        }
        inherited.push(Arc::new(
            builder
                .build()
                .map_err(|error| failure("invalid_ignore", error.to_string()))?,
        ));
        Ok(inherited)
    }
}
pub(super) fn glob(pattern: &str) -> Result<GlobMatcher, ToolFailure> {
    if pattern.is_empty() || pattern.len() > 4096 {
        return Err(failure(
            "invalid_arguments",
            "Glob must contain 1..4096 bytes",
        ));
    }
    let pattern = if pattern.contains('/') {
        pattern.into()
    } else {
        format!("**/{pattern}")
    };
    GlobBuilder::new(&pattern)
        .literal_separator(true)
        .build()
        .map(|glob| glob.compile_matcher())
        .map_err(|error| failure("invalid_glob", error.to_string()))
}
fn ignored(ignores: &[Arc<Gitignore>], path: &Path, is_dir: bool) -> bool {
    for ignore in ignores.iter().rev() {
        let matched = ignore.matched(path, is_dir);
        if !matched.is_none() {
            return matched.is_ignore();
        }
    }
    false
}
pub(super) async fn enumerate(
    context: &ToolContext,
    root: &Path,
    recursive: bool,
    hidden: bool,
    respect_gitignore: bool,
) -> Result<Tree, ToolFailure> {
    let paths = WorkspacePaths::from_context(context);
    let mut bounds = Bounds {
        entries: 0,
        paths: 0,
        ignore_bytes: 0,
        patterns: 0,
    };
    if root.is_file() {
        let label = paths.relative_path(root);
        bounds.entry(&label)?;
        return Ok(Tree {
            entries: vec![Entry {
                path: label,
                kind: "file",
            }],
            visited: 1,
        });
    }
    if !root.is_dir() {
        return Err(failure(
            "not_a_directory",
            "Search path is not a regular file or directory",
        ));
    }
    let mut inherited = Vec::new();
    let mut parent = context.workspace_root().to_path_buf();
    // Inherit project-local ignore rules, without reading HOME or parents of the workspace.
    for component in root
        .strip_prefix(context.workspace_root())
        .expect("checked scope")
        .components()
    {
        inherited = bounds
            .ignores(&parent, inherited, respect_gitignore)
            .await?;
        parent.push(component);
    }
    let ignores = bounds.ignores(root, inherited, respect_gitignore).await?;
    let mut frames = vec![Frame {
        directory: fs::read_dir(root).await.map_err(io_error)?,
        ignores,
    }];
    let mut entries = Vec::new();
    while let Some(frame) = frames.last_mut() {
        let Some(entry) = frame.directory.next_entry().await.map_err(io_error)? else {
            frames.pop();
            continue;
        };
        let path: PathBuf = entry.path();
        let label = path
            .strip_prefix(context.workspace_root())
            .expect("enumerated workspace path")
            .to_str()
            .ok_or_else(|| {
                failure(
                    "unsupported_path",
                    "Directory contains a non-UTF-8 filename; narrow scope",
                )
            })?
            .to_string();
        bounds.entry(&label)?;
        let name = entry.file_name();
        let name = name.to_str().expect("validated path");
        if name == ".git" || !hidden && name.starts_with('.') {
            continue;
        }
        let kind = entry.file_type().await.map_err(io_error)?;
        if ignored(&frame.ignores, &path, kind.is_dir()) {
            continue;
        }
        let label_kind = if kind.is_file() {
            "file"
        } else if kind.is_dir() {
            "directory"
        } else if kind.is_symlink() {
            "symlink"
        } else {
            "other"
        };
        if !recursive || kind.is_file() {
            entries.push(Entry {
                path: label,
                kind: label_kind,
            });
        }
        if recursive && kind.is_dir() {
            let inherited = frame.ignores.clone();
            if frames.len() >= 64 {
                return Err(failure(
                    "scope_limit",
                    "Directory depth exceeds 64; narrow path",
                ));
            }
            let ignores = bounds.ignores(&path, inherited, respect_gitignore).await?;
            frames.push(Frame {
                directory: fs::read_dir(&path).await.map_err(io_error)?,
                ignores,
            });
        }
    }
    entries.sort_unstable_by(|a, b| a.path.cmp(&b.path));
    Ok(Tree {
        entries,
        visited: bounds.entries,
    })
}
