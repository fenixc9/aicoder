//! Streaming archive validation with a bounded metadata index and materialized run tail.
//! Index messages are private validation descriptors, never canonical messages or prompts.
use super::*;
use super::{jsonl::*, limits::check};
use aicoder_core::{
    model::*,
    summary::{SUMMARY_NAMESPACE, SummaryCheckpoint},
};
use anyhow::{Context, Result, bail, ensure};
use serde_json::json;
use std::{
    fs::File,
    io::{BufRead, BufReader, Read, Seek, SeekFrom, Write},
    path::Path,
};
#[derive(Clone, Copy)]
struct Location {
    offset: u64,
    ordinal: usize,
}
#[derive(Clone)]
pub(super) struct ArchiveIndex {
    path: PathBuf,
    session: Session,
    locations: Vec<Location>,
    expected_len: u64,
    valid_len: u64,
    hash: u64,
    valid_hash: u64,
    separator: bool,
    index_bytes: usize,
    title: Option<String>,
}
#[derive(Clone)]
struct WindowCursor {
    cursor: StorageCursor,
}
pub(super) fn hash_file(
    file: &mut File,
    boundary: Option<u64>,
    limit: u64,
) -> Result<(u64, u64, Option<u64>)> {
    check("scan bytes", file.metadata()?.len(), limit)?;
    file.seek(SeekFrom::Start(0))?;
    let mut buf = [0u8; 8192];
    let mut length = 0u64;
    let mut hash = fingerprint(&[]);
    let mut prefix = hash;
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        check("scan bytes", length.saturating_add(n as u64), limit)?;
        hash = hash_more(hash, &buf[..n]);
        if let Some(boundary) = boundary {
            let count = boundary.saturating_sub(length).min(n as u64) as usize;
            prefix = hash_more(prefix, &buf[..count]);
        }
        length += n as u64;
    }
    Ok((
        length,
        hash,
        boundary
            .filter(|boundary| *boundary <= length)
            .map(|_| prefix),
    ))
}
pub(super) fn read_full(file: &mut File, limits: &SessionLoadLimits) -> Result<Vec<u8>> {
    check(
        "snapshot bytes",
        file.metadata()?.len(),
        limits.max_snapshot_bytes,
    )?;
    let mut bytes = Vec::new();
    file.take(limits.max_snapshot_bytes + 1)
        .read_to_end(&mut bytes)?;
    check(
        "snapshot bytes",
        bytes.len() as u64,
        limits.max_snapshot_bytes,
    )?;
    for line in bytes.split_inclusive(|byte| *byte == b'\n') {
        check(
            "record bytes",
            line.len() as u64,
            limits.max_record_bytes as u64,
        )?;
    }
    Ok(bytes)
}
fn line(reader: &mut BufReader<File>, limit: usize) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    loop {
        let available = reader.fill_buf()?;
        if available.is_empty() {
            break;
        }
        let n = available
            .iter()
            .position(|byte| *byte == b'\n')
            .map_or(available.len(), |n| n + 1);
        check(
            "record bytes",
            bytes.len().saturating_add(n) as u64,
            limit as u64,
        )?;
        let done = available[n - 1] == b'\n';
        bytes.extend_from_slice(&available[..n]);
        reader.consume(n);
        if done {
            break;
        }
    }
    Ok(bytes)
}
fn descriptor(entry: &SessionEntry) -> SessionEntry {
    let data = match &entry.data {
        SessionEntryData::CustomMessage { custom } => SessionEntryData::CustomMessage {
            custom: aicoder_core::CustomMessage::new(custom.source(), custom.custom_type(), vec![])
                .expect("validated custom source")
                .with_display(custom.display()),
        },
        SessionEntryData::NativeContext { source, .. } => {
            SessionEntryData::native_context(source.clone(), "").expect("validated native source")
        }
        SessionEntryData::Message { message } => SessionEntryData::Message {
            message: Message {
                role: message.role,
                content: message
                    .content
                    .iter()
                    .filter_map(|block| match block {
                        ContentBlock::ToolCall { call } => Some(ContentBlock::ToolCall {
                            call: ModelToolCall {
                                id: call.id.clone(),
                                name: call.name.clone(),
                                arguments: json!({}),
                            },
                        }),
                        ContentBlock::ToolResult {
                            call_id,
                            is_error,
                            content,
                        } => Some(ContentBlock::ToolResult {
                            call_id: call_id.clone(),
                            is_error: *is_error,
                            content: if content["error"]["code"] == "execution_interrupted" {
                                json!({"error":{"code":"execution_interrupted"}})
                            } else {
                                json!({})
                            },
                        }),
                        _ => None,
                    })
                    .chain(
                        (message.role != MessageRole::Tool
                            || message
                                .content
                                .iter()
                                .any(|block| !matches!(block, ContentBlock::ToolResult { .. })))
                        .then(|| ContentBlock::Text {
                            text: String::new(),
                        }),
                    )
                    .collect(),
            },
        },
        SessionEntryData::TurnEnd { turn_id, record } => SessionEntryData::TurnEnd {
            turn_id: turn_id.clone(),
            record: Box::new(TurnRecord {
                pending_inputs: Vec::new(),
                partial_response: None,
                stop: record.stop.clone(),
                statistics: record.statistics.clone(),
            }),
        },
        SessionEntryData::Extension { namespace, data } if namespace == CONTEXT_EDIT_NAMESPACE => {
            let mut record: ContextEditRecord =
                serde_json::from_value(data.clone()).expect("validated context edit");
            record.replacement = ContextReplacement::Restore;
            record.data()
        }
        SessionEntryData::Extension { namespace, data } if namespace == SUMMARY_NAMESPACE => {
            let mut checkpoint: SummaryCheckpoint =
                serde_json::from_value(data.clone()).expect("validated summary");
            checkpoint.draft.summary = aicoder_core::summary::ConversationSummary {
                goals: vec!["[private index descriptor]".into()],
                ..Default::default()
            };
            checkpoint.data()
        }
        SessionEntryData::Extension { namespace, data } if namespace == "aicoder.input_context" => {
            SessionEntryData::Extension {
                namespace: namespace.clone(),
                data: json!({}),
            }
        }
        SessionEntryData::Extension { namespace, .. }
            if namespace == aicoder_core::session::REASONING_NAMESPACE =>
        {
            entry.data.clone()
        }
        SessionEntryData::Extension { namespace, data }
            if namespace != aicoder_core::session::NAVIGATION_NAMESPACE
                && namespace != aicoder_core::summary::MANUAL_USAGE_NAMESPACE =>
        {
            let _ = data;
            SessionEntryData::Extension {
                namespace: namespace.clone(),
                data: json!({}),
            }
        }
        SessionEntryData::Compaction {
            strategy,
            preserve_from,
            usage,
            ..
        } => SessionEntryData::Compaction {
            strategy: strategy.clone(),
            preserve_from: preserve_from.clone(),
            usage: usage.clone(),
            summary: String::new(),
        },
        data => data.clone(),
    };
    SessionEntry {
        id: entry.id.clone(),
        parent_id: entry.parent_id.clone(),
        created_at: entry.created_at,
        data,
    }
}
fn accept(
    index: &mut ArchiveIndex,
    entries: Vec<SessionEntry>,
    leaf: Option<String>,
    revision: u64,
    updated: u64,
    offset: u64,
    limits: &SessionLoadLimits,
) -> Result<()> {
    check(
        "index entries",
        index.locations.len().saturating_add(entries.len()) as u64,
        limits.max_index_entries as u64,
    )?;
    let real = SessionTransaction {
        entries,
        leaf_id: leaf,
    };
    index.session.validate_transaction(&real)?;
    let mut headers = Vec::with_capacity(real.entries.len());
    for (ordinal, entry) in real.entries.iter().enumerate() {
        if index.title.is_none()
            && let SessionEntryData::Message { message } = &entry.data
            && message.role == MessageRole::User
        {
            let mut title = String::new();
            let mut pending_space = false;
            let mut count = 0usize;
            for ch in message
                .content
                .iter()
                .filter_map(|block| {
                    if let ContentBlock::Text { text } = block {
                        Some(text.as_str())
                    } else {
                        None
                    }
                })
                .flat_map(str::chars)
            {
                if ch.is_whitespace() {
                    pending_space = !title.is_empty();
                    continue;
                }
                if pending_space {
                    title.push(' ');
                    count += 1;
                    pending_space = false;
                    if count == 80 {
                        break;
                    }
                }
                title.push(ch);
                count += 1;
                if count == 80 {
                    break;
                }
            }
            if !title.is_empty() {
                index.title = Some(title);
            }
        }
        let header = descriptor(entry);
        index.index_bytes = index.index_bytes.saturating_add(
            serde_json::to_vec(&header)?
                .len()
                .saturating_mul(2)
                .saturating_add(256),
        );
        check(
            "index bytes",
            index.index_bytes as u64,
            limits.max_index_bytes as u64,
        )?;
        headers.push(header);
        index.locations.push(Location { offset, ordinal });
    }
    index.session.apply_transaction(
        SessionTransaction {
            entries: headers,
            leaf_id: real.leaf_id,
        },
        revision,
        updated,
    )?;
    Ok(())
}
fn scan(path: &Path, limits: &SessionLoadLimits) -> Result<ArchiveIndex> {
    scan_file(path, limits, open_data(path, true, false)?, None)
}
fn scan_file(
    path: &Path,
    limits: &SessionLoadLimits,
    file: File,
    context: Option<&aicoder_core::TurnExecutionContext>,
) -> Result<ArchiveIndex> {
    check("scan bytes", file.metadata()?.len(), limits.max_scan_bytes)?;
    let mut reader = BufReader::with_capacity(8192, file);
    let raw = line(&mut reader, limits.max_record_bytes)?;
    let header: StoredRecord = serde_json::from_slice(&raw).context("invalid session header")?;
    let StoredRecord::Session {
        version,
        id,
        created_at,
        cwd,
    } = header
    else {
        bail!("first archive record is not a session header")
    };
    ensure!(
        version == SESSION_VERSION,
        "large legacy archives require explicit migration before window loading"
    );
    validate_id(&id)?;
    let session = Session::from_entries(
        SessionMetadata {
            version,
            id,
            created_at,
            updated_at: created_at,
            cwd,
            title: None,
            leaf_id: None,
            revision: 0,
        },
        Vec::new(),
    )?;
    let mut index = ArchiveIndex {
        path: path.to_path_buf(),
        session,
        locations: Vec::new(),
        expected_len: raw.len() as u64,
        valid_len: raw.len() as u64,
        hash: fingerprint(&raw),
        valid_hash: fingerprint(&raw),
        separator: !raw.ends_with(b"\n"),
        index_bytes: 0,
        title: None,
    };
    loop {
        if let Some(context) = context
            && context.is_cancelled()
        {
            return Err(context.error().into());
        }
        let offset = index.expected_len;
        let raw = line(&mut reader, limits.max_record_bytes)?;
        if raw.is_empty() {
            break;
        }
        check(
            "scan bytes",
            index.expected_len.saturating_add(raw.len() as u64),
            limits.max_scan_bytes,
        )?;
        index.expected_len += raw.len() as u64;
        index.hash = hash_more(index.hash, &raw);
        let parsed = serde_json::from_slice::<serde_json::Value>(&raw);
        let value = match parsed {
            Ok(value) => value,
            Err(_) if !raw.ends_with(b"\n") && reader.fill_buf()?.is_empty() => break,
            Err(error) => return Err(error).context("invalid archive record"),
        };
        let record: StoredRecord =
            serde_json::from_value(value).context("invalid archive record schema")?;
        let StoredRecord::Transaction {
            revision,
            updated_at,
            entries,
            leaf_id,
            checksum,
        } = record
        else {
            bail!("non-transaction in v3 archive")
        };
        let marker = b",\"checksum\":\"";
        let position = raw
            .windows(marker.len())
            .rposition(|part| part == marker)
            .context("invalid transaction checksum envelope")?;
        let suffix = format!(",\"checksum\":\"{checksum}\"}}");
        ensure!(
            checksum.len() == 16 && raw[position..].trim_ascii_end() == suffix.as_bytes(),
            "invalid transaction checksum envelope"
        );
        ensure!(
            u64::from_str_radix(&checksum, 16).ok() == Some(fingerprint(&raw[..position])),
            "session transaction checksum mismatch"
        );
        accept(
            &mut index, entries, leaf_id, revision, updated_at, offset, limits,
        )?;
        index.valid_len = index.expected_len;
        index.valid_hash = index.hash;
        index.separator = !raw.ends_with(b"\n");
    }
    Ok(index)
}
fn cached<'a>(
    repository: &JsonlSessionRepository,
    id: &str,
    cache: &'a mut Option<ArchiveIndex>,
) -> Result<&'a mut ArchiveIndex> {
    let path = repository.find(id)?;
    let mut file = open_data(&path, true, false)?;
    let (length, hash, _) = hash_file(&mut file, None, repository.limits.max_scan_bytes)?;
    if cache.as_ref().is_none_or(|index| {
        index.path != path || index.expected_len != length || index.hash != hash
    }) {
        *cache = Some(scan(&path, &repository.limits)?);
    }
    Ok(cache.as_mut().expect("archive index"))
}
fn metadata(index: &ArchiveIndex) -> SessionMetadata {
    let mut metadata = index.session.metadata().clone();
    if metadata.title.is_none() {
        metadata.title = index.title.clone();
    }
    metadata
}
struct Reader {
    reader: BufReader<File>,
    record: Option<(u64, Vec<SessionEntry>)>,
    limit: usize,
}
impl Reader {
    fn new(path: &Path, limit: usize) -> Result<Self> {
        Ok(Self {
            reader: BufReader::with_capacity(8192, open_data(path, true, false)?),
            record: None,
            limit,
        })
    }
    fn entry(&mut self, location: Location) -> Result<SessionEntry> {
        if self
            .record
            .as_ref()
            .is_none_or(|(offset, _)| *offset != location.offset)
        {
            self.reader.seek(SeekFrom::Start(location.offset))?;
            let raw = line(&mut self.reader, self.limit)?;
            let StoredRecord::Transaction { entries, .. } = serde_json::from_slice(&raw)? else {
                bail!("archive entry location changed")
            };
            self.record = Some((location.offset, entries));
        }
        self.record
            .as_ref()
            .unwrap()
            .1
            .get(location.ordinal)
            .cloned()
            .context("archive entry ordinal changed")
    }
}
struct ArchiveProjection {
    messages: Vec<Message>,
    source: SessionContextSource,
    entry_ids: Option<Vec<Option<String>>>,
}
/// Normal window loads borrow origins; only explicit lineage requests allocate owned IDs.
fn project_loaded(
    index: &ArchiveIndex,
    loaded: &[SessionEntry],
    active_turn: Option<&str>,
    lineage: bool,
) -> Result<ArchiveProjection> {
    let mut projection_entries = index.session.entries().to_vec();
    let loaded_ids = loaded
        .iter()
        .map(|e| e.id.as_str())
        .collect::<std::collections::HashSet<_>>();
    for entry in loaded {
        let ordinal = index
            .session
            .entry_position(&entry.id)
            .context("materialized entry missing")?;
        projection_entries[ordinal] = entry.clone();
    }
    let projection = Session::from_entries(metadata(index), projection_entries)?;
    let branch = projection.selected_branch();
    let projected = match active_turn {
        Some(turn) => branch.context_for_active_turn(turn)?,
        None => branch.model_context()?,
    };
    let total = index.session.selected_branch().messages().count();
    let loaded_messages = loaded
        .iter()
        .filter(|e| e.data.as_message().is_some())
        .count();
    let windowed = projected.source.summary_checkpoint.is_none() && loaded_messages < total;
    let offset = if windowed {
        let first_ordinal = loaded
            .iter()
            .filter(|e| e.data.as_message().is_some())
            .filter_map(|e| index.session.entry_position(&e.id))
            .min()
            .context("missing raw window boundary")?;
        projected
            .origins
            .iter()
            .position(|o| {
                o.recorded_entry_id()
                    .and_then(|id| index.session.entry_position(id))
                    .is_some_and(|i| i >= first_ordinal)
            })
            .unwrap_or(projected.messages.len())
    } else {
        0
    };
    ensure!(
        projected.origins[offset..]
            .iter()
            .filter_map(ContextOrigin::recorded_entry_id)
            .all(|id| loaded_ids.contains(id))
            && projected
                .source
                .summary_checkpoint
                .as_deref()
                .is_none_or(|id| loaded_ids.contains(id)),
        "private archive descriptor escaped the materialized view"
    );
    let mut entry_ids = lineage.then(|| {
        projected.origins[offset..]
            .iter()
            .map(|o| o.recorded_entry_id().map(str::to_owned))
            .collect::<Vec<_>>()
    });
    let uncertain = windowed
        && projected.messages[..offset]
            .iter()
            .any(|m| m.role == MessageRole::System);
    let mut messages = projected.messages;
    if windowed {
        messages = messages.split_off(offset);
        messages.insert(0,Message::text(MessageRole::System,format!("[Archive window omitted {} earlier recorded messages to honor the host load budget. Original records remain available through archive pages; inspect them before relying on missing details.]",total-loaded_messages)));
        if let Some(ids) = &mut entry_ids {
            ids.insert(0, None);
        }
        if uncertain {
            messages.insert(0, aicoder_core::summary::recovery_notice());
            if let Some(ids) = &mut entry_ids {
                ids.insert(0, None);
            }
        }
    }
    let mut source = projected.source;
    source.archived_messages = total
        .checked_sub(loaded_messages)
        .context("invalid materialized raw count")?;
    source.recorded_messages = total;
    Ok(ArchiveProjection {
        messages,
        source,
        entry_ids,
    })
}
pub(super) fn project_window_context(
    repository: &JsonlSessionRepository,
    window: &SessionWindow,
    active_turn: Option<&str>,
) -> Result<OwnedSessionContext> {
    let _lock = repository.metadata_lock(&window.metadata().id, false)?;
    let mut cache = repository
        .archive_cache
        .lock()
        .map_err(|_| anyhow::anyhow!("archive cache poisoned"))?;
    let index = cached(repository, &window.metadata().id, &mut cache)?;
    let cursor = window
        .backend_state::<WindowCursor>()
        .context("window has no backend checkpoint")?;
    ensure!(
        index.path == cursor.cursor.path
            && index.expected_len == cursor.cursor.expected_len
            && index.hash == cursor.cursor.fingerprint
            && index.session.metadata().revision == window.metadata().revision
            && index.session.metadata().leaf_id == window.metadata().leaf_id,
        "stale window projection"
    );
    // No cold payload reads or enlarged load budget: only real records already admitted to this run.
    let projected = project_loaded(index, window.entries(), active_turn, true)?;
    Ok(OwnedSessionContext {
        messages: projected.messages,
        source: projected.source,
        entry_ids: projected.entry_ids.expect("requested context lineage"),
    })
}
fn materialize(index: &ArchiveIndex, limits: &SessionLoadLimits) -> Result<SessionWindow> {
    let mut reader = Reader::new(&index.path, limits.max_record_bytes)?;
    materialize_using(index, limits, |location| reader.entry(location))
}
fn materialize_using(
    index: &ArchiveIndex,
    limits: &SessionLoadLimits,
    mut read: impl FnMut(Location) -> Result<SessionEntry>,
) -> Result<SessionWindow> {
    let branch = index.session.selected_branch();
    // Validate historical tool identities/turn boundaries using metadata descriptors only.
    branch.model_context()?;
    let selected = branch.entries();
    let summary=selected.iter().rev().find(|entry|matches!(&entry.data,SessionEntryData::Extension {namespace,..} if namespace==SUMMARY_NAMESPACE));
    let summary_start = summary
        .map(|entry| {
            let SessionEntryData::Extension { data, .. } = &entry.data else {
                unreachable!()
            };
            serde_json::from_value::<SummaryCheckpoint>(data.clone())
                .map(|checkpoint| checkpoint.first_kept_entry_id)
        })
        .transpose()?;
    let mut loaded = Vec::new();
    let mut bytes = 0usize;
    let mut messages = 0usize;
    for entry in selected.iter().rev() {
        if entry.data.as_message().is_none()
            && !matches!(&entry.data,SessionEntryData::Extension {namespace,..} if namespace==SUMMARY_NAMESPACE)
        {
            continue;
        }
        let ordinal = index
            .session
            .entry_position(&entry.id)
            .expect("indexed entry");
        let real = read(index.locations[ordinal])?;
        let size = serde_json::to_vec(&real)?.len();
        let is_message = real.data.as_message().is_some();
        if bytes.saturating_add(size) > limits.max_window_bytes
            || messages + usize::from(is_message) > limits.max_window_messages
        {
            if summary_start.is_some() {
                return Err(super::SessionLoadLimit {
                    resource: "summary tail bytes/messages",
                    limit: limits.max_window_bytes as u64,
                    requested: bytes.saturating_add(size) as u64,
                }
                .into());
            }
            break;
        }
        bytes += size;
        messages += usize::from(is_message);
        let done = summary_start.as_ref().is_some_and(|id| *id == real.id);
        loaded.push(real);
        if done {
            break;
        }
    }
    loaded.reverse();
    // An unpaired leading result is omitted together with its whole older call unit.
    if summary_start.is_none() {
        while loaded.first().is_some_and(|entry| {
            !entry
                .data
                .as_message()
                .is_some_and(|message| message.role != MessageRole::Tool)
        }) {
            loaded.remove(0);
        }
    }
    ensure!(
        index.session.messages().is_empty()
            || loaded.iter().any(|entry| entry.data.as_message().is_some()),
        "newest complete message unit exceeds the window budget"
    );
    let loaded_ids = if selected.iter().any(|e|matches!(&e.data, SessionEntryData::Extension { namespace, .. } if namespace==CONTEXT_EDIT_NAMESPACE)) {
        loaded
        .iter()
        .filter(|e| e.data.as_message().is_some())
        .map(|e| e.id.as_str())
        .collect::<std::collections::HashSet<_>>()
    } else { std::collections::HashSet::new() };
    let mut latest_edits = std::collections::HashMap::new();
    for entry in selected {
        if let SessionEntryData::Extension { namespace, data } = &entry.data
            && namespace == CONTEXT_EDIT_NAMESPACE
        {
            let record: ContextEditRecord = serde_json::from_value(data.clone())?;
            if loaded_ids.contains(record.target_id.as_str()) {
                latest_edits.insert(record.target_id, entry);
            }
        }
    }
    let mut extra_edits = Vec::new();
    for entry in latest_edits.values() {
        let ordinal = index
            .session
            .entry_position(&entry.id)
            .expect("indexed edit");
        let real = read(index.locations[ordinal])?;
        bytes = bytes.saturating_add(serde_json::to_vec(&real)?.len());
        check(
            "context edit window bytes",
            bytes as u64,
            limits.max_window_bytes as u64,
        )?;
        extra_edits.push(real);
    }
    if !extra_edits.is_empty() {
        loaded.extend(extra_edits);
        loaded.sort_by_key(|entry| {
            index
                .session
                .entry_position(&entry.id)
                .expect("real window entry")
        });
    }
    let projected = project_loaded(index, &loaded, None, false)?;
    let source = projected.source;
    let context = projected.messages;
    let model = index
        .session
        .selected_model()
        .map(|(provider, model)| (provider.to_string(), model.to_string()));
    let unfinished = index.session.unfinished_turn().map(str::to_string);
    let mut window = SessionWindow::new(
        metadata(index),
        loaded,
        context,
        source,
        model,
        unfinished,
        index.session.selected_branch().accounting()?,
    )?
    .with_reasoning(index.session.reasoning_selection()?)?;
    window.attach_backend_state(WindowCursor {
        cursor: cursor_for(
            index.path.clone(),
            index.expected_len,
            index.valid_len,
            index.separator,
            index.hash,
        ),
    });
    Ok(window)
}
pub(super) fn open_window(repository: &JsonlSessionRepository, id: &str) -> Result<SessionWindow> {
    let _lock = repository.metadata_lock(id, false)?;
    let mut cache = repository
        .archive_cache
        .lock()
        .map_err(|_| anyhow::anyhow!("archive cache poisoned"))?;
    let index = cached(repository, id, &mut cache)?;
    materialize(index, &repository.limits)
}
pub(super) fn info(repository: &JsonlSessionRepository, id: &str) -> Result<SessionInfo> {
    let path = repository.find(id)?;
    if super::jsonl::read_header(&path)?.1 < SESSION_VERSION {
        return repository
            .open(id)
            .map(|session| SessionInfo::from(&session));
    }
    let _lock = repository.metadata_lock(id, false)?;
    let mut cache = repository
        .archive_cache
        .lock()
        .map_err(|_| anyhow::anyhow!("archive cache poisoned"))?;
    let index = cached(repository, id, &mut cache)?;
    let mut info = SessionInfo::from(&index.session);
    info.title = metadata(index).title;
    info.path = Some(index.path.clone());
    Ok(info)
}
pub(super) fn append(
    repository: &JsonlSessionRepository,
    window: &mut SessionWindow,
    data: Vec<SessionEntryData>,
) -> Result<()> {
    let mut parent = window.metadata().leaf_id.clone();
    let now = now_millis()?;
    let entries = data
        .into_iter()
        .map(|data| {
            let id = uuid::Uuid::new_v4().to_string();
            let entry = SessionEntry {
                id: id.clone(),
                parent_id: parent.clone(),
                created_at: now,
                data,
            };
            parent = Some(id);
            entry
        })
        .collect();
    commit_transaction(
        repository,
        window,
        SessionTransaction {
            entries,
            leaf_id: parent,
        },
        false,
    )
}
fn commit_transaction(
    repository: &JsonlSessionRepository,
    window: &mut SessionWindow,
    transaction: SessionTransaction,
    rebuild: bool,
) -> Result<()> {
    let id = window.metadata().id.clone();
    let _metadata = repository.metadata_lock(&id, true)?;
    let _run = repository.write_lease(&id, window.run_token())?;
    let cursor = window
        .backend_state::<WindowCursor>()
        .context("window has no backend checkpoint")?
        .cursor
        .clone();
    ensure!(
        cursor.path.parent() == Some(repository.root()),
        "window belongs to another repository"
    );
    let mut file = open_data(&cursor.path, true, true)?;
    fs2::FileExt::lock_exclusive(&file)?;
    let (length, hash, _) = hash_file(
        &mut file,
        Some(cursor.valid_len),
        repository.limits.max_scan_bytes,
    )?;
    if length != cursor.expected_len || hash != cursor.fingerprint {
        return Err(SessionConflict { id }.into());
    }
    let mut cache = repository
        .archive_cache
        .lock()
        .map_err(|_| anyhow::anyhow!("archive cache poisoned"))?;
    let index = cached(repository, &id, &mut cache)?;
    ensure!(index.path == cursor.path, "window repository path changed");
    ensure!(
        index.session.metadata().revision == window.metadata().revision,
        "window revision conflict"
    );
    let entries = transaction.entries.clone();
    let parent = transaction.leaf_id.clone();
    let now = now_millis()?;
    index.session.validate_transaction(&transaction)?;
    check(
        "index entries",
        index.locations.len().saturating_add(entries.len()) as u64,
        repository.limits.max_index_entries as u64,
    )?;
    let revision = window.metadata().revision + 1;
    let encoded = transaction_line(revision, now, &entries, &parent)?;
    check(
        "record bytes",
        encoded.len() as u64,
        repository.limits.max_record_bytes as u64,
    )?;
    let added = entries
        .iter()
        .map(|entry| {
            serde_json::to_vec(&descriptor(entry))
                .map(|bytes| bytes.len().saturating_mul(2).saturating_add(256))
        })
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .sum::<usize>();
    check(
        "index bytes",
        index.index_bytes.saturating_add(added) as u64,
        repository.limits.max_index_bytes as u64,
    )?;
    file.set_len(index.valid_len)?;
    file.seek(SeekFrom::End(0))?;
    if index.separator {
        file.write_all(b"\n")?;
    }
    let offset = index.valid_len + u64::from(index.separator);
    file.write_all(&encoded)?;
    file.sync_data()?;
    let mut hash = index.valid_hash;
    if index.separator {
        hash = hash_more(hash, b"\n");
    }
    hash = hash_more(hash, &encoded);
    accept(
        index,
        entries.clone(),
        parent,
        revision,
        now,
        offset,
        &repository.limits,
    )?;
    index.expected_len = offset + encoded.len() as u64;
    index.valid_len = index.expected_len;
    index.hash = hash;
    index.valid_hash = hash;
    index.separator = false;
    let model = index
        .session
        .selected_model()
        .map(|(provider, model)| (provider.to_string(), model.to_string()));
    let unfinished = index.session.unfinished_turn().map(str::to_string);
    if rebuild {
        let token = window.run_token().map(str::to_string);
        let mut fresh = materialize(index, &repository.limits)?;
        fresh.attach_run_token(token);
        *window = fresh;
    } else {
        window.apply_committed(
            metadata(index),
            entries,
            model,
            unfinished,
            index.session.selected_branch().accounting()?,
        )?;
    }
    window.attach_backend_state(WindowCursor {
        cursor: cursor_for(
            index.path.clone(),
            index.expected_len,
            index.valid_len,
            false,
            hash,
        ),
    });
    Ok(())
}
pub(super) fn page(
    repository: &JsonlSessionRepository,
    id: &str,
    limit: usize,
    cursor: Option<&ArchiveCursor>,
) -> Result<ArchivePage> {
    ensure!(
        (1..=100).contains(&limit),
        "archive page limit must be 1..100"
    );
    let _lock = repository.metadata_lock(id, false)?;
    let mut cache = repository
        .archive_cache
        .lock()
        .map_err(|_| anyhow::anyhow!("archive cache poisoned"))?;
    let index = cached(repository, id, &mut cache)?;
    let offset = if let Some(cursor) = cursor {
        ensure!(
            cursor.session_id == id && cursor.revision == index.session.metadata().revision,
            "stale or foreign archive cursor"
        );
        cursor.offset
    } else {
        0
    };
    ensure!(offset <= index.locations.len(), "invalid archive offset");
    let mut reader = Reader::new(&index.path, repository.limits.max_record_bytes)?;
    let mut entries = Vec::new();
    let mut end = offset;
    while end < index.locations.len() && entries.len() < limit {
        let entry = reader.entry(index.locations[end])?;
        entries.push(entry);
        let candidate = ArchivePage {
            metadata: metadata(index),
            entries: entries.clone(),
            next: Some(ArchiveCursor {
                session_id: id.into(),
                revision: index.session.metadata().revision,
                offset: end + 1,
            }),
        };
        if serde_json::to_vec(&candidate)?.len() > repository.limits.max_page_bytes {
            entries.pop();
            ensure!(
                !entries.is_empty(),
                "archive page budget cannot hold the next complete record"
            );
            break;
        }
        end += 1;
    }
    Ok(ArchivePage {
        metadata: metadata(index),
        entries,
        next: (end < index.locations.len()).then(|| ArchiveCursor {
            session_id: id.into(),
            revision: index.session.metadata().revision,
            offset: end,
        }),
    })
}

