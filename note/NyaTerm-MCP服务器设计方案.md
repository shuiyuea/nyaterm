# NyaTerm 内置 MCP Server 设计方案

> 目标：为 NyaTerm 增加一个 MCP Server，供外部客户端（Claude Desktop / Cursor / Cline / 其他 AI 工具）连接调用，让外部 AI 与用户**操作同一个终端会话**。
> 前提结论：NyaTerm 已具备「共享终端」的核心基础设施，本方案**不修改 PTY/会话核心**，只是把 `SessionManager` 的能力以 MCP tools 形式暴露出去，并叠加鉴权、权限、审计。

---

## 一、核心结论（一句话）

用 `rmcp` 起一个 MCP Server，把 `SessionManager::send_command`（写输入）+ `SessionCommand::CaptureExec`（带 marker 捕获输出）封装成 3~5 个 MCP tools，挂在现有会话管理与安全链路之上；**stdio 传输先落地（零新增 HTTP 依赖），streamable HTTP 作为可选二期支持远程。**

---

## 二、可复用的现有能力（精确到接口）

| 需求 | 现有接口 | 位置 |
|---|---|---|
| 写输入到终端 | `SessionManager::send_command(id, SessionCommand::Write { data, automated, origin, sensitivity })` | `core/session.rs` |
| 执行命令并拿到输出 | `SessionCommand::CaptureExec { marker_id, wrapped_command, result_tx }` | `core/session.rs` |
| 命令包装（带 START/END marker） | `capture::build_capture_command(profile, marker_id, command)` | `core/capture/command.rs` |
| 会话发现/元数据 | `SessionManager::list_sessions()` / `session_info(id)`（含 host、cwd、session_type、ai_execution_profile） | `core/session.rs` |
| 执行 profile（Posix/PowerShell/Cmd） | `AiExecutionProfile` | `config` |
| 命令风险识别 | `assess_agent_command_risk`（模型自评 + 本地启发式取 max） | `core/ai/agent.rs` |
| 前端确认 | `AgentApprovalManager::register/respond` | `core/ai/agent.rs` |
| 审计 | `append_ai_audit` | `core/ai/history.rs` |
| 敏感脱敏 | `redact_sensitive_text` | `core/ai/redaction.rs` |
| 预留的 MCP 模式名 | `tool_integration_mode = "nyaterm_mcp"`（Codex/Claude Code 默认值） | `config/settings/ai.rs` |

> 关键点：`CaptureExec` 会把命令**注入到共享 PTY**（用户界面上可见、可中断），输出通过 `marker_id` 配对回传。这正是「用户和 AI 同一终端」所需的机制，无需重新发明。

---

## 三、总体架构

```
外部 MCP 客户端（Claude Desktop / Cursor / 远程 AI）
        │  MCP 协议（stdio 或 streamable HTTP）
        ▼
┌───────────────────────────────────────────────┐
│ core/mcp/  （新增模块，Rust）                   │
│  McpServerManager（生命周期：start/stop/status）│
│  ├─ transport：stdio / streamable-http          │
│  ├─ tools：list_terminals / execute_command /   │
│  │         write_to_terminal / get_session_info │
│  └─ 安全门：auth → tool/session 白名单 → 权限   │
│             → 风险识别 → 审批 → 审计 → 脱敏     │
└──────────────┬────────────────────────────────┘
               │  复用（不新建）
               ▼
┌───────────────────────────────────────────────┐
│ 现有核心：SessionManager → SessionCommand →    │
│           共享 PTY 会话 I/O 循环                 │
└───────────────────────────────────────────────┘
```

---

## 四、传输层选型

| 传输 | 适用 | 新增依赖 | 说明 |
|---|---|---|---|
| **stdio**（推荐一期） | 本地客户端：Claude Desktop、Cursor、Cline、内置 Codex/Claude Code | 仅 `rmcp`（transport-io） | 零 HTTP 栈，天然单客户端，无鉴权面 |
| **streamable HTTP**（二期） | 远程/多客户端 | `rmcp`（transport-streamable-http-server，引入 axum） | 需 OAuth/token + 仅绑 127.0.0.1，外网走 SSH 隧道/反代 |

当前 `Cargo.toml` 只有 `reqwest`（客户端）、无 HTTP 服务端，因此一期走 stdio 最省事。二期若上 streamable HTTP，建议复用 `rmcp` 内置的 OAuth（或自实现 bearer token），**默认只监听 loopback**。

---

## 五、MCP 工具面设计

命名空间：`nyaterm`（与 `tool_integration_mode = "nyaterm_mcp"` 对齐）。

