//! Lifecycle management for the in-process MCP server.
//!
//! The server runs a streamable-HTTP endpoint inside the Tauri process, so it
//! shares the same [`SessionManager`] (and therefore the same terminals) as the
//! user. `start`/`stop`/`status` are driven from the frontend via Tauri commands
//! or auto-started from settings on app launch.

use std::net::SocketAddr;
use std::sync::Arc;

use axum::extract::State as AxumState;
use axum::http::header::AUTHORIZATION;
use axum::http::{Request, StatusCode};
use axum::middleware::Next;
use axum::response::Response;
use rmcp::transport::streamable_http_server::{
    StreamableHttpServerConfig, StreamableHttpService, session::local::LocalSessionManager,
};
use serde::Serialize;
use tauri::AppHandle;
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

use crate::config::McpSettings;
use crate::core::SessionManager;
use crate::core::ai::AgentApprovalManager;
use crate::error::{AppError, AppResult};

use super::tools::McpTools;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct McpServerStatus {
    pub running: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bind_address: Option<String>,
}

struct McpServerState {
    /// Cancelling this token triggers the axum graceful shutdown.
    cancellation: Option<CancellationToken>,
    bind_address: Option<String>,
}

impl Default for McpServerState {
    fn default() -> Self {
        Self {
            cancellation: None,
            bind_address: None,
        }
    }
}

/// Bearer-token check for the `/mcp` route. Only applied when an auth token is
/// configured; otherwise the router is built without this layer.
async fn auth_middleware(
    AxumState(token): AxumState<String>,
    req: Request<axum::body::Body>,
    next: Next,
) -> Result<Response, StatusCode> {
    let provided = req
        .headers()
        .get(AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("");
    if provided == format!("Bearer {token}") {
        Ok(next.run(req).await)
    } else {
        Err(StatusCode::UNAUTHORIZED)
    }
}

pub struct McpServerManager {
    state: Arc<Mutex<McpServerState>>,
}

impl Default for McpServerManager {
    fn default() -> Self {
        Self::new()
    }
}

impl McpServerManager {
    pub fn new() -> Self {
        Self {
            state: Arc::new(Mutex::new(McpServerState::default())),
        }
    }

    /// Starts the MCP server using the supplied settings.
    pub async fn start(
        &self,
        app: &AppHandle,
        session_manager: Arc<SessionManager>,
        approval_manager: Arc<AgentApprovalManager>,
        settings: McpSettings,
    ) -> AppResult<McpServerStatus> {
        let mut state = self.state.lock().await;
        if state.cancellation.is_some() {
            return Err(AppError::Config("MCP server is already running".to_string()));
        }

        let addr: SocketAddr = settings
            .bind_address
            .parse()
            .map_err(|error| AppError::Config(format!("invalid bind address: {error}")))?;

        let listener = tokio::net::TcpListener::bind(addr)
            .await
            .map_err(|error| AppError::Config(format!("failed to bind MCP server: {error}")))?;

        let actual_addr = listener
            .local_addr()
            .map_err(|error| AppError::Config(format!("failed to resolve local address: {error}")))?;

        let cancellation = CancellationToken::new();
        let shutdown_token = cancellation.child_token();

        let tools = McpTools::new(session_manager, app.clone(), settings.clone(), approval_manager);
        let service = StreamableHttpService::new(
            move || Ok::<_, std::io::Error>(tools.clone()),
            LocalSessionManager::default().into(),
            StreamableHttpServerConfig::default().with_cancellation_token(shutdown_token),
        );

        let mut router = axum::Router::new().nest_service("/mcp", service);
        if let Some(token) = settings.auth_token.clone().filter(|value| !value.trim().is_empty()) {
            router = router.layer(axum::middleware::from_fn_with_state(token, auth_middleware));
        }

        let shutdown_cancel = cancellation.clone();
        tauri::async_runtime::spawn(async move {
            let _ = axum::serve(listener, router)
                .with_graceful_shutdown(async move {
                    shutdown_cancel.cancelled().await;
                })
                .await;
        });

        let address = actual_addr.to_string();
        state.cancellation = Some(cancellation);
        state.bind_address = Some(address.clone());

        Ok(McpServerStatus {
            running: true,
            bind_address: Some(address),
        })
    }

    /// Stops the MCP server, if it is running.
    pub async fn stop(&self) -> AppResult<McpServerStatus> {
        let mut state = self.state.lock().await;
        if let Some(cancellation) = state.cancellation.take() {
            cancellation.cancel();
        }
        state.bind_address = None;

        Ok(McpServerStatus {
            running: false,
            bind_address: None,
        })
    }

    /// Returns the current server state without mutating it.
    pub async fn status(&self) -> McpServerStatus {
        let state = self.state.lock().await;
        McpServerStatus {
            running: state.cancellation.is_some(),
            bind_address: state.bind_address.clone(),
        }
    }
}

/// Reads app settings and auto-starts the MCP server when enabled.
pub async fn auto_start(
    app: &AppHandle,
    session_manager: Arc<SessionManager>,
    approval_manager: Arc<AgentApprovalManager>,
    manager: Arc<McpServerManager>,
) -> AppResult<Option<McpServerStatus>> {
    let settings = crate::config::load_app_settings(app)?;
    if !settings.ai.mcp.enabled {
        return Ok(None);
    }
    let status = manager
        .start(app, session_manager, approval_manager, settings.ai.mcp)
        .await?;
    Ok(Some(status))
}