pub(super) fn tree(
    repository: &JsonlSessionRepository,
    id: &str,
    limit: usize,
    cursor: Option<&TreeCursor>,
) -> Result<TreePage> {
    let _lock = repository.metadata_lock(id, false)?;
    let mut cache = repository
        .archive_cache
        .lock()
        .map_err(|_| anyhow::anyhow!("archive cache poisoned"))?;
    let index = cached(repository, id, &mut cache)?;
    let mut page = index.session.tree_page(limit, cursor)?;
    let mut reader = Reader::new(&index.path, repository.limits.max_record_bytes)?;
    for row in &mut page.rows {
        let ordinal = index.session.entry_position(&row.id).expect("tree entry");
        let real = reader.entry(index.locations[ordinal])?;
        if let Some(message) = real.data.as_message() {
            row.preview = message
                .content
                .iter()
                .take(16)
                .find_map(|block| {
                    if let ContentBlock::Text { text } = block {
                        Some(text.chars().take(160).collect())
                    } else {
                        None
                    }
                })
                .unwrap_or_default();
        }
    }
    check(
        "page bytes",
        serde_json::to_vec(&page)?.len() as u64,
        repository.limits.max_page_bytes as u64,
    )?;
    Ok(page)
}
pub(super) fn search(
    repository: &JsonlSessionRepository,
    id: &str,
    query: &str,
    limit: usize,
    cursor: Option<&SearchCursor>,
) -> Result<SearchPage> {
    let _lock = repository.metadata_lock(id, false)?;
    let mut cache = repository
        .archive_cache
        .lock()
        .map_err(|_| anyhow::anyhow!("archive cache poisoned"))?;
    let index = cached(repository, id, &mut cache)?;
    let mut reader = Reader::new(&index.path, repository.limits.max_record_bytes)?;
    let metadata = metadata(index);
    let page = search_archive_entries(
        &metadata,
        index.locations.len(),
        query,
        limit,
        cursor,
        |ordinal| {
            reader
                .entry(index.locations[ordinal])
                .map(std::borrow::Cow::Owned)
        },
    )?;
    check(
        "page bytes",
        serde_json::to_vec(&page)?.len() as u64,
        repository.limits.max_page_bytes as u64,
    )?;
    Ok(page)
}
pub(super) fn prepare_navigation(
    repository: &JsonlSessionRepository,
    window: &SessionWindow,
    action: ArchiveNavigation,
) -> Result<SessionNavigationPlan> {
    let _lock = repository.metadata_lock(&window.metadata().id, false)?;
    let mut cache = repository
        .archive_cache
        .lock()
        .map_err(|_| anyhow::anyhow!("archive cache poisoned"))?;
    let index = cached(repository, &window.metadata().id, &mut cache)?;
    ensure!(
        index.session.metadata().revision == window.metadata().revision,
        "window revision conflict"
    );
    let (action, transaction) = prepare_transaction(&index.session, action)?;
    if let ArchiveNavigation::EditContext { edit } = &action {
        let ordinal = index
            .session
            .entry_position(&edit.target_id)
            .context("edit target missing")?;
        let mut reader = Reader::new(&index.path, repository.limits.max_record_bytes)?;
        let target = reader.entry(index.locations[ordinal])?;
        edit.validate_replacement(
            target
                .data
                .as_message()
                .context("edit target has no message")?,
        )?;
    }
    let mut candidate = index.clone();
    if let Some(transaction) = &transaction {
        let revision = index
            .session
            .metadata()
            .revision
            .checked_add(1)
            .context("navigation revision exhausted")?;
        accept(
            &mut candidate,
            transaction.entries.clone(),
            transaction.leaf_id.clone(),
            revision,
            now_millis()?,
            index.valid_len,
            &repository.limits,
        )?;
    }
    let owned = transaction
        .as_ref()
        .map(|t| t.entries.as_slice())
        .unwrap_or_default();
    let mut reader = Reader::new(&index.path, repository.limits.max_record_bytes)?;
    // Planned edit entries have no file location yet. Their actual immutable payloads are
    // used only in this private preview; older records are still read from the pinned archive.
    let preview = materialize_using(&candidate, &repository.limits, |location| {
        if location.offset == index.valid_len {
            owned
                .get(location.ordinal)
                .cloned()
                .context("planned navigation entry missing")
        } else {
            reader.entry(location)
        }
    })?;
    SessionNavigationPlan::new(
        window.metadata().clone(),
        action,
        transaction,
        RunSession::Window(Box::new(preview)),
    )
}
pub(super) fn commit_navigation(
    repository: &JsonlSessionRepository,
    window: &mut SessionWindow,
    plan: SessionNavigationPlan,
) -> Result<()> {
    plan.validate_source(window.metadata())?;
    if let Some(transaction) = plan.into_transaction() {
        commit_transaction(repository, window, transaction, true)?;
    }
    Ok(())
}
pub(super) fn navigate(
    repository: &JsonlSessionRepository,
    window: &mut SessionWindow,
    action: ArchiveNavigation,
) -> Result<()> {
    let plan = prepare_navigation(repository, window, action)?;
    commit_navigation(repository, window, plan)
}

