use super::*;
use aicoder_core::session::SessionEntryData;
#[derive(Serialize)]
struct Entry<'a> {
    selected_branch: bool,
    entry: &'a SessionEntry,
}
pub(super) fn document(
    out: &mut dyn Write,
    manifest: &Manifest,
    entries: &mut dyn Iterator<Item = Result<SessionEntry>>,
    selected: &HashSet<String>,
    format: ExportFormat,
) -> Result<()> {
    match format {
        ExportFormat::Json => {
            out.write_all(b"{\"schema_version\":1,\"manifest\":")?;
            serde_json::to_writer(&mut *out, manifest)?;
            out.write_all(b",\"entries\":[")?;
        }
        ExportFormat::Html => {
            out.write_all(include_str!("header.html").as_bytes())?;
            escaped(
                out,
                manifest
                    .metadata
                    .title
                    .as_deref()
                    .unwrap_or("Session export")
                    .as_bytes(),
            )?;
            out.write_all(b"</h1><p>Complete canonical history &middot; all branches &middot; data only</p><details><summary>Source and scope</summary><pre>")?;
            serde_json::to_writer_pretty(Escaped(out), manifest)?;
            out.write_all(b"</pre></details><p>Selected-branch records have a green border. Summaries, navigation, tool results and interruption records are shown as recorded. External artifact bytes are not bundled. This document restores no permissions.</p><main>")?;
        }
    }
    let mut count = 0;
    for entry in entries {
        let entry = entry?;
        let selected = selected.contains(&entry.id);
        match format {
            ExportFormat::Json => {
                if count > 0 {
                    out.write_all(b",")?;
                }
                serde_json::to_writer(
                    &mut *out,
                    &Entry {
                        selected_branch: selected,
                        entry: &entry,
                    },
                )?;
            }
            ExportFormat::Html => {
                out.write_all(if selected {
                    b"<article class=selected>"
                } else {
                    b"<article>"
                })?;
                let kind = match &entry.data {
                    SessionEntryData::NativeContext { .. } => "native extension context",
                    SessionEntryData::Message { message } => match message.role {
                        aicoder_core::model::MessageRole::System => "system",
                        aicoder_core::model::MessageRole::User => "user",
                        aicoder_core::model::MessageRole::Assistant => "assistant",
                        aicoder_core::model::MessageRole::Tool => "tool result",
                    },
                    SessionEntryData::ModelChanged { .. } => "model selection",
                    SessionEntryData::Compaction { .. } => "recorded compaction",
                    SessionEntryData::Extension { .. } => "extension / navigation / summary",
                    SessionEntryData::TurnStart { .. } => "turn start",
                    SessionEntryData::TurnEnd { .. } => "recorded turn outcome",
                };
                write!(out, "<h2>{kind}</h2><p>Entry ")?;
                escaped(out, entry.id.as_bytes())?;
                out.write_all(b" &middot; parent ")?;
                escaped(out, entry.parent_id.as_deref().unwrap_or("root").as_bytes())?;
                out.write_all(b"</p>")?;
                preview(out, &entry.data)?;
                out.write_all(b"<details><summary>Canonical record</summary><pre>")?;
                serde_json::to_writer_pretty(Escaped(out), &entry)?;
                out.write_all(b"</pre></details></article>")?;
            }
        }
        count += 1;
    }
    ensure!(
        count == manifest.entry_count,
        "export source entry count changed"
    );
    match format{ExportFormat::Json=>out.write_all(b"],\"canonical_entries_complete\":true}\n")?,ExportFormat::Html=>out.write_all(b"</main><footer>All canonical entries exported. Raw storage framing is not included. Source-tail completeness and unfinished-turn state are declared above.</footer></body></html>\n")?}
    Ok(())
}
fn escaped(out: &mut dyn Write, bytes: &[u8]) -> io::Result<()> {
    let mut start = 0;
    for (i, b) in bytes.iter().enumerate() {
        let entity: Option<&[u8]> = match b {
            b'&' => Some(b"&amp;"),
            b'<' => Some(b"&lt;"),
            b'>' => Some(b"&gt;"),
            b'"' => Some(b"&quot;"),
            b'\'' => Some(b"&#39;"),
            _ => None,
        };
        if let Some(entity) = entity {
            out.write_all(&bytes[start..i])?;
            out.write_all(entity)?;
            start = i + 1;
        }
    }
    out.write_all(&bytes[start..])
}
struct Escaped<'a>(&'a mut dyn Write);
impl Write for Escaped<'_> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        escaped(self.0, bytes)?;
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        self.0.flush()
    }
}

// Human-readable previews are separate from the complete, unchanged canonical payload.
fn text_preview(out: &mut dyn Write, text: &str) -> Result<()> {
    let mut end = text.len().min(8192);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    out.write_all(b"<pre>")?;
    escaped(out, &text.as_bytes()[..end])?;
    if end < text.len() {
        out.write_all(b"\n[Display preview clipped; full data in canonical record below]")?;
    }
    out.write_all(b"</pre>")?;
    Ok(())
}
fn json_preview(out: &mut dyn Write, value: &serde_json::Value) -> Result<()> {
    struct Preview(Vec<u8>);
    impl Write for Preview {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            if bytes.len() > 8192usize.saturating_sub(self.0.len()) {
                return Err(io::Error::other("preview clipped"));
            }
            self.0.extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    let mut preview = Preview(Vec::new());
    let clipped = serde_json::to_writer_pretty(&mut preview, value).is_err();
    out.write_all(b"<pre>")?;
    escaped(out, &preview.0)?;
    if clipped {
        out.write_all(b"\n[Display preview clipped; full data in canonical record below]")?;
    }
    out.write_all(b"</pre>")?;
    Ok(())
}
fn preview(out: &mut dyn Write, data: &SessionEntryData) -> Result<()> {
    use aicoder_core::model::ContentBlock;
    match data {
        SessionEntryData::Message { message } | SessionEntryData::NativeContext { message, .. } => {
            for block in &message.content {
                match block {
                ContentBlock::Text{text}=>text_preview(out,text)?,
                ContentBlock::Reasoning{text}=>{out.write_all(b"<h3>Recorded reasoning</h3>")?;text_preview(out,text)?;},
                ContentBlock::ToolCall{call}=>{out.write_all(b"<h3>Tool call: ")?;escaped(out,call.name.as_bytes())?;out.write_all(b"</h3>")?;json_preview(out,&call.arguments)?;},
                ContentBlock::ToolResult{content,..}=>json_preview(out,content)?,
                ContentBlock::Image{..}=>out.write_all(b"<p>Recorded image source is retained in the canonical record; no image is fetched or rendered.</p>")?,
                ContentBlock::ProviderData{..}=>out.write_all(b"<p>Opaque provider state is retained as data in the canonical record.</p>")?,
            }
            }
        }
        SessionEntryData::Compaction { summary, .. } => text_preview(out, summary)?,
        _ => {
            out.write_all(b"<p>Recorded session metadata; inspect the canonical record below.</p>")?
        }
    }
    Ok(())
}
