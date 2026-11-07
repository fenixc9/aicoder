use crate::{
    WebHost,
    host::{MAX_BODY, MAX_ID, Run},
};
use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, Path, Request, State, rejection::JsonRejection},
    http::{HeaderMap, StatusCode, header},
    middleware::{self, Next},
    response::{
        Html, IntoResponse, Response, Sse,
        sse::{Event, KeepAlive},
    },
    routing::{get, post},
};
use futures::{Stream, stream};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    convert::Infallible,
    io::{self, Write},
    sync::Arc,
    time::Duration,
};
use tower::limit::ConcurrencyLimitLayer;

pub(crate) fn router(host: WebHost) -> Router {
    Router::new()
        .route("/", get(|| async { Html(include_str!("index.html")) }))
        .route(
            "/app.js",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "text/javascript; charset=utf-8")],
                    include_str!("app.js"),
                )
            }),
        )
        .route(
            "/style.css",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "text/css; charset=utf-8")],
                    include_str!("style.css"),
                )
            }),
        )
        .route("/api/state", get(state))
        .route("/api/runs", post(start))
        .route("/api/session/fork", post(start_fork))
        .route("/api/session/operation", get(session_operation))
        .route(
            "/api/session/operation/{id}/cancel",
            post(cancel_session_operation),
        )
        .route("/api/session/operation/{id}/ui", post(session_operation_ui))
        .route("/api/commands", get(commands).post(start_command))
        .route("/api/commands/{id}", get(command_status))
        .route("/api/commands/{id}/ui", post(command_ui))
        .route("/api/commands/{id}/cancel", post(cancel_command))
        .route("/api/permissions/revoke", post(revoke))
        .route("/api/runs/{id}", get(status))
        .route("/api/runs/{id}/events", get(events))
        .route("/api/runs/{id}/cancel", post(cancel))
        .route("/api/runs/{id}/approval", post(approve))
        .route("/api/runs/{id}/ui", post(answer_ui))
        .fallback(|| async { error(StatusCode::NOT_FOUND, "not_found", "Unknown route") })
        .layer(DefaultBodyLimit::max(MAX_BODY))
        .layer(ConcurrencyLimitLayer::new(32))
        .layer(middleware::from_fn_with_state(host.clone(), guard))
        .with_state(host)
}
fn error(status: StatusCode, code: &str, message: &str) -> Response {
    (
        status,
        Json(json!({"version":1,"error":{"code":code,"message":message}})),
    )
        .into_response()
}
async fn guard(State(host): State<WebHost>, request: Request, next: Next) -> Response {
    let expected = host.0.options.address.to_string();
    let origin = format!("http://{expected}");
    let headers = request.headers();
    let response =
        if headers.get(header::HOST).and_then(|v| v.to_str().ok()) != Some(expected.as_str()) {
            error(StatusCode::FORBIDDEN, "host_denied", "Unexpected host")
        } else if headers
            .get(header::ORIGIN)
            .is_some_and(|v| v.to_str().ok() != Some(origin.as_str()))
        {
            error(StatusCode::FORBIDDEN, "origin_denied", "Unexpected origin")
        } else if request.uri().path().starts_with("/api")
            && !authorized(headers, &host.0.options.token)
        {
            error(
                StatusCode::UNAUTHORIZED,
                "unauthorized",
                "Web authentication required",
            )
        } else {
            match tokio::time::timeout(Duration::from_secs(10), next.run(request)).await {
                Ok(response) => response,
                Err(_) => error(
                    StatusCode::REQUEST_TIMEOUT,
                    "request_timeout",
                    "Request timed out",
                ),
            }
        };
    let mut response = response;
    let h = response.headers_mut();
    h.insert(header::CACHE_CONTROL, "no-store".parse().unwrap());
    h.insert(header::X_CONTENT_TYPE_OPTIONS, "nosniff".parse().unwrap());
    h.insert(header::CONTENT_SECURITY_POLICY,"default-src 'none'; script-src 'self'; style-src 'self'; connect-src 'self'; base-uri 'none'; form-action 'none'; frame-ancestors 'none'".parse().unwrap());
    response
}
fn authorized(headers: &HeaderMap, token: &str) -> bool {
    let Some(value) = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
    else {
        return false;
    };
    let mut mismatch = value.len() ^ token.len();
    for i in 0..256 {
        mismatch |= usize::from(
            value.as_bytes().get(i).copied().unwrap_or(0)
                ^ token.as_bytes().get(i).copied().unwrap_or(0),
        );
    }
    mismatch == 0
}
async fn state(State(host): State<WebHost>) -> Response {
    let s = host.0.state.lock().expect("Web state");
    bounded_json(
        &json!({"version":1,"closing":s.closing,"session_id":match &s.selection{aicoder_core::SessionSelection::Existing(id)=>Some(id),_=>None},"next_prompt_id":s.last_id.checked_add(1).filter(|id|*id<=MAX_ID),"run":s.current.as_ref().map(|r|r.snapshot()),"command":s.command.as_ref().map(|job|job.snapshot()),"session_operation":s.session_operation.as_ref().map(|job|job.snapshot())}),
    )
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Prompt {
    id: u64,
    text: String,
}
fn body<T>(value: Result<Json<T>, JsonRejection>) -> Result<T, Box<Response>> {
    value.map(|Json(v)| v).map_err(|error_value| {
        Box::new(error(
            error_value.status(),
            "invalid_body",
            "Expected bounded JSON with the declared fields",
        ))
    })
}
async fn start(
    State(host): State<WebHost>,
    payload: Result<Json<Prompt>, JsonRejection>,
) -> Response {
    let payload = match body(payload) {
        Ok(v) => v,
        Err(response) => return *response,
    };
    match host.start(payload.id,&payload.text){
        Ok(run)=>(StatusCode::ACCEPTED,Json(json!({"version":1,"run_id":run.handle.run_id(),"events":format!("/api/runs/{}/events",run.handle.run_id())}))).into_response(),
        Err((code,message))=>error(if code=="invalid_input"{StatusCode::BAD_REQUEST}else{StatusCode::CONFLICT},code,message),
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ForkInput {
    id: u64,
    point: aicoder_core::session::ForkPoint,
}
async fn start_fork(
    State(host): State<WebHost>,
    payload: Result<Json<ForkInput>, JsonRejection>,
) -> Response {
    let p = match body(payload) {
        Ok(v) => v,
        Err(r) => return *r,
    };
    match host.start_fork(p.id, p.point) {
        Ok(job) => (
            StatusCode::ACCEPTED,
            Json(json!({"version":1,"operation_id":job.id,"request_id":job.request_id})),
        )
            .into_response(),
        Err((code, message)) => error(StatusCode::CONFLICT, code, message),
    }
}
async fn session_operation(State(host): State<WebHost>) -> Response {
    bounded_json(
        &json!({"version":1,"operation":host.session_operation().map(|job|job.snapshot())}),
    )
}
async fn cancel_session_operation(
    State(host): State<WebHost>,
    Path(id): Path<String>,
    payload: Result<Json<Empty>, JsonRejection>,
) -> Response {
    if let Err(response) = body(payload) {
        return *response;
    }
    let Some(job) = host.session_operation().filter(|job| job.id == id) else {
        return error(
            StatusCode::NOT_FOUND,
            "unknown_operation",
            "No owned session operation matches",
        );
    };
    job.cancel("Web session operation cancelled");
    bounded_json(&json!({"cancel_requested":true}))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SessionUiInput {
    question_id: aicoder_core::ui::UiRequestId,
    answer: aicoder_core::ui::UiAnswer,
}
async fn session_operation_ui(
    State(host): State<WebHost>,
    Path(id): Path<String>,
    payload: Result<Json<SessionUiInput>, JsonRejection>,
) -> Response {
    let p = match body(payload) {
        Ok(v) => v,
        Err(r) => return *r,
    };
    let Some(job) = host.session_operation().filter(|job| job.id == id) else {
        return error(
            StatusCode::NOT_FOUND,
            "unknown_operation",
            "No owned session operation matches",
        );
    };
    match job.ui.answer(p.question_id, p.answer) {
        Ok(()) => bounded_json(&json!({"answered":true})),
        Err(_) => error(
            StatusCode::CONFLICT,
            "invalid_ui_answer",
            "Question expired or answer invalid",
        ),
    }
}
async fn commands(State(host): State<WebHost>) -> Response {
    bounded_json(&json!({"version":1,"commands":host.commands()}))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CommandInput {
    id: u64,
    name: String,
    arguments: Value,
}
async fn start_command(
    State(host): State<WebHost>,
    payload: Result<Json<CommandInput>, JsonRejection>,
) -> Response {
    let payload = match body(payload) {
        Ok(v) => v,
        Err(response) => return *response,
    };
    match host.start_command(payload.id, payload.name, payload.arguments) {
        Ok(job) => (StatusCode::ACCEPTED, Json(job.snapshot())).into_response(),
        Err((code, message)) => error(
            if matches!(code, "unknown_command" | "invalid_input") {
                StatusCode::BAD_REQUEST
            } else {
                StatusCode::CONFLICT
            },
            code,
            message,
        ),
    }
}
async fn command_status(State(host): State<WebHost>, Path(id): Path<String>) -> Response {
    match host.command(&id) {
        Some(job) => bounded_json(&job.snapshot()),
        None => error(
            StatusCode::NOT_FOUND,
            "unknown_command_job",
            "Command is not owned by this host",
        ),
    }
}
async fn command_ui(
    State(host): State<WebHost>,
    Path(id): Path<String>,
    payload: Result<Json<UiReply>, JsonRejection>,
) -> Response {
    let payload = match body(payload) {
        Ok(v) => v,
        Err(response) => return *response,
    };
    let Some(job) = host.command(&id) else {
        return error(
            StatusCode::NOT_FOUND,
            "unknown_command_job",
            "Command is not owned by this host",
        );
    };
    match job.ui.answer(payload.question_id, payload.answer) {
        Ok(()) => Json(json!({"version":1,"answered":true})).into_response(),
        Err(aicoder_core::ui::UiError::Invalid) => error(
            StatusCode::BAD_REQUEST,
            "invalid_ui_answer",
            "Answer does not match the pending question",
        ),
        Err(_) => error(
            StatusCode::CONFLICT,
            "stale_ui_question",
            "Question is closed, expired, foreign or already answered",
        ),
    }
}
async fn cancel_command(
    State(host): State<WebHost>,
    Path(id): Path<String>,
    payload: Result<Json<Empty>, JsonRejection>,
) -> Response {
    if let Err(response) = body(payload) {
        return *response;
    }
    let Some(job) = host.command(&id) else {
        return error(
            StatusCode::NOT_FOUND,
            "unknown_command_job",
            "Command is not owned by this host",
        );
    };
    if host.cancel_command(&job.id) {
        Json(json!({"version":1,"cancellation_requested":true})).into_response()
    } else {
        error(
            StatusCode::CONFLICT,
            "command_closed",
            "Command no longer accepts cancellation",
        )
    }
}

async fn status(State(host): State<WebHost>, Path(id): Path<String>) -> Response {
    match host.run(&id) {
        Some(run) => bounded_json(&run.snapshot()),
        None => error(
            StatusCode::NOT_FOUND,
            "unknown_run",
            "Run is not owned by this host",
        ),
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Empty {}
async fn revoke(
    State(host): State<WebHost>,
    payload: Result<Json<Empty>, JsonRejection>,
) -> Response {
    if let Err(response) = body(payload) {
        return *response;
    }
    Json(json!({"version":1,"revoked":host.revoke()})).into_response()
}
async fn cancel(
    State(host): State<WebHost>,
    Path(id): Path<String>,
    payload: Result<Json<Empty>, JsonRejection>,
) -> Response {
    if let Err(response) = body(payload) {
        return *response;
    }
    let Some(run) = host.run(&id) else {
        return error(
            StatusCode::NOT_FOUND,
            "unknown_run",
            "Run is not owned by this host",
        );
    };
    run.ui.close();
    match run.handle.cancel("Web cancel") {
        Ok(()) => Json(json!({"version":1,"cancellation_requested":true})).into_response(),
        Err(_) => error(
            StatusCode::CONFLICT,
            "control_closed",
            "Run no longer accepts cancellation",
        ),
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct UiReply {
    question_id: aicoder_core::ui::UiRequestId,
    answer: aicoder_core::ui::UiAnswer,
}
async fn answer_ui(
    State(host): State<WebHost>,
    Path(id): Path<String>,
    payload: Result<Json<UiReply>, JsonRejection>,
) -> Response {
    let payload = match body(payload) {
        Ok(v) => v,
        Err(response) => return *response,
    };
    let Some(run) = host.run(&id) else {
        return error(
            StatusCode::NOT_FOUND,
            "unknown_run",
            "Run is not owned by this host",
        );
    };
    match run.ui.answer(payload.question_id, payload.answer) {
        Ok(()) => Json(json!({"version":1,"answered":true})).into_response(),
        Err(aicoder_core::ui::UiError::Invalid) => error(
            StatusCode::BAD_REQUEST,
            "invalid_ui_answer",
            "Answer does not match the pending question",
        ),
        Err(_) => error(
            StatusCode::CONFLICT,
            "stale_ui_question",
            "Question is closed, expired, foreign or already answered",
        ),
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Approval {
    approval_id: aicoder_core::ApprovalRequestId,
    decision: aicoder_core::tools::ApprovalDecision,
}
async fn approve(
    State(host): State<WebHost>,
    Path(id): Path<String>,
    payload: Result<Json<Approval>, JsonRejection>,
) -> Response {
    let payload = match body(payload) {
        Ok(v) => v,
        Err(response) => return *response,
    };
    let Some(run) = host.run(&id) else {
        return error(
            StatusCode::NOT_FOUND,
            "unknown_run",
            "Run is not owned by this host",
        );
    };
    match run
        .handle
        .respond_approval_decision(&payload.approval_id, payload.decision)
    {
        Ok(()) => Json(json!({"version":1,"answered":true})).into_response(),
        Err(_) => error(
            StatusCode::CONFLICT,
            "approval_expired",
            "Approval is unknown, expired or already answered",
        ),
    }
}
const MAX_EVENT: usize = 8 * 1024 * 1024;
struct Encoded(Vec<u8>);
impl Write for Encoded {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.len() > MAX_EVENT.saturating_sub(self.0.len()) {
            return Err(io::Error::other("SSE event exceeds 8 MiB"));
        }
        self.0.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
fn encode(value: &impl Serialize) -> Result<String, io::Error> {
    let mut buffer = Encoded(Vec::new());
    serde_json::to_writer(&mut buffer, value).map_err(io::Error::other)?;
    Ok(String::from_utf8(buffer.0).expect("JSON is UTF-8"))
}
fn bounded_json(value: &Value) -> Response {
    match encode(value) {
        Ok(data) => ([(header::CONTENT_TYPE, "application/json")], data).into_response(),
        Err(_) => error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "serialization_limit",
            "Response exceeds host serialization budget",
        ),
    }
}
struct StreamState {
    run: Arc<Run>,
    subscription: Option<aicoder_core::events::EventSubscription>,
    finished: bool,
    _permit: tokio::sync::OwnedSemaphorePermit,
}
async fn events(
    State(host): State<WebHost>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Response {
    if headers.contains_key("last-event-id") {
        return error(
            StatusCode::CONFLICT,
            "replay_unavailable",
            "Use status; SSE replay is unavailable",
        );
    }
    let Some(run) = host.run(&id) else {
        return error(
            StatusCode::NOT_FOUND,
            "unknown_run",
            "Run is not owned by this host",
        );
    };
    let Ok(permit) = host.0.streams.clone().try_acquire_owned() else {
        return error(
            StatusCode::SERVICE_UNAVAILABLE,
            "stream_limit",
            "Host SSE consumer limit reached",
        );
    };
    let Some(subscription) = run.events.lock().expect("Web events").take() else {
        return error(
            StatusCode::CONFLICT,
            "observer_claimed",
            "This run already had an SSE consumer",
        );
    };
    Sse::new(event_stream(StreamState {
        run,
        subscription: Some(subscription),
        finished: false,
        _permit: permit,
    }))
    .keep_alive(KeepAlive::new().interval(Duration::from_secs(15)))
    .into_response()
}
fn event_stream(state: StreamState) -> impl Stream<Item = Result<Event, Infallible>> {
    stream::unfold(state, |mut s| async move {
        if s.finished {
            return None;
        }
        if let Some(subscription) = &mut s.subscription {
            match subscription.recv().await {
                Ok(Some(event)) => match encode(event.as_ref()) {
                    Ok(data) => {
                        return Some((
                            Ok(Event::default()
                                .event("agent")
                                .id(format!("{}/{}", event.run_id, event.sequence))
                                .data(data)),
                            s,
                        ));
                    }
                    Err(_) => {
                        s.subscription = None;
                        return Some((
                            Ok(Event::default()
                                .event("observation_error")
                                .data("{\"code\":\"serialization_limit\"}")),
                            s,
                        ));
                    }
                },
                Err(error) => {
                    s.subscription = None;
                    return Some((
                        Ok(Event::default()
                            .event("observation_error")
                            .data(encode(&error).expect("small stream error"))),
                        s,
                    ));
                }
                Ok(None) => s.subscription = None,
            }
        }
        let result = s.run.done().await;
        s.finished = true;
        let event = match encode(&result) {
            Ok(data) => Event::default().event("result").data(data),
            Err(_) => Event::default()
                .event("observation_error")
                .data("{\"code\":\"result_serialization_limit\"}"),
        };
        Some((Ok(event), s))
    })
}
