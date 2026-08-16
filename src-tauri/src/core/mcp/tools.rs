//! MCP tool handlers that operate on the user's live terminal sessions.
//!
//! The tools are a thin protocol adapter: every operation routes through
//! [`SessionManager`], reusing the exact same write path (`SessionCommand::Write`)
//! and marker-capture path (`SessionCommand::CaptureExec`) that the built-in AI
//! agent uses. On top of that, MCP adds its own security gate: permission mode,
//! local risk assessment, session allow-list, frontend approval, and an audit
//! trail.

use std::sync::Arc;
use std::time::{Duration, Instant};

use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{CallToolResult, ContentBlock};
use rmcp::{
    ErrorData as McpError, ServerHandler, schemars, tool, tool_handler, tool_router,
};
use serde::Serialize;
use tauri::{AppHandle, Emitter};

use crate::config::{AiExecutionProfile, AiPermissionMode, McpSettings, RiskLevel};
use crate::core::ai::{
    AgentApprovalManager, append_ai_audit, assess_local_command_risk, AppendAiAuditRequest,
};
use crate::core::capture::{CapturedOutput, build_capture_command};
use crate::core::{InputOrigin, InputSensitivity, SessionCommand, SessionManager};
use crate::error::{AppError, AppResult};

/// Default command timeout when the client does not specify one.
const DEFAULT_TIMEOUT_MS: u64 = 30_000;
/// How long to wait for the user to approve/reject a command in Confirm mode.
const APPROVAL_TIMEOUT: Duration = Duration::from_secs(60);

#[derive(Debug, Clone, Serialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct TerminalInfo {
    pub id: String,
    pub name: String,
    pub session_type: String,
    pub connected: bool,
    pub ai_execution_profile: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub connection_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct CommandResult {
    pub output: String,
    pub exit_code: Option<i32>,
    pub duration_ms: u64,
    /// False when the command was sent to the terminal but its output could not
    /// be captured (e.g. `send_only` execution profile).
    pub captured: bool,
}

#[derive(Debug, Clone, serde::Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ExecuteCommandParams {
    /// Target terminal session id (from `list_terminals`).
    pub session_id: String,
    /// A single shell command to execute.
    pub command: String,
    #[serde(default = "default_timeout_ms")]
    pub timeout_ms: u64,
}

#[derive(Debug, Clone, serde::Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct WriteParams {
    pub session_id: String,
    /// Raw input to send (may include control sequences).
    pub data: String,
    #[serde(default)]
    pub send_enter: bool,
}

/// Emitted to the frontend when a command needs user approval in Confirm mode.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct McpApprovalRequest {
    pub key: String,
    pub session_id: String,
    pub command: String,
    pub risk_level: RiskLevel,
}

fn default_timeout_ms() -> u64 {
    DEFAULT_TIMEOUT_MS
}

#[derive(Clone)]
pub struct McpTools {
    pub session_manager: Arc<SessionManager>,
    pub app: AppHandle,
    pub settings: McpSettings,
    pub approval_manager: Arc<AgentApprovalManager>,
}

impl McpTools {
    pub fn new(
        session_manager: Arc<SessionManager>,
        app: AppHandle,
        settings: McpSettings,
        approval_manager: Arc<AgentApprovalManager>,
    ) -> Self {
        Self {
            session_manager,
            app,
            settings,
            approval_manager,
        }
    }

    fn is_session_allowed(&self, session_id: &str) -> bool {
        self.settings.allowed_sessions.is_empty()
            || self
                .settings
                .allowed_sessions
                .iter()
                .any(|id| id == session_id)
    }

    fn check_write_permission(&self) -> AppResult<()> {
        match self.settings.permission_mode {
            AiPermissionMode::Observer => Err(AppError::Config(
                "MCP server is in observer mode (read-only)".to_string(),
            )),
            AiPermissionMode::Confirm | AiPermissionMode::Auto => Ok(()),
        }
    }