/// One pinned read/validation pass followed by bounded record visits, never model projections.
pub(crate) fn visit_export<T>(
    repository: &JsonlSessionRepository,
    workspace: &Path,
    id: &str,
    limits: &crate::exports::ExportLimits,
    context: &aicoder_core::TurnExecutionContext,
    render: impl FnOnce(
        crate::exports::Manifest,
        &mut dyn Iterator<Item = Result<SessionEntry>>,
        &std::collections::HashSet<String>,
    ) -> Result<T>,
) -> Result<T> {
    use std::os::unix::fs::MetadataExt;
    let workspace = workspace.canonicalize()?;
    if context.is_cancelled() {
        return Err(context.error().into());
    }
    let _metadata = repository.export_lock(id)?;
    let _idle = repository.write_lease(id, None)?;
    let path = repository.find(id)?;
    let mut file = open_data(&path, true, false)?;
    let before = file.metadata()?;
    check("export source bytes", before.len(), limits.max_source_bytes)?;
    let (header_id, version, _, cwd) = read_header(&path)?;
    ensure!(
        header_id == id && cwd == workspace,
        "session belongs to another workspace or identity"
    );
    let mut source_limits = repository.limits.clone();
    source_limits.max_scan_bytes = source_limits.max_scan_bytes.min(limits.max_source_bytes);
    source_limits.max_snapshot_bytes = source_limits
        .max_snapshot_bytes
        .min(limits.max_source_bytes);
    source_limits.max_record_bytes = source_limits
        .max_record_bytes
        .min(limits.max_source_bytes as usize);
    source_limits.max_index_entries = source_limits.max_index_entries.min(limits.max_entries);
    let (result, expected_len, expected_hash) = if version == SESSION_VERSION {
        let index = scan_file(&path, &source_limits, file.try_clone()?, Some(context))?;
        ensure!(
            index.session.metadata().id == id && index.session.metadata().cwd == workspace,
            "session source changed"
        );
        let selected = index
            .session
            .selected_branch()
            .entries()
            .iter()
            .map(|e| e.id.clone())
            .collect();
        let manifest = crate::exports::Manifest::new(
            index.session.metadata().clone(),
            index.locations.len(),
            index.expected_len,
            index.expected_len - index.valid_len,
            index.session.unfinished_turn().map(str::to_string),
        );
        let mut reader = Reader {
            reader: BufReader::with_capacity(8192, file.try_clone()?),
            record: None,
            limit: source_limits.max_record_bytes,
        };
        let mut entries = index.locations.iter().map(|location| {
            if context.is_cancelled() {
                return Err(context.error().into());
            }
            reader.entry(*location)
        });
        (
            render(manifest, &mut entries, &selected)?,
            index.expected_len,
            index.hash,
        )
    } else {
        let bytes = read_full(&mut file, &source_limits)?;
        let session = load_bytes(&path, &bytes)?;
        ensure!(
            session.metadata().id == id && session.metadata().cwd == workspace,
            "session source changed"
        );
        check(
            "export entries",
            session.entries().len() as u64,
            limits.max_entries as u64,
        )?;
        let cursor = session
            .backend_state::<StorageCursor>()
            .context("legacy storage cursor missing")?;
        let selected = session
            .selected_branch()
            .entries()
            .iter()
            .map(|e| e.id.clone())
            .collect();
        let manifest = crate::exports::Manifest::new(
            session.metadata().clone(),
            session.entries().len(),
            cursor.expected_len,
            cursor.expected_len - cursor.valid_len,
            session.unfinished_turn().map(str::to_string),
        );
        let mut entries = session.entries().iter().cloned().map(Ok);
        (
            render(manifest, &mut entries, &selected)?,
            bytes.len() as u64,
            fingerprint(&bytes),
        )
    };
    if context.is_cancelled() {
        return Err(context.error().into());
    }
    let (length, hash, _) = hash_file(&mut file, None, source_limits.max_scan_bytes)?;
    let after = file.metadata()?;
    let named = std::fs::symlink_metadata(&path)?;
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
    ensure!(
        length == expected_len
            && hash == expected_hash
            && stamp(&before) == stamp(&after)
            && !named.file_type().is_symlink()
            && stamp(&after) == stamp(&named),
        "session changed while preparing export"
    );
    if context.is_cancelled() {
        return Err(context.error().into());
    }
    Ok(result)
}

