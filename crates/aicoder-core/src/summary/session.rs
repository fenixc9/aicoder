use super::*;
use crate::session::{Session, SessionEntry, SessionEntryData, SessionRepository};
pub const SUMMARY_NAMESPACE: &str = "aicoder.context.summary.v1";
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SummaryCheckpoint {
    pub version: u32,
    pub source_leaf: String,
    pub source_revision: u64,
    pub first_kept_entry_id: String,
    pub summarized_messages: usize,
    pub accounted_by_turn: Option<String>,
    pub draft: SummaryDraft,
}
impl SummaryCheckpoint {
    pub fn data(&self) -> SessionEntryData {
        SessionEntryData::Extension {
            namespace: SUMMARY_NAMESPACE.into(),
            data: serde_json::to_value(self).expect("checkpoint"),
        }
    }
}
pub fn validate_checkpoint(
    record: &SummaryCheckpoint,
    ancestors: &[&SessionEntry],
    parent: Option<&str>,
) -> Result<()> {
    ensure!(
        matches!(record.version, 1 | 2),
        "unsupported summary checkpoint version"
    );
    record.draft.summary.validate()?;
    ensure!(
        parent == Some(record.source_leaf.as_str()),
        "summary source must be its direct parent snapshot"
    );
    ensure!(
        record.draft.keep_messages > 0 && record.summarized_messages > 0,
        "summary checkpoint must retain and replace history"
    );
    ensure!(
        !record.draft.provider.is_empty()
            && !record.draft.model.is_empty()
            && record.draft.provider.len() <= 256
            && record.draft.model.len() <= 256,
        "invalid summary generator identity"
    );
    ensure!(
        record.draft.source_digest.len() == 16
            && record
                .draft
                .source_digest
                .bytes()
                .all(|b| b.is_ascii_hexdigit()),
        "invalid summary digest"
    );
    let start = ancestors
        .iter()
        .position(|entry| entry.id == record.first_kept_entry_id)
        .ok_or_else(|| anyhow::anyhow!("summary boundary is not an ancestor"))?;
    ensure!(
        if record.version == 1 {
            matches!(&ancestors[start].data,SessionEntryData::Message{message} if message.role!=MessageRole::Tool)
        } else {
            ancestors[start]
                .data
                .as_message()
                .is_some_and(|m| m.role != MessageRole::Tool)
        },
        "summary boundary must start a complete message unit"
    );
    ensure!(
        ancestors[..start]
            .iter()
            .filter(|entry| if record.version == 1 {
                matches!(entry.data, SessionEntryData::Message { .. })
            } else {
                entry.data.as_message().is_some()
            })
            .count()
            == record.summarized_messages,
        "summary replaced-message count differs from source"
    );
    ensure!(
        ancestors[start..]
            .iter()
            .filter(|entry| if record.version == 1 {
                matches!(entry.data, SessionEntryData::Message { .. })
            } else {
                entry.data.as_message().is_some()
            })
            .count()
            == record.draft.keep_messages,
        "summary retained-message count differs from source"
    );
    ensure!(
        (1..=32).contains(&record.draft.attempts),
        "invalid summary attempt count"
    );
    if let Some(turn) = &record.accounted_by_turn {
        let open = ancestors
            .iter()
            .rev()
            .find_map(|entry| match &entry.data {
                SessionEntryData::TurnStart { turn_id } => Some(Some(turn_id)),
                SessionEntryData::TurnEnd { .. } => Some(None),
                _ => None,
            })
            .flatten();
        ensure!(
            open == Some(turn),
            "summary accounting must name the active turn"
        );
    }
    Ok(())
}
pub fn commit_summary(
    repository: &dyn SessionRepository,
    session: &mut Session,
    draft: SummaryDraft,
    first_kept_entry_id: String,
    turn_id: Option<String>,
) -> Result<String> {
    let branch = session.selected_branch();
    let start = branch
        .entries()
        .iter()
        .position(|entry| entry.id == first_kept_entry_id)
        .ok_or_else(|| anyhow::anyhow!("summary boundary missing"))?;
    let checkpoint = SummaryCheckpoint {
        version: 2,
        source_leaf: session
            .metadata()
            .leaf_id
            .clone()
            .ok_or_else(|| anyhow::anyhow!("empty summary source"))?,
        source_revision: session.metadata().revision,
        first_kept_entry_id,
        summarized_messages: branch.entries()[..start]
            .iter()
            .filter(|entry| entry.data.as_message().is_some())
            .count(),
        accounted_by_turn: turn_id,
        draft,
    };
    validate_checkpoint(
        &checkpoint,
        branch.entries(),
        session.metadata().leaf_id.as_deref(),
    )?;
    repository.append_entries(session, vec![checkpoint.data()])?;
    Ok(session.metadata().leaf_id.clone().expect("summary entry"))
}
