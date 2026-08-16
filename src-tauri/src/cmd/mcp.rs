use std::sync::Arc;

use crate::config;
use crate::core::SessionManager;
use crate::core::ai::AgentApprovalManager;
use crate::core::mcp::{McpServerManager, McpServerStatus};
use crate::error::AppResult;

/// Starts the in-process MCP server using the persisted `ai.mcp` settings.
#[tauri::command]
pub async fn start_mcp_server(
    app: tauri::AppHandle,
    state: tauri::State<'_, Arc<McpServerManager>>,
    session_manager: tauri::State<'_, Arc<SessionManager>>,
    approval_manager: tauri::State<'_, Arc<AgentApprovalManager>>,
) -> AppResult<McpServerStatus> {
    let settings = config::load_app_settings(&app)?;
    state
        .start(
            &app,
            session_manager.inner().clone(),
            approval_manager.inner().clone(),
            settings.ai.mcp,
        )
        .await
}

/// Stops the MCP server, if it is running.
#[tauri::command]
pub async fn stop_mcp_server(
    state: tauri::State<'_, Arc<McpServerManager>>,
) -> AppResult<McpServerStatus> {
    state.stop().await
}

/// Returns the current MCP server status.
#[tauri::command]
pub async fn get_mcp_server_status(
    state: tauri::State<'_, Arc<McpServerManager>>,
) -> AppResult<McpServerStatus> {
    Ok(state.status().await)
}

/// Responds to a pending MCP command approval request.
#[tauri::command]
pub async fn respond_mcp_approval(
    state: tauri::State<'_, Arc<AgentApprovalManager>>,
    key: String,
    approved: bool,
) -> AppResult<()> {
    state.respond(&key, approved).await;
    Ok(())
}