pub(super) fn extension_state(
    repository: &JsonlSessionRepository,
    id: &str,
    namespace: &str,
    max_bytes: usize,
) -> Result<Option<aicoder_core::session::SessionExtensionState>> {
    ensure!(
        !namespace.is_empty() && namespace.len() <= 256 && (1..=64 * 1024).contains(&max_bytes),
        "invalid extension-state query bounds"
    );
    let _lock = repository.metadata_lock(id, false)?;
    let path = repository.find(id)?;
    let (_, version, _, _) = read_header(&path)?;
    if version != SESSION_VERSION {
        // Small legacy stores retain the default full-load behavior; oversized ones need migration.
        let mut file = open_data(&path, true, false)?;
        let session = load_bytes(&path, &read_full(&mut file, &repository.limits)?)?;
        let entry = session.active_entries().into_iter().rev().find(|entry| matches!(&entry.data, SessionEntryData::Extension {namespace:stored,..} if stored == namespace));
        return entry
            .map(|entry| {
                let SessionEntryData::Extension { data, .. } = &entry.data else {
                    unreachable!()
                };
                crate::packages::bounded_json(data, max_bytes)?;
                Ok(aicoder_core::session::SessionExtensionState {
                    metadata: session.metadata().clone(),
                    entry_id: entry.id.clone(),
                    data: data.clone(),
                })
            })
            .transpose();
    }
    let mut cache = repository
        .archive_cache
        .lock()
        .map_err(|_| anyhow::anyhow!("archive cache poisoned"))?;
    let index = cached(repository, id, &mut cache)?;
    let branch = index.session.selected_branch();
    let selected = branch.entries();
    let entry=selected.iter().rev().find(|entry|matches!(&entry.data,SessionEntryData::Extension {namespace:stored,..} if stored==namespace));
    let Some(entry) = entry else {
        return Ok(None);
    };
    let ordinal = index
        .session
        .entry_position(&entry.id)
        .context("extension state location missing")?;
    let mut reader = Reader::new(&index.path, repository.limits.max_record_bytes)?;
    let actual = reader.entry(index.locations[ordinal])?;
    let SessionEntryData::Extension {
        namespace: stored,
        data,
    } = actual.data
    else {
        anyhow::bail!("extension state archive location changed");
    };
    ensure!(stored == namespace, "extension state namespace changed");
    crate::packages::bounded_json(&data, max_bytes)?;
    Ok(Some(aicoder_core::session::SessionExtensionState {
        metadata: metadata(index),
        entry_id: actual.id,
        data,
    }))
}

