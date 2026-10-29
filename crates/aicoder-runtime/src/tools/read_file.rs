use super::{
    ExecutableTool, ToolCapability, ToolContext, ToolFailure, ToolSuccess, WorkspacePaths, io::*,
    util::parse_arguments,
};
use aicoder_core::model::ToolDefinition;
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncSeekExt, BufReader, SeekFrom};

pub struct ReadFileTool;
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Arguments {
    path: String,
    offset_line: Option<u64>,
    #[serde(default = "default_lines")]
    limit_lines: usize,
    cursor: Option<String>,
}
fn default_lines() -> usize {
    400
}
#[async_trait]
impl ExecutableTool for ReadFileTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "read_file".into(),
            description: Some("Read a bounded UTF-8 page. Pass next_cursor unchanged to continue, including inside long lines. Do not combine cursor with offset_line.".into()),
            parameters: json!({"type":"object","properties":{
                "path":{"type":"string"},"offset_line":{"type":"integer","minimum":1,"default":1},
                "limit_lines":{"type":"integer","minimum":1,"maximum":4000,"default":400},
                "cursor":{"type":"string"}},"required":["path"],"additionalProperties":false}),
        }
    }
    fn capability(&self) -> ToolCapability {
        ToolCapability::ReadOnly
    }
    async fn execute(
        &self,
        context: &ToolContext,
        arguments: Value,
    ) -> Result<ToolSuccess, ToolFailure> {
        bounded(context, read(context, parse_arguments(arguments)?)).await
    }
}
async fn read(context: &ToolContext, args: Arguments) -> Result<ToolSuccess, ToolFailure> {
    let limit = page_limit(context)?;
    if args.path.len() > MAX_PATH_BYTES {
        return Err(failure("invalid_arguments", "path exceeds 4096 bytes"));
    }
    if args
        .offset_line
        .is_some_and(|line| line == 0 || line > u64::MAX - MAX_SCAN_BYTES as u64)
        || !(1..=4000).contains(&args.limit_lines)
        || args.cursor.is_some() && args.offset_line.is_some()
    {
        return Err(failure(
            "invalid_arguments",
            "Use offset_line >= 1 or cursor, and limit_lines between 1 and 4000",
        ));
    }
    let paths = WorkspacePaths::from_context(context);
    let path = paths.resolve_existing(&args.path)?;
    if path.to_str().is_none() {
        return Err(failure("unsupported_path", "Tool paths require UTF-8"));
    }
    let label = paths.relative_path(&path);
    let query = key(&("read_file", &path));
    let (file, stamp) = open_regular(&path).await?;
    let mut reader = BufReader::new(file);
    let (offset, line, skipped) = if let Some(cursor) = args.cursor {
        let cursor = Cursor::decode(&cursor, &query)?;
        cursor.check_file(&stamp)?;
        if cursor.path != label {
            return Err(failure(
                "invalid_cursor",
                "Cursor path differs from requested file",
            ));
        }
        reader
            .seek(SeekFrom::Start(cursor.offset))
            .await
            .map_err(io_error)?;
        (cursor.offset, cursor.line, 0)
    } else {
        let target = args.offset_line.unwrap_or(1);
        let mut line = 1_u64;
        let mut offset = 0_u64;
        while line < target {
            let bytes = reader.fill_buf().await.map_err(io_error)?;
            if bytes.is_empty() {
                break;
            }
            let mut count = 0;
            for byte in bytes {
                count += 1;
                if *byte == b'\n' {
                    line += 1;
                }
                if line == target {
                    break;
                }
            }
            if offset + count as u64 > MAX_SCAN_BYTES as u64 {
                return Err(failure(
                    "scan_limit",
                    "offset_line requires scanning more than 16 MiB; continue from an earlier page's next_cursor",
                ));
            }
            offset += count as u64;
            reader.consume(count);
        }
        (offset, target, offset as usize)
    };
    // Fixed-size page read; never next_line/read_to_string on an unbounded source line.
    let mut bytes = Vec::new();
    (&mut reader)
        .take((limit + 4) as u64)
        .read_to_end(&mut bytes)
        .await
        .map_err(io_error)?;
    let mut lines = 0;
    if let Some(index) = bytes.iter().position(|b| {
        if *b == b'\n' {
            lines += 1;
        }
        lines == args.limit_lines
    }) {
        bytes.truncate(index + 1);
    }
    let text = utf8_prefix(&bytes)?;
    if text.len() != bytes.len() && offset + bytes.len() as u64 == stamp.len {
        return Err(failure(
            "non_utf8",
            "File ends with an incomplete UTF-8 character",
        ));
    }
    let max = text.len().min(limit);
    let make = |mut count: usize| {
        while !text.is_char_boundary(count) {
            count -= 1;
        }
        let raw = &text[..count];
        let newline_count = raw.bytes().filter(|b| *b == b'\n').count() as u64;
        let next_line = line + newline_count;
        let next_offset = offset + count as u64;
        let has_more = next_offset < stamp.len;
        let complete = raw.ends_with('\n') || !has_more;
        let content = raw
            .strip_suffix('\n')
            .map(|s| s.strip_suffix('\r').unwrap_or(s))
            .unwrap_or(raw);
        let lines_read = newline_count + u64::from(!raw.is_empty() && !raw.ends_with('\n'));
        let end_line = (lines_read > 0).then(|| line + lines_read - 1);
        json!({"path":label,"offset_line":line,"lines_read":lines_read,"end_line":end_line,
            "content":content,"content_truncated":has_more && !complete,"end_line_complete":complete,
            "has_more":has_more,"next_offset_line":has_more.then_some(next_line),
            "next_offset_byte":has_more.then_some(next_offset),
            "next_cursor":has_more.then(||Cursor::new(&query,label.clone(),next_offset,next_line,Some(stamp.clone())).encode()),
            "scan_bytes":skipped + bytes.len()})
    };
    // JSON escaping and cursor metadata also count, keeping the cursor intact through dispatch.
    let (mut low, mut high) = (0, max);
    while low < high {
        let mid = low + (high - low).div_ceil(2);
        if make(mid).to_string().len() <= limit {
            low = mid;
        } else {
            high = mid - 1;
        }
    }
    let output = make(low);
    if !text.is_empty() && output["next_offset_byte"].as_u64() == Some(offset) {
        return Err(too_small());
    }
    unchanged(reader.get_ref(), &stamp).await?;
    success(output, limit)
}