    /// Requests frontend approval for a command and waits for the verdict.
    async fn request_approval(
        &self,
        session_id: &str,
        command: &str,
        risk: RiskLevel,
    ) -> AppResult<bool> {
        let key = format!("mcp-{}", uuid::Uuid::new_v4());
        let rx = self.approval_manager.register(key.clone()).await;

        let _ = self.app.emit(
            "mcp-approval-request",
            McpApprovalRequest {
                key,
                session_id: session_id.to_string(),
                command: command.to_string(),
                risk_level: risk,
            },
        );

        match tokio::time::timeout(APPROVAL_TIMEOUT, rx).await {
            Ok(Ok(approved)) => Ok(approved),
            Ok(Err(_)) => Err(AppError::Channel("approval channel closed".to_string())),
            Err(_) => Err(AppError::Config("command approval timed out".to_string())),
        }
    }

    /// Returns the command's local risk level, or an error when it must be
    /// blocked under the current permission mode. In `Confirm` mode every
    /// command is gated behind frontend approval.
    async fn check_command_risk(&self, session_id: &str, command: &str) -> AppResult<RiskLevel> {
        let (risk, _reason) = assess_local_command_risk(command);
        match self.settings.permission_mode {
            AiPermissionMode::Auto => Ok(risk),
            AiPermissionMode::Observer => Err(AppError::Config(
                "MCP server is in observer mode (read-only)".to_string(),
            )),
            AiPermissionMode::Confirm => {
                if self
                    .request_approval(session_id, command, risk.clone())
                    .await?
                {
                    Ok(risk)
                } else {
                    Err(AppError::Config("command rejected by user".to_string()))
                }
            }
        }
    }

    fn record_audit(
        &self,
        session_id: &str,
        command: &str,
        risk: RiskLevel,
        executed: bool,
        blocked: bool,
    ) {
        let request = AppendAiAuditRequest {
            connection_id: Some(session_id.to_string()),
            action: "mcp.execute_command".to_string(),
            user_input: None,
            generated_command: Some(command.to_string()),
            risk_level: Some(risk),
            inserted_to_terminal: executed,
            executed,
            blocked,
        };
        if let Err(error) = append_ai_audit(&self.app, request) {
            tracing::warn!(%error, "Failed to append MCP audit log");
        }
    }

    async fn execute_command_on_terminal(
        &self,
        session_id: &str,
        command: &str,
        timeout_ms: u64,
    ) -> AppResult<CommandResult> {
        let info = self.session_manager.session_info(session_id).await?;
        let profile = info.ai_execution_profile;

        match profile {
            AiExecutionProfile::Disabled => Err(AppError::Config(
                "AI execution is disabled for this session".to_string(),
            )),
            AiExecutionProfile::Auto | AiExecutionProfile::SendOnly => {
                // No marker capture available — send the command into the PTY and
                // report that its output is visible in the user's terminal.
                let started = Instant::now();
                let mut bytes = command.as_bytes().to_vec();
                bytes.push(b'\n');
                self.session_manager
                    .send_command(
                        session_id,
                        SessionCommand::Write {
                            data: bytes,
                            automated: true,
                            origin: InputOrigin::AiAgent,
                            sensitivity: InputSensitivity::Normal,
                        },
                    )
                    .await?;

                Ok(CommandResult {
                    output: "command sent to terminal (output capture unavailable for this session profile)"
                        .to_string(),
                    exit_code: None,
                    duration_ms: started.elapsed().as_millis() as u64,
                    captured: false,
                })
            }
            _ => {
                let marker_id = uuid::Uuid::new_v4().to_string();
                let wrapped = build_capture_command(profile, &marker_id, command).ok_or_else(
                    || {
                        AppError::Config(format!(
                            "unsupported AI execution profile: {profile:?}"
                        ))
                    },
                )?;

                let (tx, rx) = tokio::sync::oneshot::channel();
                self.session_manager
                    .send_command(
                        session_id,
                        SessionCommand::CaptureExec {
                            marker_id,
                            wrapped_command: wrapped.into_bytes(),
                            result_tx: tx,
                        },
                    )
                    .await?;

                let captured: CapturedOutput =
                    match tokio::time::timeout(Duration::from_millis(timeout_ms), rx).await {
                        Ok(Ok(captured)) => captured,
                        Ok(Err(_)) => {
                            return Err(AppError::Channel(
                                "capture channel closed — session may have disconnected"
                                    .to_string(),
                            ));
                        }
                        Err(_) => {
                            return Err(AppError::Config("command timed out".to_string()));
                        }
                    };

                Ok(CommandResult {
                    output: captured.output,
                    exit_code: captured.exit_code,
                    duration_ms: captured.duration_ms,
                    captured: true,
                })
            }
        }
    }
}