pub(super) fn prepare_fork(
    repository: &JsonlSessionRepository,
    source: &RunSession,
    point: ForkPoint,
    fork_limits: SessionForkLimits,
    context: &aicoder_core::TurnExecutionContext,
) -> Result<SessionForkPlan> {
    use super::fork::{ForkSourcePin, Stamp};
    let fork_limits = fork_limits.validate()?;
    let id = source.metadata().id.clone();
    let _metadata = repository.metadata_lock(&id, false)?;
    let cursor = match source {
        RunSession::Full(s) => s.backend_state::<StorageCursor>().cloned(),
        RunSession::Window(w) => w.backend_state::<WindowCursor>().map(|c| c.cursor.clone()),
    }
    .context("fork source has no native checkpoint")?;
    ensure!(
        cursor.path.parent() == Some(repository.root()) && repository.find(&id)? == cursor.path,
        "fork source repository mismatch"
    );
    let mut file = open_data(&cursor.path, true, false)?;
    let stamp = Stamp::new(&file.metadata()?);
    let max_scan = fork_limits
        .max_source_bytes
        .min(repository.limits.max_scan_bytes);
    let (len, hash, _) = hash_file(&mut file, None, max_scan)?;
    ensure!(
        len == cursor.expected_len && hash == cursor.fingerprint,
        "fork source checkpoint changed"
    );
    let mut limits = repository.limits.clone();
    limits.max_scan_bytes = max_scan;
    limits.max_snapshot_bytes = limits.max_snapshot_bytes.min(fork_limits.max_source_bytes);
    limits.max_index_entries = limits.max_index_entries.min(fork_limits.max_source_entries);
    let version = read_header(&cursor.path)?.1;
    let mut plan = if version == SESSION_VERSION {
        file.seek(SeekFrom::Start(0))?;
        let index = scan_file(&cursor.path, &limits, file.try_clone()?, Some(context))?;
        ensure!(
            index.session.metadata().id == id
                && index.session.metadata().revision == source.metadata().revision
                && index.session.metadata().leaf_id == source.metadata().leaf_id,
            "fork source selection changed"
        );
        let (resolved, leaf, _) =
            resolve_fork_point(&index.session, &point, fork_limits.max_editor_bytes)?;
        let mut reader = Reader {
            reader: BufReader::with_capacity(8192, file.try_clone()?),
            record: None,
            limit: limits.max_record_bytes,
        };
        let editor = if let ForkPoint::Before(entry_id) = &resolved {
            let ordinal = index
                .session
                .entry_position(entry_id)
                .context("fork editor source missing")?;
            let entry = reader.entry(index.locations[ordinal])?;
            let message = entry
                .data
                .as_message()
                .filter(|m| m.role == MessageRole::User)
                .context("fork editor requires real user content")?;
            Some(fork_editor(entry_id, message, fork_limits.max_editor_bytes))
        } else {
            None
        };
        let branch = index.session.branch(leaf.as_deref())?;
        let mut records = Vec::new();
        let mut bytes = 0usize;
        for entry in branch.entries() {
            if context.is_cancelled() {
                return Err(context.error().into());
            }
            ensure!(
                records.len() < fork_limits.max_entries,
                "fork entry budget exceeded"
            );
            let ordinal = index
                .session
                .entry_position(&entry.id)
                .context("fork archive position missing")?;
            let real = reader.entry(index.locations[ordinal])?;
            bytes = bytes.saturating_add(super::fork::bounded_size(&real, fork_limits.max_bytes)?);
            ensure!(
                bytes <= fork_limits.max_bytes,
                "fork prefix byte budget exceeded"
            );
            records.push(real);
        }
        SessionForkPlan::from_records(
            source.metadata().clone(),
            resolved,
            leaf,
            editor,
            records,
            index.session.labels().clone(),
            index.expected_len - index.valid_len,
            fork_limits,
            context,
        )?
    } else {
        file.seek(SeekFrom::Start(0))?;
        let bytes = read_full(&mut file, &limits)?;
        let session = load_bytes(&cursor.path, &bytes)?;
        ensure!(
            session.metadata() == source.metadata(),
            "legacy fork source changed"
        );
        SessionForkPlan::prepare_full(&session, point, fork_limits, context)?
    };
    let (ranges, output_bytes) = super::fork::layout(&plan, repository.limits.max_record_bytes)?;
    plan.set_publication_revision(ranges.len() as u64)?;
    if output_bytes as u64 > repository.limits.max_snapshot_bytes {
        let preview = fork_preview(repository, &plan, &ranges)?;
        plan.set_window_preview(preview)?;
    }
    let pin = ForkSourcePin {
        root: repository.root().to_owned(),
        path: cursor.path,
        file: std::sync::Mutex::new(file),
        stamp,
        expected_len: len,
        hash,
        max_scan,
        ranges,
        output_bytes,
    };
    super::fork::verify(&pin, context)?;
    plan.attach_backend(pin);
    Ok(plan)
}

