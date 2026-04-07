use std::collections::HashMap;
use std::convert::Infallible;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use async_stream::stream;
use axum::extract::ws::{Message as WsMessage, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Json, Router};
use runtime::{
    BridgeState, ConversationMessage, InboundAction,
    Session as RuntimeSession,
};
use serde::{Deserialize, Serialize};
use tokio::sync::{broadcast, RwLock};

pub type SessionId = String;
pub type SessionStore = Arc<RwLock<HashMap<SessionId, Session>>>;

const BROADCAST_CAPACITY: usize = 64;

#[derive(Clone)]
pub struct AppState {
    sessions: SessionStore,
    next_session_id: Arc<AtomicU64>,
    bridge: Arc<RwLock<BridgeState>>,
}

impl AppState {
    #[must_use]
    pub fn new() -> Self {
        Self {
            sessions: Arc::new(RwLock::new(HashMap::new())),
            next_session_id: Arc::new(AtomicU64::new(1)),
            bridge: Arc::new(RwLock::new(BridgeState::new())),
        }
    }

    fn allocate_session_id(&self) -> SessionId {
        let id = self.next_session_id.fetch_add(1, Ordering::Relaxed);
        format!("session-{id}")
    }
}

impl Default for AppState {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Clone)]
pub struct Session {
    pub id: SessionId,
    pub created_at: u64,
    pub conversation: RuntimeSession,
    events: broadcast::Sender<SessionEvent>,
}

impl Session {
    fn new(id: SessionId) -> Self {
        let (events, _) = broadcast::channel(BROADCAST_CAPACITY);
        Self {
            id,
            created_at: unix_timestamp_millis(),
            conversation: RuntimeSession::new(),
            events,
        }
    }

