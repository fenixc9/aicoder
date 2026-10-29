use super::{
    ExecutableTool, ToolCapability, ToolContext, ToolFailure, ToolSuccess, WorkspacePaths, io::*,
    tree, util::parse_arguments,
};
use aicoder_core::model::ToolDefinition;
use async_trait::async_trait;
use regex::RegexBuilder;
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::io::{AsyncSeekExt, BufReader, SeekFrom};

pub struct GrepTool;
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Arguments {
    pattern: String,
    #[serde(default = "default_path")]
    path: String,
    glob: Option<String>,
    #[serde(default = "default_results")]
    max_results: usize,
    #[serde(default)]
    hidden: bool,
    #[serde(default = "yes")]
    respect_gitignore: bool,
    #[serde(default)]
    literal: bool,
    #[serde(default)]
    case_insensitive: bool,
    cursor: Option<String>,
}
fn default_path() -> String {
    ".".into()
}
fn default_results() -> usize {
    100
}
fn yes() -> bool {
    true
}
#[async_trait]
impl ExecutableTool for GrepTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "grep".into(),
            description: Some("Search UTF-8 files with Rust regex (or literal text), project .gitignore, bounded scan and stable path/line pages. Copy next_cursor to continue; has_more means unscanned input remains. Binary files are skipped; oversized lines report a recoverable error.".into()),
            parameters: json!({"type":"object","properties":{
                "pattern":{"type":"string"},"path":{"type":"string","default":"."},"glob":{"type":"string"},
                "max_results":{"type":"integer","minimum":1,"maximum":1000,"default":100},
                "hidden":{"type":"boolean","default":false},"respect_gitignore":{"type":"boolean","default":true},
                "literal":{"type":"boolean","default":false},"case_insensitive":{"type":"boolean","default":false},
                "cursor":{"type":"string"}},"required":["pattern"],"additionalProperties":false}),
        }
    }
    fn capability(&self) -> ToolCapability {
        ToolCapability::ReadOnly
    }
    async fn execute(
        &self,
        context: &ToolContext,
        args: Value,
    ) -> Result<ToolSuccess, ToolFailure> {
        bounded(context, search(context, parse_arguments(args)?)).await
    }
}
struct Record {
    value: Value,
    display: String,
    resume: Cursor,
}
async fn search(context: &ToolContext, args: Arguments) -> Result<ToolSuccess, ToolFailure> {
    let limit = page_limit(context)?;
    if args.path.len() > MAX_PATH_BYTES {
        return Err(failure("invalid_arguments", "path exceeds 4096 bytes"));
    }
    if !(1..=1000).contains(&args.max_results) || args.pattern.len() > 4096 {
        return Err(failure(
            "invalid_arguments",
            "max_results must be 1..1000 and pattern at most 4096 bytes",
        ));
    }
    let pattern = if args.literal {
        regex::escape(&args.pattern)
    } else {
        args.pattern.clone()
    };
    let regex = RegexBuilder::new(&pattern)
        .case_insensitive(args.case_insensitive)
        .size_limit(1024 * 1024)
        .dfa_size_limit(1024 * 1024)
        .build()
        .map_err(|error| failure("invalid_regex", error.to_string()))?;
    let matcher = args.glob.as_deref().map(tree::glob).transpose()?;
    let paths = WorkspacePaths::from_context(context);
    let root = paths.resolve_existing(&args.path)?;
    if root.to_str().is_none() {
        return Err(failure("unsupported_path", "Tool paths require UTF-8"));
    }
    let query = key(&(
        "grep",
        &root,
        &args.pattern,
        &args.glob,
        args.hidden,
        args.respect_gitignore,
        args.literal,
        args.case_insensitive,
    ));
    let resume = args
        .cursor
        .as_deref()
        .map(|s| Cursor::decode(s, &query))
        .transpose()?;
    let tree = tree::enumerate(context, &root, true, args.hidden, args.respect_gitignore).await?;
    let root_is_file = root.is_file();
    let files = tree
        .entries
        .into_iter()
        .filter(|entry| {
            let path = context.workspace_root().join(&entry.path);
            let relative = if root_is_file {
                path.file_name()
                    .map(std::path::Path::new)
                    .expect("file name")
            } else {
                path.strip_prefix(&root).expect("scoped result")
            };
            matcher.as_ref().is_none_or(|m| m.is_match(relative))
        })
        .collect::<Vec<_>>();
    let start = match &resume {
        Some(cursor) => files
            .binary_search_by(|entry| entry.path.cmp(&cursor.path))
            .map_err(|_| {
                failure(
                    "stale_cursor",
                    "Continuation file is missing or excluded; restart query",
                )
            })?,
        None => 0,
    };
    let mut records: Vec<Record> = Vec::new();
    let mut scanned = 0;
    let mut retained_bytes = 0;
    let mut binary_files = 0;
    let mut next = None;
    'files: for (index, entry) in files.iter().enumerate().skip(start) {
        let path = context.workspace_root().join(&entry.path);
        // Recheck canonical identity for every opened candidate, not only the scope root.
        let resolved = paths.resolve_existing(&entry.path)?;
        if path != resolved {
            return Err(failure(
                "stale_cursor",
                "Search file became a symbolic link; restart query",
            ));
        }
        let (file, stamp) = open_regular(&path).await?;
        let mut reader = BufReader::new(file);
        let (mut offset, mut line) = if index == start {
            if let Some(cursor) = &resume {
                cursor.check_file(&stamp)?;
                reader
                    .seek(SeekFrom::Start(cursor.offset))
                    .await
                    .map_err(io_error)?;
                (cursor.offset, cursor.line)
            } else {
                (0, 1)
            }
        } else {
            (0, 1)
        };
        loop {
            let bytes = match next_line(&mut reader, &mut scanned).await.map_err(|error| error.with_details(json!({
                "path":entry.path,"line":line,"read_cursor":Cursor::new(&key(&("read_file",&path)),entry.path.clone(),offset,line,Some(stamp.clone())).encode()
            })))? {
                LineRead::Eof => break,
                LineRead::Binary => { binary_files += 1; break; }
                LineRead::ScanLimit => {
                    next = Some(Cursor::new(&query,entry.path.clone(),offset,line,Some(stamp.clone())));
                    unchanged(reader.get_ref(), &stamp).await?;
                    break 'files;
                }
                LineRead::Line(bytes) => bytes,
            };
            let text = match std::str::from_utf8(&bytes) {
                Ok(text) => text
                    .strip_suffix('\n')
                    .unwrap_or(text)
                    .strip_suffix('\r')
                    .unwrap_or(text.strip_suffix('\n').unwrap_or(text)),
                Err(_) => {
                    // Discarded read-ahead still counts toward the per-query input budget.
                    scanned += reader.buffer().len().min(MAX_SCAN_BYTES - scanned);
                    binary_files += 1;
                    break;
                }
            };
            let next_offset = offset + bytes.len() as u64;
            let next_line = line + u64::from(bytes.ends_with(b"\n"));
            if let Some(found) = regex.find(text) {
                let mut begin = found.start().saturating_sub(128);
                while !text.is_char_boundary(begin) {
                    begin -= 1;
                }
                let mut end = (begin + 1024).min(text.len());
                while !text.is_char_boundary(end) {
                    end -= 1;
                }
                let excerpt = &text[begin..end];
                let cursor = Cursor::new(
                    &query,
                    entry.path.clone(),
                    next_offset,
                    next_line,
                    Some(stamp.clone()),
                );
                let value = json!({"path":entry.path,"line":line,"byte_offset":offset,"text":excerpt,"text_offset":begin,"match_start":found.start(),"text_truncated":begin > 0 || end < text.len(),
                    "read_cursor":Cursor::new(&key(&("read_file",&path)),entry.path.clone(),offset,line,Some(stamp.clone())).encode()});
                let display = format!("{}:{line}:{excerpt}", entry.path);
                retained_bytes +=
                    value.to_string().len() + serde_json::to_string(&display).expect("text").len();
                records.push(Record {
                    value,
                    display,
                    resume: cursor.clone(),
                });
                if records.len() == args.max_results || retained_bytes >= limit {
                    unchanged(reader.get_ref(), &stamp).await?;
                    if next_offset < stamp.len || index + 1 < files.len() {
                        next = Some(cursor);
                    }
                    break 'files;
                }
            }
            offset = next_offset;
            line = next_line;
        }
        unchanged(reader.get_ref(), &stamp).await?;
    }
    let make = |count: usize| {
        let cursor = if count < records.len() && count > 0 {
            Some(&records[count - 1].resume)
        } else {
            next.as_ref()
        };
        json!({"path":paths.relative_path(&root),"matches":records[..count].iter().map(|r|r.display.as_str()).collect::<Vec<_>>().join("\n"),
            "records":records[..count].iter().map(|r|&r.value).collect::<Vec<_>>(),"has_more":cursor.is_some(),
            "next_cursor":cursor.map(Cursor::encode),"scan_bytes":scanned,"visited_entries":tree.visited,"binary_files_skipped":binary_files})
    };
    let (mut low, mut high) = (0, records.len());
    while low < high {
        let mid = low + (high - low).div_ceil(2);
        if make(mid).to_string().len() <= limit {
            low = mid;
        } else {
            high = mid - 1;
        }
    }
    if low == 0 && !records.is_empty() {
        return Err(too_small());
    }
    success(make(low), limit)
}