/// Materialize the same bounded view before publication, using actual admitted source records.
pub(super) fn fork_preview(
    repository: &JsonlSessionRepository,
    plan: &SessionForkPlan,
    ranges: &[std::ops::Range<usize>],
) -> Result<SessionWindow> {
    let mut header_metadata = plan.target().clone();
    header_metadata.revision = 0;
    header_metadata.leaf_id = None;
    header_metadata.title = None;
    let bytes = header(&header_metadata)?;
    let mut index = ArchiveIndex {
        path: repository
            .root()
            .join(format!("{}.jsonl", plan.target().id)),
        session: Session::from_entries(header_metadata, Vec::new())?,
        locations: Vec::new(),
        expected_len: bytes.len() as u64,
        valid_len: bytes.len() as u64,
        hash: fingerprint(&bytes),
        valid_hash: fingerprint(&bytes),
        separator: false,
        index_bytes: 0,
        title: None,
    };
    let mut offsets = std::collections::HashMap::new();
    for (i, range) in ranges.iter().enumerate() {
        let records = &plan.snapshot().entries()[range.clone()];
        let leaf = records.last().map(|e| e.id.clone());
        let bytes = transaction_line(i as u64 + 1, plan.target().updated_at, records, &leaf)?;
        let offset = index.expected_len;
        offsets.insert(offset, range.start);
        accept(
            &mut index,
            records.to_vec(),
            leaf,
            i as u64 + 1,
            plan.target().updated_at,
            offset,
            &repository.limits,
        )?;
        index.expected_len += bytes.len() as u64;
        index.valid_len = index.expected_len;
        index.hash = hash_more(index.hash, &bytes);
        index.valid_hash = index.hash;
    }
    ensure!(
        metadata(&index) == *plan.target(),
        "fork preview metadata differs from publication"
    );
    materialize_using(&index, &repository.limits, |location| {
        let start = offsets
            .get(&location.offset)
            .context("fork preview location missing")?;
        plan.snapshot()
            .entries()
            .get(start + location.ordinal)
            .cloned()
            .context("fork preview ordinal changed")
    })
}
pub(super) fn bind_fork_window(window: &mut SessionWindow, path: PathBuf, length: u64, hash: u64) {
    window.attach_backend_state(WindowCursor {
        cursor: cursor_for(path, length, length, false, hash),
    });
}

#[cfg(test)]
mod export_tests {
    use super::*;
    #[test]
    fn changed_source_is_rejected_after_record_visits_even_under_an_idle_lease() {
        let root = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        let repo = JsonlSessionRepository::new(root.path()).unwrap();
        let mut session = repo.create(workspace.path()).unwrap();
        repo.append(&mut session, Message::text(MessageRole::User, "original"))
            .unwrap();
        let path = session.path().unwrap().to_path_buf();
        let result = visit_export(
            &repo,
            workspace.path(),
            &session.metadata().id,
            &crate::exports::ExportLimits::default(),
            &aicoder_core::TurnExecutionContext::new(),
            |_, entries, _| {
                for entry in entries {
                    entry?;
                }
                // Simulate an external writer ignoring repository locks after materialization.
                std::fs::OpenOptions::new()
                    .append(true)
                    .open(&path)?
                    .write_all(b" ")?;
                Ok(())
            },
        );
        assert!(result.unwrap_err().to_string().contains("session changed"));
    }
}