| 工具 | 类型 | 参数 | 返回 | 复用 |
|---|---|---|---|---|
| `list_terminals` | 只读 | — | 会话列表（id/label/host/cwd/session_type/ai_execution_profile） | `list_sessions()` |
| `get_session_info` | 只读 | session_id | 单会话详情 | `session_info()` |
| `execute_command` | 写 | session_id, command, timeout_ms? | `{ output, exit_code, duration_ms, truncated }` | `CaptureExec` + `build_capture_command` |
| `write_to_terminal` | 写 | session_id, data, send_enter? | 是否写入成功 | `SessionCommand::Write` |

可选扩展（二期）：`read_terminal_output`（读最近输出，可接 recording/history 或捕获缓冲）、`resize`。

---

## 六、「用户与 AI 同一终端」的语义保证

1. **串行化**：所有写入都经过 `SessionManager` 里的单一 `cmd_tx`（mpsc），用户输入与 AI 命令天然排队，不会字节级交错。
2. **可见性**：`execute_command` 走 `CaptureExec`，命令回显在用户终端，输出也完整出现在用户界面——AI 做什么，用户全程可见、可 Ctrl+C 打断。
3. **输出隔离**：靠 `marker_id` 唯一配对 START/END，AI 拿到的输出与用户看到的输出同源但不串味。
4. **并发协调（需新增）**：多外部客户端 + 内置 Agent + 用户同时操作时，同一 session 的 `execute_command` 需**串行化**（session 级互斥/队列），并保证 marker 唯一，避免 capture 结果错配。这是本方案唯一需要新写的核心逻辑。

---

## 七、安全设计（远程可执行 shell，最高优先级）

- **鉴权**：stdio 默认本地进程间，无额外鉴权；streamable HTTP 必须 token/OAuth，默认仅监听 127.0.0.1。
- **权限模式**：复用 `AiPermissionMode`（Observer 只读 / Confirm 需前端确认 / Auto），默认 **Confirm**。
- **风险识别**：每次 `execute_command` 复用 `assess_agent_command_risk`，危险命令强制人工确认。
- **白名单**：可配置「允许暴露的 session」与「允许的工具」白名单。
- **审计**：每次命令执行复用 `append_ai_audit`（action/user_input/generated_command/risk_level/executed/blocked）。
- **脱敏**：读输出方向可选复用 `redact_sensitive_text`。
- **默认关闭**：`enabled=false`，需在设置里显式开启并选定传输。

---

## 八、推荐依赖

- **`rmcp`**（官方 Rust MCP SDK）：`#[tool]` / `#[tool_router]` / `#[tool_handler]` 宏，支持 stdio / streamable HTTP / SSE，与 tokio 无缝；`transport-io` 即可起步，后续按需加 `transport-streamable-http-server`。
- 备选：`rust-mcp-sdk`（rust-mcp-stack，Axum/Actix + OAuth，v1.0 稳定），若后期重度依赖 OAuth/多后端可再评估。

> 落地前建议锁定 `rmcp` 的具体版本与 feature（其宏 API 在 0.2→0.3 有 breaking change）。

---

## 九、落地里程碑

- **M1（最小可用）**：`core/mcp/` + stdio server，`list_terminals` + `execute_command`（复用 `CaptureExec`）；本地 Claude Desktop 可连可执行。
- **M2（安全闭环）**：接入权限模式 + 风险识别 + 前端审批 + 审计 + 脱敏 + session/工具白名单。
- **M3（远程）**：streamable HTTP + token/OAuth，支持多客户端（默认 loopback）。
- **M4（打通内置）**：让内置 Codex / Claude Code 的 `nyaterm_mcp` 模式直接连本 server（收敛两套终端工具）。

---

## 十、风险与注意事项

1. **shell 注入面大**：一旦网络暴露且鉴权失败 = 远程 shell。必须默认关闭 + loopback + 强鉴权 + Confirm 权限。
2. **capture 输出串扰**：并发执行时必须 session 级串行化 + marker 唯一，否则输出错配。
3. **Tauri 状态注入**：`McpServerManager` 需作为 `tauri::State` 注入（参考 `AgentApprovalManager` / `CodexAppServerManager` 的注册方式），生命周期跟随应用。
4. **跨平台 profile**：`execute_command` 必须按会话的 `AiExecutionProfile`（Posix/PowerShell/Cmd）选择包装方式，不能硬编码 POSIX。
5. **前端联动**：Confirm 模式依赖前端 `respond_agent_step` 类回调，MCP 工具调用需接入同一审批通道，否则外部客户端会卡在等待。
