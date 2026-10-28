use std::{
    collections::HashSet,
    fs::{self, File, OpenOptions},
    io::{BufRead, BufReader, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result, bail, ensure};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::types::{ChatMessage, Role};

use super::{Session, SessionInfo, SessionMessage, SessionMetadata, SessionRepository};

const SESSION_VERSION: u32 = 1;
const MAX_HEADER_BYTES: u64 = 1024 * 1024;

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum StoredRecord {
    Session {
        version: u32,
        id: String,
        created_at: u64,
        cwd: PathBuf,
    },
    Message {
        id: String,
        created_at: u64,
        message: ChatMessage,
    },
}

/// Append-only JSONL storage with one file per session.
#[derive(Debug, Clone)]
pub struct JsonlSessionRepository {
    root: PathBuf,
}

impl JsonlSessionRepository {
    pub fn new(root: impl AsRef<Path>) -> Result<Self> {
        fs::create_dir_all(root.as_ref()).with_context(|| {
            format!(
                "Failed to create session directory {}",
                root.as_ref().display()
            )
        })?;
        set_private_directory_permissions(root.as_ref())?;
        let root = root.as_ref().canonicalize().with_context(|| {
            format!(
                "Failed to resolve session directory {}",
                root.as_ref().display()
            )
        })?;
        Ok(Self { root })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    fn session_files(&self) -> Result<Vec<PathBuf>> {
        let mut paths = Vec::new();
        for entry in fs::read_dir(&self.root)
            .with_context(|| format!("Failed to read session directory {}", self.root.display()))?
        {
            let entry = entry.context("Failed to read session directory entry")?;
            let path = entry.path();
            if entry
                .file_type()
                .context("Failed to inspect session directory entry")?
                .is_file()
                && path
                    .extension()
                    .is_some_and(|extension| extension == "jsonl")
            {
                paths.push(path);
            }
        }
        Ok(paths)
    }

    fn find_session_path(&self, id: &str) -> Result<PathBuf> {
        validate_session_id(id)?;
        let mut found = None;
        for path in self.session_files()? {
            let Ok((header_id, _, _, _)) = read_header(&path) else {
                continue;
            };
            if header_id == id {
                ensure!(found.is_none(), "Multiple session files use id {id}");
                found = Some(path);
            }
        }
        found.with_context(|| format!("Session {id} not found"))
    }
}

impl SessionRepository for JsonlSessionRepository {
    fn create(&self, cwd: &Path) -> Result<Session> {
        let cwd = canonical_cwd(cwd)?;
        let timestamp = now_millis()?;
        let id = Uuid::new_v4().to_string();
        let path = self.root.join(format!("{timestamp}_{id}.jsonl"));
        ensure_path_is_in_root(&path, &self.root)?;

        let record = StoredRecord::Session {
            version: SESSION_VERSION,
            id: id.clone(),
            created_at: timestamp,
            cwd: cwd.clone(),
        };
        let encoded = encode_record(&record)?;
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        set_private_file_options(&mut options);
        let mut file = options
            .open(&path)
            .with_context(|| format!("Failed to create session file {}", path.display()))?;
        file.write_all(&encoded)
            .with_context(|| format!("Failed to write session header {}", path.display()))?;
        file.sync_data()
            .with_context(|| format!("Failed to sync session header {}", path.display()))?;
        let file_len = encoded.len() as u64;

        Ok(Session {
            metadata: SessionMetadata {
                version: SESSION_VERSION,
                id,
                cwd,
                created_at: timestamp,
                updated_at: timestamp,
                title: None,
            },
            messages: Vec::new(),
            path: Some(path),
            expected_file_len: file_len,
            valid_file_len: file_len,
            needs_separator: false,
        })
    }

    fn open(&self, id: &str) -> Result<Session> {
        load_session(&self.find_session_path(id)?)
    }

    fn list(&self, cwd: &Path) -> Result<Vec<SessionInfo>> {
        let cwd = canonical_cwd(cwd)?;
        let mut sessions = Vec::new();
        for path in self.session_files()? {
            match load_session(&path) {
                Ok(session) if session.metadata.cwd == cwd => {
                    sessions.push(SessionInfo::from(&session));
                }
                Ok(_) => {}
                Err(error) => {
                    tracing::warn!(
                        path = %path.display(),
                        error = %format!("{error:#}"),
                        "Skipping invalid session file"
                    );
                }
            }
        }
        sessions.sort_by(|left, right| {
            right
                .updated_at
                .cmp(&left.updated_at)
                .then_with(|| right.id.cmp(&left.id))
        });
        Ok(sessions)
    }

    fn append(&self, session: &mut Session, message: ChatMessage) -> Result<()> {
        if message.role == Role::System {
            bail!("System messages are runtime context and cannot be stored in a session");
        }
        let path = session
            .path
            .as_deref()
            .context("Cannot persist a session without a file path")?;
        ensure_path_is_in_root(path, &self.root)?;
        let timestamp = now_millis()?;
        let entry = SessionMessage {
            id: Uuid::new_v4().to_string(),
            created_at: timestamp,
            message,
        };
        let encoded = encode_record(&StoredRecord::Message {
            id: entry.id.clone(),
            created_at: entry.created_at,
            message: entry.message.clone(),
        })?;

        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(path)
            .with_context(|| format!("Failed to open session file {}", path.display()))?;
        FileExt::lock_exclusive(&file)
            .with_context(|| format!("Failed to lock session file {}", path.display()))?;
        let write_result = (|| -> Result<u64> {
            let actual_len = file
                .metadata()
                .with_context(|| format!("Failed to inspect session file {}", path.display()))?
                .len();
            ensure!(
                actual_len == session.expected_file_len,
                "Session {} was modified by another process; reopen it before appending",
                session.metadata.id
            );
            if session.valid_file_len < actual_len {
                file.set_len(session.valid_file_len)
                    .with_context(|| format!("Failed to repair session file {}", path.display()))?;
            }
            file.seek(SeekFrom::End(0))
                .with_context(|| format!("Failed to seek session file {}", path.display()))?;
            if session.needs_separator {
                file.write_all(b"\n").with_context(|| {
                    format!("Failed to append to session file {}", path.display())
                })?;
            }
            file.write_all(&encoded)
                .with_context(|| format!("Failed to append to session file {}", path.display()))?;
            file.sync_data()
                .with_context(|| format!("Failed to sync session file {}", path.display()))?;
            file.metadata()
                .with_context(|| format!("Failed to inspect session file {}", path.display()))
                .map(|metadata| metadata.len())
        })();
        let unlock_result = FileExt::unlock(&file)
            .with_context(|| format!("Failed to unlock session file {}", path.display()));
        let new_len = write_result?;
        unlock_result?;

        if session.metadata.title.is_none() {
            session.metadata.title = title_from_message(&entry.message);
        }
        session.metadata.updated_at = timestamp;
        session.messages.push(entry);
        session.expected_file_len = new_len;
        session.valid_file_len = new_len;
        session.needs_separator = false;
        Ok(())
    }

    fn delete(&self, id: &str) -> Result<()> {
        let path = self.find_session_path(id)?;
        ensure_path_is_in_root(&path, &self.root)?;
        fs::remove_file(&path)
            .with_context(|| format!("Failed to delete session file {}", path.display()))
    }
}

fn load_session(path: &Path) -> Result<Session> {
    let file = File::open(path)
        .with_context(|| format!("Failed to open session file {}", path.display()))?;
    FileExt::lock_shared(&file)
        .with_context(|| format!("Failed to lock session file {}", path.display()))?;
    let read_result =
        fs::read(path).with_context(|| format!("Failed to read session file {}", path.display()));
    let unlock_result = FileExt::unlock(&file)
        .with_context(|| format!("Failed to unlock session file {}", path.display()));
    let bytes = read_result?;
    unlock_result?;
    ensure!(
        !bytes.is_empty(),
        "Session file {} is empty",
        path.display()
    );

    let mut records = Vec::new();
    let mut valid_file_len = 0_u64;
    let mut offset = 0_u64;
    let chunks = bytes.split_inclusive(|byte| *byte == b'\n');
    for (index, chunk) in chunks.enumerate() {
        offset += chunk.len() as u64;
        let complete_line = chunk.ends_with(b"\n");
        let payload = if complete_line {
            &chunk[..chunk.len() - 1]
        } else {
            chunk
        };
        if payload.iter().all(u8::is_ascii_whitespace) {
            valid_file_len = offset;
            continue;
        }
        let parsed = std::str::from_utf8(payload)
            .context("Session record is not valid UTF-8")
            .and_then(|line| {
                serde_json::from_str::<StoredRecord>(line)
                    .context("Session record is not valid JSON")
            });
        match parsed {
            Ok(record) => {
                records.push(record);
                valid_file_len = offset;
            }
            Err(error) if !complete_line && offset == bytes.len() as u64 => {
                tracing::warn!(
                    path = %path.display(),
                    line = index + 1,
                    error = %error,
                    "Ignoring incomplete final session record"
                );
            }
            Err(error) => {
                return Err(error).with_context(|| {
                    format!("Invalid session record at {}:{}", path.display(), index + 1)
                });
            }
        }
    }

    let mut records = records.into_iter();
    let (version, id, created_at, cwd) = match records.next() {
        Some(StoredRecord::Session {
            version,
            id,
            created_at,
            cwd,
        }) => (version, id, created_at, cwd),
        _ => bail!(
            "First record in session file {} is not a session header",
            path.display()
        ),
    };
    ensure!(
        version == SESSION_VERSION,
        "Unsupported session version {version} in {}",
        path.display()
    );
    validate_session_id(&id)?;
    ensure!(
        cwd.is_absolute(),
        "Session workspace in {} is not an absolute path",
        path.display()
    );
    let mut messages = Vec::new();
    let mut ids = HashSet::new();
    let mut updated_at = created_at;
    let mut title = None;
    for record in records {
        match record {
            StoredRecord::Session { .. } => {
                bail!("Session file {} contains multiple headers", path.display())
            }
            StoredRecord::Message {
                id,
                created_at,
                message,
            } => {
                ensure!(ids.insert(id.clone()), "Duplicate message id {id}");
                ensure!(
                    message.role != Role::System,
                    "Session {} contains a persisted system message",
                    path.display()
                );
                if title.is_none() {
                    title = title_from_message(&message);
                }
                updated_at = updated_at.max(created_at);
                messages.push(SessionMessage {
                    id,
                    created_at,
                    message,
                });
            }
        }
    }

    let actual_len = bytes.len() as u64;
    let needs_separator = valid_file_len == actual_len && !bytes.ends_with(b"\n");
    Ok(Session {
        metadata: SessionMetadata {
            version,
            id,
            cwd,
            created_at,
            updated_at,
            title,
        },
        messages,
        path: Some(path.to_path_buf()),
        expected_file_len: actual_len,
        valid_file_len,
        needs_separator,
    })
}

fn read_header(path: &Path) -> Result<(String, u32, u64, PathBuf)> {
    let file = File::open(path)
        .with_context(|| format!("Failed to open session file {}", path.display()))?;
    let mut reader = BufReader::new(file);
    let mut bytes = Vec::new();
    loop {
        let (consumed, found_newline) = {
            let available = reader
                .fill_buf()
                .with_context(|| format!("Failed to read session header {}", path.display()))?;
            if available.is_empty() {
                break;
            }
            let remaining = (MAX_HEADER_BYTES + 1) as usize - bytes.len();
            let newline = available.iter().position(|byte| *byte == b'\n');
            let consumed = newline
                .map(|position| position + 1)
                .unwrap_or(available.len())
                .min(remaining);
            bytes.extend_from_slice(&available[..consumed]);
            (
                consumed,
                newline.is_some_and(|position| position < consumed),
            )
        };
        reader.consume(consumed);
        ensure!(
            bytes.len() as u64 <= MAX_HEADER_BYTES,
            "Session header in {} exceeds {MAX_HEADER_BYTES} bytes",
            path.display()
        );
        if found_newline {
            break;
        }
    }
    let line_end = bytes
        .iter()
        .position(|byte| *byte == b'\n')
        .unwrap_or(bytes.len());
    let line = std::str::from_utf8(&bytes[..line_end])
        .with_context(|| format!("Session header {} is not valid UTF-8", path.display()))?;
    match serde_json::from_str::<StoredRecord>(line)
        .with_context(|| format!("Session header {} is not valid JSON", path.display()))?
    {
        StoredRecord::Session {
            version,
            id,
            created_at,
            cwd,
        } => Ok((id, version, created_at, cwd)),
        StoredRecord::Message { .. } => {
            bail!("First record in {} is not a session header", path.display())
        }
    }
}

fn encode_record(record: &StoredRecord) -> Result<Vec<u8>> {
    let mut encoded = serde_json::to_vec(record).context("Failed to encode session record")?;
    encoded.push(b'\n');
    Ok(encoded)
}

fn validate_session_id(id: &str) -> Result<()> {
    Uuid::parse_str(id)
        .with_context(|| format!("Invalid session id {id}"))
        .map(|_| ())
}

fn canonical_cwd(cwd: &Path) -> Result<PathBuf> {
    cwd.canonicalize()
        .with_context(|| format!("Failed to resolve workspace {}", cwd.display()))
}

fn ensure_path_is_in_root(path: &Path, root: &Path) -> Result<()> {
    let parent = path
        .parent()
        .context("Session file has no parent directory")?
        .canonicalize()
        .with_context(|| format!("Failed to resolve session path {}", path.display()))?;
    ensure!(
        parent == root,
        "Session file {} is outside repository root {}",
        path.display(),
        root.display()
    );
    Ok(())
}

fn now_millis() -> Result<u64> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("System clock is before the Unix epoch")?
        .as_millis()
        .try_into()
        .context("Current timestamp does not fit in u64")
}

fn title_from_message(message: &ChatMessage) -> Option<String> {
    if message.role != Role::User {
        return None;
    }
    let title = message
        .content
        .as_deref()?
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    if title.is_empty() {
        return None;
    }
    Some(title.chars().take(80).collect())
}

#[cfg(unix)]
fn set_private_directory_permissions(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;

    fs::set_permissions(path, fs::Permissions::from_mode(0o700)).with_context(|| {
        format!(
            "Failed to set private permissions on session directory {}",
            path.display()
        )
    })
}

#[cfg(not(unix))]
fn set_private_directory_permissions(_path: &Path) -> Result<()> {
    Ok(())
}

#[cfg(unix)]
fn set_private_file_options(options: &mut OpenOptions) {
    use std::os::unix::fs::OpenOptionsExt;

    options.mode(0o600);
}

#[cfg(not(unix))]
fn set_private_file_options(_options: &mut OpenOptions) {}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use tempfile::tempdir;

    use super::*;

    fn message(role: Role, content: &str) -> ChatMessage {
        ChatMessage {
            role,
            content: Some(content.to_string()),
            reasoning: None,
            tool_calls: None,
            tool_call_id: None,
            name: None,
        }
    }

    #[test]
    fn jsonl_repository_round_trips_messages_and_metadata() {
        let root = tempdir().unwrap();
        let workspace = tempdir().unwrap();
        let repository = JsonlSessionRepository::new(root.path()).unwrap();
        let mut session = repository.create(workspace.path()).unwrap();
        repository
            .append(
                &mut session,
                message(Role::User, "  inspect   this project "),
            )
            .unwrap();
        repository
            .append(&mut session, message(Role::Assistant, "done"))
            .unwrap();

        let reopened = repository.open(&session.metadata().id).unwrap();
        assert_eq!(reopened.messages().len(), 2);
        assert_eq!(reopened.chat_messages()[1].content.as_deref(), Some("done"));
        assert_eq!(
            reopened.metadata().title.as_deref(),
            Some("inspect this project")
        );
        assert_eq!(repository.list(workspace.path()).unwrap().len(), 1);
    }

    #[test]
    fn list_is_scoped_to_canonical_workspace() {
        let root = tempdir().unwrap();
        let workspace_a = tempdir().unwrap();
        let workspace_b = tempdir().unwrap();
        let repository = JsonlSessionRepository::new(root.path()).unwrap();
        repository.create(workspace_a.path()).unwrap();
        repository.create(workspace_b.path()).unwrap();

        let sessions = repository.list(workspace_a.path()).unwrap();
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].cwd, workspace_a.path().canonicalize().unwrap());
    }

    #[test]
    fn stale_session_cannot_append_over_external_changes() {
        let root = tempdir().unwrap();
        let workspace = tempdir().unwrap();
        let repository = JsonlSessionRepository::new(root.path()).unwrap();
        let mut first = repository.create(workspace.path()).unwrap();
        let mut stale = repository.open(&first.metadata().id).unwrap();
        repository
            .append(&mut first, message(Role::User, "first"))
            .unwrap();

        let error = repository
            .append(&mut stale, message(Role::User, "stale"))
            .unwrap_err();
        assert!(error.to_string().contains("modified by another process"));
    }

    #[test]
    fn incomplete_final_record_is_repaired_on_next_append() {
        let root = tempdir().unwrap();
        let workspace = tempdir().unwrap();
        let repository = JsonlSessionRepository::new(root.path()).unwrap();
        let session = repository.create(workspace.path()).unwrap();
        let path = session.path().unwrap().to_path_buf();
        OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap()
            .write_all(b"{\"type\":\"message\"")
            .unwrap();

        let mut recovered = repository.open(&session.metadata().id).unwrap();
        assert!(recovered.messages().is_empty());
        repository
            .append(&mut recovered, message(Role::User, "recovered"))
            .unwrap();

        let reopened = repository.open(&session.metadata().id).unwrap();
        assert_eq!(reopened.messages().len(), 1);
        assert_eq!(
            reopened.messages()[0].message.content.as_deref(),
            Some("recovered")
        );
    }

    #[test]
    fn corruption_before_final_line_is_an_error() {
        let root = tempdir().unwrap();
        let workspace = tempdir().unwrap();
        let repository = JsonlSessionRepository::new(root.path()).unwrap();
        let session = repository.create(workspace.path()).unwrap();
        let path = session.path().unwrap();
        let mut file = OpenOptions::new().append(true).open(path).unwrap();
        file.write_all(b"not-json\n").unwrap();

        let error = repository.open(&session.metadata().id).unwrap_err();
        assert!(error.to_string().contains("Invalid session record"));
    }

    #[test]
    fn delete_removes_only_the_selected_session() {
        let root = tempdir().unwrap();
        let workspace = tempdir().unwrap();
        let repository = JsonlSessionRepository::new(root.path()).unwrap();
        let first = repository.create(workspace.path()).unwrap();
        let second = repository.create(workspace.path()).unwrap();

        repository.delete(&first.metadata().id).unwrap();
        assert!(repository.open(&first.metadata().id).is_err());
        assert!(repository.open(&second.metadata().id).is_ok());
    }
}
