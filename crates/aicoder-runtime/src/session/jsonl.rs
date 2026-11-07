use super::*;
#[cfg(test)]
use aicoder_core::model::{Message, MessageRole};
use anyhow::{Context, Result, bail, ensure};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    fs::{self, File, OpenOptions},
    io::{BufRead, BufReader, Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
};
use uuid::Uuid;
const MAX_HEADER_BYTES: u64 = 1024 * 1024;
#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(super) enum StoredRecord {
    Session {
        version: u32,
        id: String,
        created_at: u64,
        cwd: PathBuf,
    },
    Message {
        id: String,
        created_at: u64,
        message: Value,
    },
    Transaction {
        revision: u64,
        updated_at: u64,
        entries: Vec<SessionEntry>,
        leaf_id: Option<String>,
        checksum: String,
    },
}
#[derive(Serialize)]
struct TransactionRef<'a> {
    #[serde(rename = "type")]
    kind: &'static str,
    revision: u64,
    updated_at: u64,
    entries: &'a [SessionEntry],
    leaf_id: &'a Option<String>,
}
#[derive(Clone)]
pub struct JsonlSessionRepository {
    root: PathBuf,
    pub(super) limits: super::SessionLoadLimits,
    pub(super) archive_cache:
        std::sync::Arc<std::sync::Mutex<Option<super::archive::ArchiveIndex>>>,
}
pub(super) struct Lock(pub(super) File);
impl Drop for Lock {
    fn drop(&mut self) {
        let _ = FileExt::unlock(&self.0);
    }
}
impl JsonlSessionRepository {
    pub fn new(root: impl AsRef<Path>) -> Result<Self> {
        fs::create_dir_all(root.as_ref())?;
        let root = root.as_ref().canonicalize()?;
        private_directory(&root)?;
        for name in [".locks", ".runs"] {
            let path = root.join(name);
            fs::create_dir_all(&path)?;
            ensure!(
                !fs::symlink_metadata(&path)?.file_type().is_symlink(),
                "session lock directory must not be a symlink"
            );
            private_directory(&path)?;
        }
        Ok(Self {
            root,
            limits: super::SessionLoadLimits::default(),
            archive_cache: Default::default(),
        })
    }
    pub fn root(&self) -> &Path {
        &self.root
    }
    fn paths(&self) -> Result<Vec<PathBuf>> {
        let mut paths = Vec::new();
        for entry in fs::read_dir(&self.root)? {
            let entry = entry?;
            if entry.file_type()?.is_file()
                && entry.path().extension().is_some_and(|ext| ext == "jsonl")
            {
                super::limits::check(
                    "session files",
                    paths.len().saturating_add(1) as u64,
                    self.limits.max_session_files as u64,
                )?;
                paths.push(entry.path());
            }
        }
        Ok(paths)
    }
    pub(super) fn find(&self, id: &str) -> Result<PathBuf> {
        validate_id(id)?;
        let mut found = None;
        for path in self.paths()? {
            if let Ok((header_id, _, _, _)) = read_header(&path)
                && header_id == id
            {
                ensure!(found.is_none(), "multiple session files use this ID");
                found = Some(path);
            }
        }
        found.context("session not found")
    }
    pub(super) fn metadata_lock(&self, id: &str, exclusive: bool) -> Result<Lock> {
        validate_id(id)?;
        let file = open_lock(&self.root.join(".locks").join(format!("{id}.lock")))?;
        if exclusive {
            FileExt::lock_exclusive(&file)?;
        } else {
            FileExt::lock_shared(&file)?;
        }
        Ok(Lock(file))
    }
    pub(super) fn export_lock(&self, id: &str) -> Result<Lock> {
        validate_id(id)?;
        let file = open_lock(&self.root.join(".locks").join(format!("{id}.lock")))?;
        FileExt::try_lock_shared(&file)
            .context("session metadata is busy; retry export after the writer finishes")?;
        Ok(Lock(file))
    }
    pub(super) fn run_file(&self, id: &str) -> Result<File> {
        validate_id(id)?;
        open_lock(&self.root.join(".runs").join(format!("{id}.lock")))
    }
    // Called under the metadata lock: owner changes and owner-token reads cannot race.
    pub(super) fn write_lease(&self, id: &str, token: Option<&str>) -> Result<Option<Lock>> {
        let mut file = self.run_file(id)?;
        match FileExt::try_lock_exclusive(&file) {
            Ok(()) => Ok(Some(Lock(file))),
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                let mut owner = String::new();
                Read::take(&mut file, 128).read_to_string(&mut owner)?;
                ensure!(
                    token.is_some_and(|token| token == owner),
                    "session already has an active run"
                );
                Ok(None)
            }
            Err(error) => Err(error.into()),
        }
    }
    fn read(&self, path: &Path) -> Result<Vec<u8>> {
        ensure!(
            path.parent() == Some(self.root.as_path()),
            "session path is outside repository"
        );
        let mut file = open_data(path, true, false)?;
        super::archive::read_full(&mut file, &self.limits)
    }
    fn commit_inner(
        &self,
        session: &mut Session,
        transaction: SessionTransaction,
    ) -> Result<Option<PathBuf>> {
        session.validate_transaction(&transaction)?;
        let path = session
            .path()
            .context("session has no storage path")?
            .to_path_buf();
        ensure!(
            path.parent() == Some(self.root.as_path()),
            "session belongs to a different repository"
        );
        let _metadata = self.metadata_lock(&session.metadata().id, true)?;
        let _run = self.write_lease(&session.metadata().id, session.run_token())?;
        let mut file = open_data(&path, true, true)?;
        FileExt::lock_exclusive(&file)?;
        let (actual_len, actual_hash, valid_hash) = super::archive::hash_file(
            &mut file,
            cursor_boundary(session),
            self.limits.max_scan_bytes,
        )?;
        let cursor = session
            .backend_state::<StorageCursor>()
            .context("session view has no JSONL checkpoint")?;
        if actual_len != cursor.expected_len || actual_hash != cursor.fingerprint {
            return Err(SessionConflict {
                id: session.metadata().id.clone(),
            }
            .into());
        }
        let timestamp = if transaction.entries.is_empty()
            && transaction.leaf_id == session.metadata().leaf_id
        {
            session.metadata().updated_at
        } else {
            now_millis()?
        };
        if session.metadata().version < SESSION_VERSION {
            file.seek(SeekFrom::Start(0))?;
            let actual = super::archive::read_full(&mut file, &self.limits)?;
            let mut backup_name = path.as_os_str().to_os_string();
            backup_name.push(format!(".v{}.bak", session.metadata().version));
            let backup = PathBuf::from(backup_name);
            match create_private(&backup) {
                Ok(mut backup_file) => {
                    backup_file.write_all(&actual)?;
                    backup_file.sync_all()?;
                    sync_directory(&self.root)?;
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                    ensure!(
                        self.read_backup(&backup)? == actual,
                        "migration backup already exists with different contents"
                    );
                }
                Err(error) => return Err(error.into()),
            }
            let mut metadata = session.metadata().clone();
            metadata.version = SESSION_VERSION;
            metadata.revision = if session.entries().is_empty() && transaction.entries.is_empty() {
                0
            } else {
                1
            };
            metadata.updated_at = timestamp;
            metadata.leaf_id = transaction.leaf_id;
            let mut entries = session.entries().to_vec();
            entries.extend(transaction.entries);
            let mut upgraded = Session::from_entries(metadata, entries)?;
            upgraded.attach_run_token(session.run_token().map(str::to_string));
            let mut bytes = header(upgraded.metadata())?;
            if !upgraded.entries().is_empty() {
                bytes.extend(transaction_line(
                    1,
                    timestamp,
                    upgraded.entries(),
                    &upgraded.metadata().leaf_id,
                )?);
            }
            atomic_replace(&self.root, &path, &bytes)?;
            attach_cursor(
                &mut upgraded,
                cursor_for(
                    path,
                    bytes.len() as u64,
                    bytes.len() as u64,
                    false,
                    fingerprint(&bytes),
                ),
            );
            *session = upgraded;
            return Ok(Some(backup));
        }
        let revision = session
            .metadata()
            .revision
            .checked_add(1)
            .context("session revision overflow")?;
        let encoded = transaction_line(
            revision,
            timestamp,
            &transaction.entries,
            &transaction.leaf_id,
        )?;
        file.set_len(cursor.valid_len)?;
        file.seek(SeekFrom::End(0))?;
        if cursor.needs_separator {
            file.write_all(b"\n")?;
        }
        file.write_all(&encoded)?;
        file.sync_data()?;
        let mut hash = valid_hash.context("missing valid-prefix fingerprint")?;
        if cursor.needs_separator {
            hash = hash_more(hash, b"\n");
        }
        hash = hash_more(hash, &encoded);
        let length = cursor.valid_len + u64::from(cursor.needs_separator) + encoded.len() as u64;
        session.apply_transaction(transaction, revision, timestamp)?;
        attach_cursor(session, cursor_for(path, length, length, false, hash));
        Ok(None)
    }
    pub(super) fn read_backup(&self, path: &Path) -> Result<Vec<u8>> {
        let mut file = open_data(path, true, false)?;
        super::archive::read_full(&mut file, &self.limits)
    }
}
impl SessionRepository for JsonlSessionRepository {
    fn prepare_fork(
        &self,
        source: &RunSession,
        point: ForkPoint,
        limits: SessionForkLimits,
        context: &aicoder_core::TurnExecutionContext,
    ) -> Result<SessionForkPlan> {
        super::archive::prepare_fork(self, source, point, limits, context)
    }
    fn publish_fork(
        &self,
        source: &RunSession,
        plan: SessionForkPlan,
        context: &aicoder_core::TurnExecutionContext,
    ) -> Result<PublishedSessionFork> {
        super::fork::publish(self, source, plan, context)
    }