#[tool_router]
impl McpTools {
    #[tool(description = "List the user's currently open terminal sessions.")]
    async fn list_terminals(&self) -> Result<CallToolResult, McpError> {
        let terminals: Vec<TerminalInfo> = self
            .session_manager
            .list_sessions()
            .await
            .into_iter()
            .filter(|session| self.is_session_allowed(&session.id))
            .map(|session| TerminalInfo {
                id: session.id,
                name: session.name,
                session_type: format!("{:?}", session.session_type),
                connected: session.connected,
                ai_execution_profile: format!("{:?}", session.ai_execution_profile),
                connection_id: session.connection_id,
            })
            .collect();

        Ok(CallToolResult::success(vec![ContentBlock::text(
            serde_json::to_string_pretty(&terminals).unwrap_or_else(|error| error.to_string()),
        )]))
    }

    #[tool(
        description = "Execute a single shell command in one of the user's terminal sessions and capture its output. The command runs in the user's visible terminal."
    )]
    async fn execute_command(
        &self,
        Parameters(params): Parameters<ExecuteCommandParams>,
    ) -> Result<CallToolResult, McpError> {
        let command = params.command.trim().to_string();
        if command.is_empty() {
            return Err(McpError::invalid_params("command must not be empty", None));
        }

        if !self.is_session_allowed(&params.session_id) {
            return Err(McpError::invalid_params(
                "session is not in the MCP allowed-session list",
                None,
            ));
        }

        if let Err(error) = self.check_write_permission() {
            return Err(McpError::internal_error(error.to_string(), None));
        }

        let risk = match self.check_command_risk(&params.session_id, &command).await {
            Ok(risk) => risk,
            Err(error) => {
                self.record_audit(&params.session_id, &command, RiskLevel::High, false, true);
                return Err(McpError::internal_error(error.to_string(), None));
            }
        };

        match self
            .execute_command_on_terminal(&params.session_id, &command, params.timeout_ms)
            .await
        {
            Ok(result) => {
                self.record_audit(&params.session_id, &command, risk, true, false);
                Ok(CallToolResult::success(vec![ContentBlock::text(
                    serde_json::to_string_pretty(&result)
                        .unwrap_or_else(|error| error.to_string()),
                )]))
            }
            Err(error) => {
                self.record_audit(&params.session_id, &command, risk, false, false);
                Err(McpError::internal_error(error.to_string(), None))
            }
        }
    }

    #[tool(description = "Write raw input to one of the user's terminal sessions.")]
    async fn write_to_terminal(
        &self,
        Parameters(params): Parameters<WriteParams>,
    ) -> Result<CallToolResult, McpError> {
        if !self.is_session_allowed(&params.session_id) {
            return Err(McpError::invalid_params(
                "session is not in the MCP allowed-session list",
                None,
            ));
        }

        if let Err(error) = self.check_write_permission() {
            return Err(McpError::internal_error(error.to_string(), None));
        }

        let mut bytes = params.data.into_bytes();
        if params.send_enter {
            bytes.push(b'\n');
        }

        match self
            .session_manager
            .send_command(
                &params.session_id,
                SessionCommand::Write {
                    data: bytes,
                    automated: true,
                    origin: InputOrigin::AiAgent,
                    sensitivity: InputSensitivity::Normal,
                },
            )
            .await
        {
            Ok(()) => Ok(CallToolResult::success(vec![ContentBlock::text(
                "input written to terminal",
            )])),
            Err(error) => Err(McpError::internal_error(error.to_string(), None)),
        }
    }
}

#[tool_handler(
    name = "nyaterm",
    version = "0.2.0",
    instructions = "Operate the user's NyaTerm terminal sessions. Use list_terminals to discover sessions, execute_command to run commands and capture output, and write_to_terminal to send raw input."
)]
impl ServerHandler for McpTools {}
