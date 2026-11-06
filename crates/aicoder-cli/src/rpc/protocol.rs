//! RPC framing is deliberately independent from provider wire types.
use aicoder_core::{
    ApprovalRequestId,
    session::{SearchCursor, TreeCursor},
    tools::ApprovalDecision,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const VERSION: u32 = 1;
pub const MAX_ID: u64 = (1 << 53) - 1;
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub version: u32,
    pub id: u64,
    pub command: Value,
}
#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum Command {
    Initialize,
    Prompt {
        text: String,
    },
    Status,
    Steer {
        run_id: String,
        text: String,
    },
    FollowUp {
        run_id: String,
        text: String,
    },
    Cancel {
        run_id: String,
    },
    Approve {
        run_id: String,
        approval_id: ApprovalRequestId,
        decision: ApprovalDecision,
    },
    CancelSessionSelection {
        request_id: u64,
    },
    SelectSession {
        session_id: Option<String>,
    },
    SessionTree {
        session_id: String,
        limit: usize,
        cursor: Option<TreeCursor>,
    },
    SessionSearch {
        session_id: String,
        query: String,
        limit: usize,
        cursor: Option<SearchCursor>,
    },
    Permissions,
    Revoke,
    Shutdown,
}
#[derive(Debug, Serialize)]
pub struct Failure {
    pub code: &'static str,
    pub message: String,
}
impl Failure {
    pub fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}
#[derive(Serialize)]
pub struct Response {
    pub version: u32,
    pub r#type: &'static str,
    pub id: Option<u64>,
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<Failure>,
}
impl Response {
    pub fn new(id: Option<u64>, result: Result<Value, Failure>) -> Self {
        let (result, error) = match result {
            Ok(v) => (Some(v), None),
            Err(e) => (None, Some(e)),
        };
        Self {
            version: VERSION,
            r#type: "response",
            id,
            ok: error.is_none(),
            result,
            error,
        }
    }
}

pub fn parse(line: &str, last: &mut u64) -> Result<(u64, Command), Response> {
    // Read the small header first: reserve valid IDs even for rejected commands/versions.
    let value: Value = serde_json::from_str(line).map_err(|_| {
        Response::new(
            None,
            Err(Failure::new("invalid_request", "Expected one JSON object")),
        )
    })?;
    let id = value
        .get("id")
        .and_then(Value::as_u64)
        .filter(|id| (1..=MAX_ID).contains(id));
    let Some(id) = id else {
        return Err(Response::new(
            None,
            Err(Failure::new(
                "invalid_id",
                "ID must be a positive JavaScript-safe integer",
            )),
        ));
    };
    if id <= *last {
        return Err(Response::new(
            Some(id),
            Err(Failure::new(
                "stale_id",
                "IDs must strictly increase; the operation was not repeated",
            )),
        ));
    }
    *last = id;
    let request: Request = serde_json::from_value(value).map_err(|_| {
        Response::new(
            Some(id),
            Err(Failure::new(
                "invalid_request",
                "Expected version, id and command; no additional fields",
            )),
        )
    })?;
    debug_assert_eq!(id, request.id);
    if request.version != VERSION {
        return Err(Response::new(
            Some(id),
            Err(Failure::new(
                "unsupported_version",
                "Only RPC version 1 is supported",
            )),
        ));
    }
    let command: Command = serde_json::from_value(request.command).map_err(|_| {
        Response::new(
            Some(id),
            Err(Failure::new(
                "invalid_command",
                "Unknown command, fields or arguments",
            )),
        )
    })?;
    if let Command::CancelSessionSelection { request_id } = command
        && !(1..=MAX_ID).contains(&request_id)
    {
        return Err(Response::new(
            Some(id),
            Err(Failure::new(
                "invalid_command",
                "request_id must be a positive JavaScript-safe integer",
            )),
        ));
    }
    Ok((id, command))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rejected_commands_reserve_ids_and_unknown_fields_cannot_smuggle_rules() {
        let mut last = 0;
        for (line, code) in [
            (
                r#"{"version":2,"id":1,"command":{"type":"initialize"}}"#,
                "unsupported_version",
            ),
            (
                r#"{"version":1,"id":1,"command":{"type":"prompt","text":"never"}}"#,
                "stale_id",
            ),
            (
                r#"{"version":1,"id":2,"command":{"type":"approve","run_id":"x","approval_id":{"run_id":"x","sequence":1},"decision":"allow_once","rules":["*"]}}"#,
                "invalid_command",
            ),
            (
                r#"{"version":1,"id":3,"command":{"type":"initialize"},"workspace":"/"}"#,
                "invalid_request",
            ),
            (
                r#"{"version":1,"id":9007199254740992,"command":{"type":"initialize"}}"#,
                "invalid_id",
            ),
        ] {
            let Err(response) = parse(line, &mut last) else {
                panic!("accepted invalid command")
            };
            assert_eq!(response.error.unwrap().code, code);
        }
        assert_eq!(last, 3);
        assert!(
            parse(
                r#"{"version":1,"id":4,"command":{"type":"initialize"}}"#,
                &mut last
            )
            .is_ok()
        );
    }
}
