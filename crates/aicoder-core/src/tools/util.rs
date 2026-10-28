use serde::Deserialize;
use serde_json::Value;

use super::ToolFailure;

pub(crate) fn truncate_utf8(input: &str, max_bytes: usize) -> (String, bool) {
    if input.len() <= max_bytes {
        return (input.to_string(), false);
    }
    let mut end = max_bytes;
    while !input.is_char_boundary(end) {
        end -= 1;
    }
    (input[..end].to_string(), true)
}

pub(crate) fn parse_arguments<T>(arguments: Value) -> Result<T, ToolFailure>
where
    T: for<'de> Deserialize<'de>,
{
    serde_json::from_value(arguments).map_err(|error| {
        ToolFailure::new(
            "invalid_arguments",
            format!("Invalid tool arguments: {error}"),
        )
    })
}