    fn subscribe(&self) -> broadcast::Receiver<SessionEvent> {
        self.events.subscribe()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
enum SessionEvent {
    Snapshot {
        session_id: SessionId,
        session: RuntimeSession,
    },
    Message {
        session_id: SessionId,
        message: ConversationMessage,
    },
}

impl SessionEvent {
    fn event_name(&self) -> &'static str {
        match self {
            Self::Snapshot { .. } => "snapshot",
            Self::Message { .. } => "message",
        }
    }

    fn to_sse_event(&self) -> Result<Event, serde_json::Error> {
        Ok(Event::default()
            .event(self.event_name())
            .data(serde_json::to_string(self)?))
    }
}

#[derive(Debug, Serialize)]
struct ErrorResponse {
    error: String,
}

type ApiError = (StatusCode, Json<ErrorResponse>);
type ApiResult<T> = Result<T, ApiError>;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CreateSessionResponse {
    pub session_id: SessionId,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SessionSummary {
    pub id: SessionId,
    pub created_at: u64,
    pub message_count: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ListSessionsResponse {
    pub sessions: Vec<SessionSummary>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SessionDetailsResponse {
    pub id: SessionId,
    pub created_at: u64,
    pub session: RuntimeSession,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SendMessageRequest {
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct HealthResponse {
    pub status: String,
    pub version: String,
    pub uptime_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TeleportExportResponse {
    pub session_id: SessionId,
    pub bundle_json: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TeleportImportRequest {
    pub bundle_json: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TeleportImportResponse {
    pub session_id: SessionId,
    pub source_host: String,
}

pub fn app(state: AppState) -> Router {
    Router::new()
        .route("/health", get(health_check))
        .route("/sessions", post(create_session).get(list_sessions))
        .route("/sessions/{id}", get(get_session).delete(delete_session))
        .route("/sessions/{id}/events", get(stream_session_events))
        .route("/sessions/{id}/message", post(send_message))
        .route("/sessions/{id}/teleport", get(teleport_export))
        .route("/sessions/import", post(teleport_import))
        .route("/bridge", get(bridge_ws_handler))
        .route("/bridge/sessions", post(create_bridge_session).get(list_bridge_sessions))
        .with_state(state)
}

async fn create_session(
    State(state): State<AppState>,
) -> (StatusCode, Json<CreateSessionResponse>) {
    let session_id = state.allocate_session_id();
    let session = Session::new(session_id.clone());

    state
        .sessions
        .write()
        .await
        .insert(session_id.clone(), session);

    (
        StatusCode::CREATED,
        Json(CreateSessionResponse { session_id }),
    )
}

async fn list_sessions(State(state): State<AppState>) -> Json<ListSessionsResponse> {
    let sessions = state.sessions.read().await;
    let mut summaries = sessions
        .values()
        .map(|session| SessionSummary {
            id: session.id.clone(),
            created_at: session.created_at,
            message_count: session.conversation.messages.len(),
        })
        .collect::<Vec<_>>();
    summaries.sort_by(|left, right| left.id.cmp(&right.id));

    Json(ListSessionsResponse {
        sessions: summaries,
    })
}

async fn get_session(
    State(state): State<AppState>,
    Path(id): Path<SessionId>,
) -> ApiResult<Json<SessionDetailsResponse>> {
    let sessions = state.sessions.read().await;
    let session = sessions
        .get(&id)
        .ok_or_else(|| not_found(format!("session `{id}` not found")))?;

    Ok(Json(SessionDetailsResponse {
        id: session.id.clone(),
        created_at: session.created_at,
        session: session.conversation.clone(),
    }))
}

async fn send_message(
    State(state): State<AppState>,
    Path(id): Path<SessionId>,
    Json(payload): Json<SendMessageRequest>,
) -> ApiResult<StatusCode> {
    let message = ConversationMessage::user_text(payload.message);
    let broadcaster = {
        let mut sessions = state.sessions.write().await;
        let session = sessions
            .get_mut(&id)
            .ok_or_else(|| not_found(format!("session `{id}` not found")))?;
        session.conversation.messages.push(message.clone());
        session.events.clone()
    };

    let _ = broadcaster.send(SessionEvent::Message {
        session_id: id,
        message,
    });

    Ok(StatusCode::NO_CONTENT)
}

async fn stream_session_events(
    State(state): State<AppState>,
    Path(id): Path<SessionId>,
) -> ApiResult<impl IntoResponse> {
    let (snapshot, mut receiver) = {
        let sessions = state.sessions.read().await;
        let session = sessions
            .get(&id)
            .ok_or_else(|| not_found(format!("session `{id}` not found")))?;
        (
            SessionEvent::Snapshot {
                session_id: session.id.clone(),
                session: session.conversation.clone(),
            },
            session.subscribe(),
        )
    };

    let stream = stream! {
        if let Ok(event) = snapshot.to_sse_event() {
            yield Ok::<Event, Infallible>(event);
        }

        loop {
            match receiver.recv().await {
                Ok(event) => {
                    if let Ok(sse_event) = event.to_sse_event() {
                        yield Ok::<Event, Infallible>(sse_event);
                    }
                }
                Err(broadcast::error::RecvError::Lagged(_)) => {},
                Err(broadcast::error::RecvError::Closed) => break,
            }
        }
    };

    Ok(Sse::new(stream).keep_alive(KeepAlive::new().interval(Duration::from_secs(15))))
}

// ── Bridge endpoints ──────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateBridgeSessionRequest {
    #[serde(default)]
    pub ide_name: Option<String>,
    #[serde(default)]
    pub ide_version: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateBridgeSessionResponse {
    pub session_id: String,
}

async fn create_bridge_session(
    State(state): State<AppState>,
    Json(payload): Json<CreateBridgeSessionRequest>,
) -> (StatusCode, Json<CreateBridgeSessionResponse>) {
    let bridge = state.bridge.read().await;
    let session_id = bridge.create_session(payload.ide_name, payload.ide_version);
    (
        StatusCode::CREATED,
        Json(CreateBridgeSessionResponse { session_id }),
    )
}

async fn list_bridge_sessions(
    State(state): State<AppState>,
) -> Json<serde_json::Value> {
    let bridge = state.bridge.read().await;
    let sessions = bridge.list_sessions();
    Json(serde_json::json!({ "sessions": sessions }))
}

async fn bridge_ws_handler(
    ws: WebSocketUpgrade,
    State(state): State<AppState>,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_bridge_socket(socket, state))
}

async fn handle_bridge_socket(mut socket: WebSocket, state: AppState) {
    // Send initial capabilities
    let init_msg = serde_json::json!({
        "type": "system",
        "uuid": format!("init-{}", unix_timestamp_millis()),
        "content": "Bridge connected",
        "subtype": "bridge_init"
    });
    let _ = socket
        .send(WsMessage::Text(serde_json::to_string(&init_msg).unwrap_or_default().into()))
        .await;

    // Keepalive + message loop
    loop {
        tokio::select! {
            msg = socket.recv() => {
                match msg {
                    Some(Ok(WsMessage::Text(text))) => {
                        let bridge = state.bridge.read().await;
                        if let Some(action) = bridge.process_inbound(&text) {
                            match action {
                                InboundAction::UserMessage { uuid: _, content } => {
                                    // Echo back as assistant (placeholder — real impl routes to conversation runtime)
                                    let response = bridge.build_assistant_message(
                                        &format!("[bridge] Received: {}", &content[..content.len().min(100)])
                                    );
                                    if let Ok(json) = serde_json::to_string(&response) {
                                        let _ = socket.send(WsMessage::Text(json.into())).await;
                                    }
                                }
                                InboundAction::ControlRequest { request_id, request } => {
                                    let response = bridge.handle_control_request(&request_id, &request);
                                    if let Ok(json) = serde_json::to_string(&response) {
                                        let _ = socket.send(WsMessage::Text(json.into())).await;
                                    }
                                }
                                InboundAction::ControlResponse { .. } => {
                                    // Received a response to a request we sent — handle async
                                }
                            }
                        }
                    }
                    Some(Ok(WsMessage::Ping(data))) => {
                        let _ = socket.send(WsMessage::Pong(data)).await;
                    }
                    Some(Ok(WsMessage::Close(_))) | None => break,
                    _ => {}
                }
            }
            () = tokio::time::sleep(Duration::from_secs(15)) => {
                // Send keepalive ping
                let _ = socket.send(WsMessage::Ping(vec![].into())).await;
            }
        }
    }
}

async fn health_check() -> Json<HealthResponse> {
    Json(HealthResponse {
        status: "ok".to_string(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        uptime_ms: unix_timestamp_millis(), // simplified; real uptime would track start time
    })
}

async fn delete_session(
    State(state): State<AppState>,
    Path(id): Path<SessionId>,
) -> ApiResult<StatusCode> {
    let mut sessions = state.sessions.write().await;
    sessions
        .remove(&id)
        .ok_or_else(|| not_found(format!("session `{id}` not found")))?;
    Ok(StatusCode::NO_CONTENT)
}

async fn teleport_export(
    State(state): State<AppState>,
    Path(id): Path<SessionId>,
) -> ApiResult<Json<TeleportExportResponse>> {
    let sessions = state.sessions.read().await;
    let session = sessions
        .get(&id)
        .ok_or_else(|| not_found(format!("session `{id}` not found")))?;

    let bundle = runtime::TeleportBundle {
        version: 1,
        session: session.conversation.clone(),
        source_host: std::env::var("HOSTNAME").unwrap_or_else(|_| "unknown".to_string()),
        exported_at: unix_timestamp_millis().to_string(),
        title: Some(format!("Teleported session {id}")),
    };
    let bundle_json = serde_json::to_string(&bundle)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, Json(ErrorResponse { error: e.to_string() })))?;

    Ok(Json(TeleportExportResponse {
        session_id: id,
        bundle_json,
    }))
}

async fn teleport_import(
    State(state): State<AppState>,
    Json(payload): Json<TeleportImportRequest>,
) -> ApiResult<(StatusCode, Json<TeleportImportResponse>)> {
    let bundle: runtime::TeleportBundle = serde_json::from_str(&payload.bundle_json)
        .map_err(|e| (StatusCode::BAD_REQUEST, Json(ErrorResponse { error: e.to_string() })))?;

    runtime::validate_bundle(&bundle)
        .map_err(|e| (StatusCode::BAD_REQUEST, Json(ErrorResponse { error: e })))?;

    let session_id = state.allocate_session_id();
    let mut session = Session::new(session_id.clone());
    session.conversation = bundle.session;

    state
        .sessions
        .write()
        .await
        .insert(session_id.clone(), session);

    Ok((
        StatusCode::CREATED,
        Json(TeleportImportResponse {
            session_id,
            source_host: bundle.source_host,
        }),
    ))
}

fn unix_timestamp_millis() -> u64 {
    u64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time should be after epoch")
            .as_millis(),
    )
    .unwrap_or(u64::MAX)
}

fn not_found(message: String) -> ApiError {
    (
        StatusCode::NOT_FOUND,
        Json(ErrorResponse { error: message }),
    )
}

#[cfg(test)]
mod tests {
    use super::{
        app, AppState, CreateSessionResponse, ListSessionsResponse, SessionDetailsResponse,
    };
    use reqwest::Client;
    use std::net::SocketAddr;
    use std::time::Duration;
    use tokio::net::TcpListener;
    use tokio::task::JoinHandle;
    use tokio::time::timeout;

    struct TestServer {
        address: SocketAddr,
        handle: JoinHandle<()>,
    }

    impl TestServer {
        async fn spawn() -> Self {
            let listener = TcpListener::bind("127.0.0.1:0")
                .await
                .expect("test listener should bind");
            let address = listener
                .local_addr()
                .expect("listener should report local address");
            let handle = tokio::spawn(async move {
                axum::serve(listener, app(AppState::default()))
                    .await
                    .expect("server should run");
            });

            Self { address, handle }
        }

        fn url(&self, path: &str) -> String {
            format!("http://{}{}", self.address, path)
        }
    }

    impl Drop for TestServer {
        fn drop(&mut self) {
            self.handle.abort();
        }
    }

    async fn create_session(client: &Client, server: &TestServer) -> CreateSessionResponse {
        client
            .post(server.url("/sessions"))
            .send()
            .await
            .expect("create request should succeed")
            .error_for_status()
            .expect("create request should return success")
            .json::<CreateSessionResponse>()
            .await
            .expect("create response should parse")
    }

    async fn next_sse_frame(response: &mut reqwest::Response, buffer: &mut String) -> String {
        loop {
            if let Some(index) = buffer.find("\n\n") {
                let frame = buffer[..index].to_string();
                let remainder = buffer[index + 2..].to_string();
                *buffer = remainder;
                return frame;
            }

            let next_chunk = timeout(Duration::from_secs(5), response.chunk())
                .await
                .expect("SSE stream should yield within timeout")
                .expect("SSE stream should remain readable")
                .expect("SSE stream should stay open");
            buffer.push_str(&String::from_utf8_lossy(&next_chunk));
        }
    }

    #[tokio::test]
    async fn creates_and_lists_sessions() {
        let server = TestServer::spawn().await;
        let client = Client::new();

        // given
        let created = create_session(&client, &server).await;

        // when
        let sessions = client
            .get(server.url("/sessions"))
            .send()
            .await
            .expect("list request should succeed")
            .error_for_status()
            .expect("list request should return success")
            .json::<ListSessionsResponse>()
            .await
            .expect("list response should parse");
        let details = client
            .get(server.url(&format!("/sessions/{}", created.session_id)))
            .send()
            .await
            .expect("details request should succeed")
            .error_for_status()
            .expect("details request should return success")
            .json::<SessionDetailsResponse>()
            .await
            .expect("details response should parse");

        // then
        assert_eq!(created.session_id, "session-1");
        assert_eq!(sessions.sessions.len(), 1);
        assert_eq!(sessions.sessions[0].id, created.session_id);
        assert_eq!(sessions.sessions[0].message_count, 0);
        assert_eq!(details.id, "session-1");
        assert!(details.session.messages.is_empty());
    }

    #[tokio::test]
    async fn streams_message_events_and_persists_message_flow() {
        let server = TestServer::spawn().await;
        let client = Client::new();

        // given
        let created = create_session(&client, &server).await;
        let mut response = client
            .get(server.url(&format!("/sessions/{}/events", created.session_id)))
            .send()
            .await
            .expect("events request should succeed")
            .error_for_status()
            .expect("events request should return success");
        let mut buffer = String::new();
        let snapshot_frame = next_sse_frame(&mut response, &mut buffer).await;

        // when
        let send_status = client
            .post(server.url(&format!("/sessions/{}/message", created.session_id)))
            .json(&super::SendMessageRequest {
                message: "hello from test".to_string(),
            })
            .send()
            .await
            .expect("message request should succeed")
            .status();
        let message_frame = next_sse_frame(&mut response, &mut buffer).await;
        let details = client
            .get(server.url(&format!("/sessions/{}", created.session_id)))
            .send()
            .await
            .expect("details request should succeed")
            .error_for_status()
            .expect("details request should return success")
            .json::<SessionDetailsResponse>()
            .await
            .expect("details response should parse");

        // then
        assert_eq!(send_status, reqwest::StatusCode::NO_CONTENT);
        assert!(snapshot_frame.contains("event: snapshot"));
        assert!(snapshot_frame.contains("\"session_id\":\"session-1\""));
        assert!(message_frame.contains("event: message"));
        assert!(message_frame.contains("hello from test"));
        assert_eq!(details.session.messages.len(), 1);
        assert_eq!(
            details.session.messages[0],
            runtime::ConversationMessage::user_text("hello from test")
        );
    }
}