    fn extension_state(
        &self,
        id: &str,
        namespace: &str,
        max_bytes: usize,
    ) -> Result<Option<aicoder_core::session::SessionExtensionState>> {
        super::archive::extension_state(self, id, namespace, max_bytes)
    }

    fn create(&self, cwd: &Path) -> Result<Session> {
        let timestamp = now_millis()?;
        let id = Uuid::new_v4().to_string();
        let _metadata = self.metadata_lock(&id, true)?;
        let metadata = SessionMetadata {
            version: SESSION_VERSION,
            id: id.clone(),
            cwd: cwd.canonicalize()?,
            created_at: timestamp,
            updated_at: timestamp,
            title: None,
            leaf_id: None,
            revision: 0,
        };
        let path = self.root.join(format!("{id}.jsonl"));
        let bytes = header(&metadata)?;
        let mut file = create_private(&path)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        sync_directory(&self.root)?;
        let mut session = Session::from_entries(metadata, Vec::new())?;
        attach_cursor(
            &mut session,
            cursor_for(
                path,
                bytes.len() as u64,
                bytes.len() as u64,
                false,
                fingerprint(&bytes),
            ),
        );
        Ok(session)
    }
    fn open(&self, id: &str) -> Result<Session> {
        let _metadata = self.metadata_lock(id, false)?;
        let path = self.find(id)?;
        let session = load_bytes(&path, &self.read(&path)?)?;
        ensure!(session.metadata().id == id, "session identity changed");
        Ok(session)
    }
    fn open_run(&self, id: &str) -> Result<RunSession> {
        let path = self.find(id)?;
        if fs::metadata(&path)?.len()
            <= self
                .limits
                .max_snapshot_bytes
                .min(self.limits.max_window_bytes as u64)
        {
            return self
                .open(id)
                .map(|session| RunSession::Full(Box::new(session)));
        }
        super::archive::open_window(self, id).map(|window| RunSession::Window(Box::new(window)))
    }
    fn append_window(
        &self,
        window: &mut SessionWindow,
        entries: Vec<SessionEntryData>,
    ) -> Result<()> {
        super::archive::append(self, window, entries)
    }
    fn acquire_window(&self, window: &mut SessionWindow) -> Result<SessionRunLease> {
        let _metadata = self.metadata_lock(&window.metadata().id, true)?;
        let mut file = self.run_file(&window.metadata().id)?;
        FileExt::try_lock_exclusive(&file).context("session already has an active run")?;
        let token = Uuid::new_v4().to_string();
        file.set_len(0)?;
        file.write_all(token.as_bytes())?;
        file.flush()?;
        window.attach_run_token(Some(token));
        Ok(Box::new(Lock(file)))
    }
    fn archive_page(
        &self,
        id: &str,
        limit: usize,
        cursor: Option<&ArchiveCursor>,
    ) -> Result<ArchivePage> {
        super::archive::page(self, id, limit, cursor)
    }
    fn session_info(&self, id: &str) -> Result<SessionInfo> {
        super::archive::info(self, id)
    }
    fn prepare_window_navigation(
        &self,
        window: &SessionWindow,
        action: ArchiveNavigation,
    ) -> Result<SessionNavigationPlan> {
        super::archive::prepare_navigation(self, window, action)
    }
    fn commit_window_navigation(
        &self,
        window: &mut SessionWindow,
        plan: SessionNavigationPlan,
    ) -> Result<()> {
        super::archive::commit_navigation(self, window, plan)
    }
    fn navigate_window(&self, window: &mut SessionWindow, action: ArchiveNavigation) -> Result<()> {
        super::archive::navigate(self, window, action)
    }
    fn tree_page(&self, id: &str, limit: usize, cursor: Option<&TreeCursor>) -> Result<TreePage> {
        super::archive::tree(self, id, limit, cursor)
    }
    fn search_page(
        &self,
        id: &str,
        query: &str,
        limit: usize,
        cursor: Option<&SearchCursor>,
    ) -> Result<SearchPage> {
        super::archive::search(self, id, query, limit, cursor)
    }
    fn list(&self, cwd: &Path) -> Result<Vec<SessionInfo>> {
        let cwd = cwd.canonicalize()?;
        let mut sessions = Vec::new();
        for path in self.paths()? {
            let Ok((id, _, _, _)) = read_header(&path) else {
                continue;
            };
            match super::archive::info(self, &id) {
                Ok(session) if session.cwd == cwd => sessions.push(session),
                Ok(_) => {}
                Err(error) => tracing::warn!(error=%error,"skipping invalid session"),
            }
        }
        sort_sessions(&mut sessions);
        Ok(sessions)
    }
    fn commit(&self, session: &mut Session, transaction: SessionTransaction) -> Result<()> {
        self.commit_inner(session, transaction).map(|_| ())
    }
    fn acquire_run(&self, session: &mut Session) -> Result<SessionRunLease> {
        let _metadata = self.metadata_lock(&session.metadata().id, true)?;
        let path = self.find(&session.metadata().id)?;
        ensure!(
            Some(path.as_path()) == session.path(),
            "session path changed"
        );
        let mut file = self.run_file(&session.metadata().id)?;
        FileExt::try_lock_exclusive(&file).context("session already has an active run")?;
        let token = Uuid::new_v4().to_string();
        file.set_len(0)?;
        file.write_all(token.as_bytes())?;
        file.flush()?;
        session.attach_run_token(Some(token));
        Ok(Box::new(Lock(file)))
    }
    fn delete(&self, id: &str) -> Result<()> {
        let _metadata = self.metadata_lock(id, true)?;
        let _run = self.write_lease(id, None)?;
        let path = self.find(id)?;
        fs::remove_file(path)?;
        sync_directory(&self.root)
    }
    fn migrate(&self, session: &mut Session) -> Result<Option<PathBuf>> {
        if session.metadata().version == SESSION_VERSION {
            return Ok(None);
        }
        self.commit_inner(
            session,
            SessionTransaction {
                entries: Vec::new(),
                leaf_id: session.metadata().leaf_id.clone(),
            },
        )
    }
}
pub(super) fn header(metadata: &SessionMetadata) -> Result<Vec<u8>> {
    line(
        &serde_json::json!({"type":"session","version":SESSION_VERSION,"id":metadata.id,"created_at":metadata.created_at,"cwd":metadata.cwd}),
    )
}
fn line(record: &impl Serialize) -> Result<Vec<u8>> {
    let mut bytes = serde_json::to_vec(record)?;
    bytes.push(b'\n');
    Ok(bytes)
}
pub(super) fn transaction_line(
    revision: u64,
    updated_at: u64,
    entries: &[SessionEntry],
    leaf_id: &Option<String>,
) -> Result<Vec<u8>> {
    let mut bytes = serde_json::to_vec(&TransactionRef {
        kind: "transaction",
        revision,
        updated_at,
        entries,
        leaf_id,
    })?;
    bytes.pop();
    let checksum = fingerprint(&bytes);
    writeln!(&mut bytes, ",\"checksum\":\"{checksum:016x}\"}}")?;
    Ok(bytes)
}
pub(super) fn load_bytes(path: &Path, bytes: &[u8]) -> Result<Session> {
    let mut offset = 0;
    let mut valid_len = 0;
    let mut session: Option<Session> = None;
    let mut legacy_entries = Vec::new();
    let mut legacy_leaf = None;
    let mut legacy_updated = 0;
    for raw in bytes.split_inclusive(|byte| *byte == b'\n') {
        let last = offset + raw.len() == bytes.len();
        let parsed = serde_json::from_slice::<Value>(raw);
        let value = match parsed {
            Ok(value) => value,
            Err(_) if last && !raw.ends_with(b"\n") && session.is_some() => break,
            Err(error) => return Err(error).context("Invalid session record"),
        };
        let record: StoredRecord =
            serde_json::from_value(value).context("Invalid session record schema")?;
        match record {
            StoredRecord::Session {
                version,
                id,
                created_at,
                cwd,
            } => {
                ensure!(session.is_none(), "multiple session headers");
                ensure!(
                    (1..=SESSION_VERSION).contains(&version),
                    "unsupported session version {version}"
                );
                validate_id(&id)?;
                ensure!(cwd.is_absolute(), "session workspace must be absolute");
                legacy_updated = created_at;
                session = Some(Session::from_entries(
                    SessionMetadata {
                        version,
                        id,
                        cwd,
                        created_at,
                        updated_at: created_at,
                        title: None,
                        leaf_id: None,
                        revision: 0,
                    },
                    Vec::new(),
                )?);
            }
            StoredRecord::Message {
                id,
                created_at,
                message,
            } => {
                let current = session
                    .as_ref()
                    .context("first record is not a session header")?;
                ensure!(
                    current.metadata().version < SESSION_VERSION,
                    "legacy record in v3 log"
                );
                let message = if current.metadata().version == 1 {
                    super::legacy::decode(message)?
                } else {
                    serde_json::from_value(message)?
                };
                legacy_entries.push(SessionEntry {
                    id: id.clone(),
                    parent_id: legacy_leaf,
                    created_at,
                    data: SessionEntryData::Message { message },
                });
                legacy_leaf = Some(id);
                legacy_updated = legacy_updated.max(created_at);
            }
            StoredRecord::Transaction {
                revision,
                updated_at,
                entries,
                leaf_id,
                checksum,
            } => {
                let current = session
                    .as_mut()
                    .context("first record is not a session header")?;
                ensure!(
                    current.metadata().version == SESSION_VERSION,
                    "transaction in legacy log"
                );
                let marker = b",\"checksum\":\"";
                let position = raw
                    .windows(marker.len())
                    .rposition(|part| part == marker)
                    .context("invalid transaction checksum envelope")?;
                let envelope = format!(",\"checksum\":\"{checksum}\"}}");
                ensure!(
                    checksum.len() == 16 && raw[position..].trim_ascii_end() == envelope.as_bytes(),
                    "invalid transaction checksum envelope"
                );
                ensure!(
                    u64::from_str_radix(&checksum, 16).ok() == Some(fingerprint(&raw[..position])),
                    "session transaction checksum mismatch"
                );
                current.apply_transaction(
                    SessionTransaction { entries, leaf_id },
                    revision,
                    updated_at,
                )?;
            }
        }
        offset += raw.len();
        valid_len = offset;
    }
    let mut session = session.context("empty session file")?;
    if session.metadata().version < SESSION_VERSION {
        let mut metadata = session.metadata().clone();
        metadata.leaf_id = legacy_leaf;
        metadata.updated_at = legacy_updated;
        session = Session::from_entries(metadata, legacy_entries)?;
    }
    attach_cursor(
        &mut session,
        cursor_for(
            path.to_path_buf(),
            bytes.len() as u64,
            valid_len as u64,
            valid_len == bytes.len() && !bytes.ends_with(b"\n"),
            fingerprint(bytes),
        ),
    );
    Ok(session)
}
pub(super) fn read_header(path: &Path) -> Result<(String, u32, u64, PathBuf)> {
    let file = open_data(path, true, false)?;
    let mut bytes = Vec::new();
    BufReader::new(file.take(MAX_HEADER_BYTES + 1)).read_until(b'\n', &mut bytes)?;
    ensure!(
        bytes.len() as u64 <= MAX_HEADER_BYTES,
        "session header too large"
    );
    match serde_json::from_slice::<StoredRecord>(&bytes)? {
        StoredRecord::Session {
            id,
            version,
            created_at,
            cwd,
        } => {
            validate_id(&id)?;
            Ok((id, version, created_at, cwd))
        }
        _ => bail!("first record is not a session header"),
    }
}
pub(super) fn cursor_for(
    path: PathBuf,
    expected_len: u64,
    valid_len: u64,
    needs_separator: bool,
    fingerprint: u64,
) -> StorageCursor {
    StorageCursor {
        path,
        expected_len,
        valid_len,
        needs_separator,
        fingerprint,
    }
}
pub(super) fn validate_id(id: &str) -> Result<()> {
    Uuid::parse_str(id).context("invalid session ID")?;
    Ok(())
}
pub(super) fn fingerprint(bytes: &[u8]) -> u64 {
    hash_more(0xcbf29ce484222325, bytes)
}
pub(super) fn hash_more(mut hash: u64, bytes: &[u8]) -> u64 {
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}
fn private_directory(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}
fn options() -> OpenOptions {
    let mut options = OpenOptions::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    options
}
pub(super) fn create_private_readwrite(path: &Path) -> std::io::Result<File> {
    options().read(true).write(true).create_new(true).open(path)
}
pub(super) fn create_private(path: &Path) -> std::io::Result<File> {
    options().write(true).create_new(true).open(path)
}
pub(super) fn open_data(path: &Path, read: bool, write: bool) -> Result<File> {
    let file = options().read(read).write(write).open(path)?;
    ensure!(
        file.metadata()?.is_file(),
        "session path must be a regular file"
    );
    Ok(file)
}
fn open_lock(path: &Path) -> Result<File> {
    let file = options()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)?;
    ensure!(file.metadata()?.is_file(), "lock must be a regular file");
    Ok(file)
}
pub(super) fn sync_directory(root: &Path) -> Result<()> {
    File::open(root)?.sync_all()?;
    Ok(())
}
fn atomic_replace(root: &Path, path: &Path, bytes: &[u8]) -> Result<()> {
    let temporary = root.join(format!(".session-{}.tmp", Uuid::new_v4()));
    let result = (|| -> Result<()> {
        let mut file = create_private(&temporary)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        fs::rename(&temporary, path)?;
        sync_directory(root)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}
#[cfg(test)]
mod tests {
    use std::io::Write;

    use tempfile::tempdir;

    use super::*;

    fn message(role: MessageRole, content: &str) -> Message {
        Message::text(role, content.to_string())
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
                message(MessageRole::User, "  inspect   this project "),
            )
            .unwrap();
        repository
            .append(&mut session, message(MessageRole::Assistant, "done"))
            .unwrap();

        let reopened = repository.open(&session.metadata().id).unwrap();
        assert_eq!(reopened.messages().len(), 2);
        assert_eq!(reopened.chat_messages()[1].text_content().as_str(), "done");
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
            .append(&mut first, message(MessageRole::User, "first"))
            .unwrap();

        let error = repository
            .append(&mut stale, message(MessageRole::User, "stale"))
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
            .append(&mut recovered, message(MessageRole::User, "recovered"))
            .unwrap();

        let reopened = repository.open(&session.metadata().id).unwrap();
        assert_eq!(reopened.messages().len(), 1);
        assert_eq!(
            reopened.messages()[0].message.text_content().as_str(),
            "recovered"
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

#[cfg(test)]
mod compatibility_tests {
    use super::*;
    use aicoder_core::model::{ContentBlock, ImageSource};
    use serde_json::json;

    #[test]
    fn v1_history_migrates_with_an_exact_backup_and_preserves_entry_ids() {
        let workspace = tempfile::tempdir().unwrap();
        let storage = tempfile::tempdir().unwrap();
        let repository = JsonlSessionRepository::new(storage.path()).unwrap();
        let id = Uuid::new_v4().to_string();
        let path = storage.path().join("legacy.jsonl");
        let records = [
            json!({"type":"session","version":1,"id":id,"created_at":1,"cwd":workspace.path().canonicalize().unwrap()}),
            json!({"type":"message","id":"user","created_at":2,"message":{"role":"user","content":"remember"}}),
            json!({"type":"message","id":"call","created_at":3,"message":{"role":"assistant","content":null,"reasoning":"analysis","tool_calls":[{"id":"c","type":"function","function":{"name":"read_file","arguments":"{\"path\":\"a.rs\"}"}}]}}),
            json!({"type":"message","id":"result","created_at":4,"message":{"role":"tool","tool_call_id":"c","name":"read_file","content":"{\"ok\":true,\"output\":\"text\"}"}}),
        ];
        let original = records
            .iter()
            .map(|record| format!("{record}\n"))
            .collect::<String>();
        fs::write(&path, &original).unwrap();
        let mut session = repository.open(&id).unwrap();
        assert_eq!(session.messages()[0].message.text_content(), "remember");
        assert_eq!(
            session.messages()[1]
                .message
                .tool_calls()
                .next()
                .unwrap()
                .arguments["path"],
            "a.rs"
        );
        assert_eq!(
            session.messages()[2].message.tool_result().unwrap().1["ok"],
            true
        );
        repository
            .append(&mut session, Message::text(MessageRole::Assistant, "done"))
            .unwrap();
        let stored = fs::read_to_string(&path).unwrap();
        assert_ne!(stored, original);
        assert_eq!(
            fs::read_to_string(storage.path().join("legacy.jsonl.v1.bak")).unwrap(),
            original
        );
        assert_eq!(session.metadata().version, 3);
        assert_eq!(session.messages()[0].id, "user");
        assert_eq!(session.model_messages().unwrap().len(), 4);
        assert_eq!(repository.open(&id).unwrap().messages().len(), 4);

        let image = Message {
            role: MessageRole::User,
            content: vec![ContentBlock::Image {
                source: ImageSource::Url {
                    url: "https://example.com/a.png".into(),
                },
            }],
        };
        repository.append(&mut session, image.clone()).unwrap();
        assert_eq!(repository.open(&id).unwrap().messages()[4].message, &image);
    }

    #[test]
    fn v3_preserves_images_reasoning_and_scoped_provider_state() {
        let workspace = tempfile::tempdir().unwrap();
        let storage = tempfile::tempdir().unwrap();
        let repository = JsonlSessionRepository::new(storage.path()).unwrap();
        let mut session = repository.create(workspace.path()).unwrap();
        assert_eq!(session.metadata().version, 3);
        let user = Message {
            role: MessageRole::User,
            content: vec![
                ContentBlock::Text {
                    text: "inspect".into(),
                },
                ContentBlock::Image {
                    source: ImageSource::Base64 {
                        media_type: "image/png".into(),
                        data: "fixture".into(),
                    },
                },
            ],
        };
        let assistant = Message {
            role: MessageRole::Assistant,
            content: vec![
                ContentBlock::Reasoning {
                    text: "analysis".into(),
                },
                ContentBlock::Text {
                    text: "answer".into(),
                },
                ContentBlock::ProviderData {
                    provider: "custom".into(),
                    data: json!({"signature":"opaque"}),
                },
            ],
        };
        repository
            .append_all(&mut session, [user.clone(), assistant.clone()])
            .unwrap();
        let reopened = repository.open(&session.metadata().id).unwrap();
        assert_eq!(reopened.messages()[0].message, &user);
        assert_eq!(reopened.messages()[1].message, &assistant);
    }
}

fn cursor_boundary(session: &Session) -> Option<u64> {
    session
        .backend_state::<StorageCursor>()
        .map(|cursor| cursor.valid_len)
}
impl std::fmt::Debug for JsonlSessionRepository {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("JsonlSessionRepository")
            .field("root", &self.root)
            .field("limits", &self.limits)
            .finish_non_exhaustive()
    }
}
impl JsonlSessionRepository {
    pub fn with_limits(root: impl AsRef<Path>, limits: super::SessionLoadLimits) -> Result<Self> {
        limits.validate()?;
        let mut repository = Self::new(root)?;
        repository.limits = limits;
        Ok(repository)
    }
    pub fn limits(&self) -> &super::SessionLoadLimits {
        &self.limits
    }
}
