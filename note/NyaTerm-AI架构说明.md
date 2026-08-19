# NyaTerm AI 功能架构说明

> 基于 `nyakang/nyaterm`（v1.2.3）实际代码梳理，聚焦 AI 相关子系统。
> 技术栈：Tauri 2 + React 19 + TypeScript（前端）/ Rust（后端）。
> 核心依赖：Rust `genai` crate（多 LLM 提供商统一抽象，作者 Jeremy Chone）。

---

## 一、整体定位

NyaTerm 的 AI 能力本质是一个**终端场景的 AI 助手 + 自动化 Agent**，它把「命令生成/解释」与「ReAct 式自主执行」两个层次统一在同一个流式架构下，并支持三种可插拔的 Agent 后端：

| 后端 | 枚举值 | 实现 | 说明 |
|---|---|---|---|
| 内置 Agent | `AiAgentKind::Nyaterm` | `genai` crate + 自研 ReAct 循环 | 直接调各 LLM 厂商 API |
| Codex | `AiAgentKind::Codex` | `codex` CLI 的 app-server 模式 | OpenAI Codex CLI 集成 |
| Claude Code | `AiAgentKind::ClaudeCode` | `claude code` CLI 的 stream-json 模式 | Anthropic Claude Code CLI 集成 |

三种后端对外暴露**同一套流式事件协议**，前端无需感知底层差异。

---

## 二、分层架构

```text
┌────────────────────────────────────────────────────────────────┐
│  React 前端 (src/)                                              │
│  components/panel/ai/   AIAssistantPanel · AgentStepView        │
│                         AICommandCardView · AssistantReasoning  │
│                         AssistantResponse · ModelCombobox       │
│  components/dialog/ai/  AutoExecutionConfirmDialog · ...        │
│  components/settings/AiTab.tsx         AI 设置页                │
│  lib/aiSettings.ts      提供商/模型配置 · lib/aiEvents.ts 跨组件 │
│  lib/terminalContext.ts 采集终端上下文                          │
└───────────────┬────────────────────────────────────────────────┘
                │  invoke(command, args)  /  listen("ai-stream-{id}")
                │  (Tauri IPC)
┌───────────────▼────────────────────────────────────────────────┐
│  Rust 后端 (src-tauri/src/)                                     │
│  cmd/ai.rs            Tauri 命令层（18 个 command）             │
│  core/ai/             AI 核心模块                                │
│    stream.rs          流编排 + 取消管理 + 事件发射               │
│    agent.rs           内置 ReAct Agent 循环（2455 行）           │
│    model.rs           模型解析 + genai Client 构建              │
│    prompt.rs          System/Agent/观察 提示词（4 语言）         │
│    parser.rs          输出解析（JSON 命令卡 / <think> 块）       │
│    redaction.rs       敏感信息脱敏                              │
│    history.rs         会话/消息/审计持久化                      │
│    codex.rs           Codex app-server 集成                     │
│    external/          ExternalAgentRuntime trait                │
│      claude_code.rs   Claude Code CLI 集成                      │
│  config/settings/ai.rs  AI 配置模型 + 密钥加解密（930 行）      │
└───────────────┬────────────────────────────────────────────────┘
                │  genai::Client / reqwest / 子进程(CLI)
┌───────────────▼────────────────────────────────────────────────┐
│  外部：OpenAI · Anthropic · Gemini · DeepSeek · Groq · Ollama   │
│        xAI · Cohere · Mimo · ZAI · 任意 OpenAI 兼容服务          │
│        codex CLI · claude CLI                                   │
└────────────────────────────────────────────────────────────────┘
```

---

## 三、核心数据模型（config/settings/ai.rs）

| 枚举 | 取值 | 作用 |
|---|---|---|
| `AiMode` | `Ask` / `Agent` | 问答模式 / 自主执行模式 |
| `AiAgentKind` | `Nyaterm` / `Codex` / `ClaudeCode` | 三种 Agent 后端 |
| `AiBackendKind` | `Genai` / `Codex` | 模型所属后端 |
| `AiProviderKind` | OpenAI/Anthropic/Gemini/DeepSeek/Groq/Ollama/Xai/Cohere/Mimo/Zai/OpenaiCompatible | 11 种提供商 |
| `AiPermissionMode` | `Observer` / `Confirm` / `Auto` | 命令执行权限策略 |
| `AgentCommandExecutionMode` | `ConfirmEach` / `Smart` / `Auto` | 内置 Agent 执行策略 |
| `AiReasoningEffort` | Auto/None/Low/Medium/High/XHigh | 推理强度 |
| `RiskLevel` | Low/Medium/High/Critical | 风险分级 |
| `AiAction` | GenerateCommand/ExplainOutput/ExplainSelected/AnalyzeError/RepairFromSelection/CustomTerminalAction/CustomFileAction | 动作意图 |
| `AiModelSource` | `RustGenai` / `Manual` | 模型来源 |

关键结构体：`AiSettings`（schema_version=5，含 provider_profiles 旧字段 / provider_credentials / models / 自定义动作 / codex / claude_code 子配置）、`AiChatRequest`（一次请求的完整载荷）、`AiCommandCard`（可执行命令卡片）、`AiSessionScope`（会话作用域 Terminal/Workspace/Global/Unbound）。

---

## 四、请求 → 响应主流程

```
前端 AIAssistantPanel
  └─ buildMergedContext + targets  →  assemble AiChatRequest
  └─ invoke("start_ai_chat_stream", { request })
        │
        ▼
cmd/ai.rs::start_ai_chat_stream
        ▼
stream.rs::start_chat_stream
  1. 校验 settings.ai.enabled
  2. 生成 stream_id / session_id（未提供时）
  3. validate_session_scope（会话作用域校验）
  4. 在 ACTIVE_STREAMS 注册 oneshot 取消通道
  5. 按 agent_kind 分派（异步 spawn）：
       Codex      → run_codex_stream
       ClaudeCode → run_claude_code_stream
       Nyaterm+Agent → run_agent_stream
       Nyaterm+Ask   → run_chat_stream
        │
        ▼  全程通过 app.emit("ai-stream-{stream_id}", ...) 推送事件
  事件类型：start / delta / reasoning_delta / done / error
            （Agent 模式额外推送 AgentStepPayload，带 stepIndex）
```

---

## 五、三种 Agent 后端详解

### 5.1 内置 Agent（Nyaterm，genai 后端）

**Ask 模式（`run_chat_stream` → `run_model_stream`）**：
1. 脱敏（可选）→ 保存用户消息（可选）
2. `resolve_request_model` 解析模型 → `build_client` 构建 genai Client
3. `build_prompt` 组装提示词；拼接 system prompt + 历史（`history_turns` 轮）+ 用户消息
4. `client.exec_chat_stream` 流式请求，`tokio::select!` 三路并发：流数据 / 超时 / 取消
5. `parse_model_output` 解析输出（JSON 命令卡 / `<think>` 推理块）→ 保存助手消息 → 发射 done

**Agent 模式（`run_agent_stream`，ReAct 循环）**：
```
for step_index in 0..max_steps(默认10):
    取消检查
    ├─ 原生 Tool Calling（优先）：
    │    agent_tools() = execute_command + final_answer（strict JSON schema）
    │    run_agent_tool_step → parse_agent_tool_invocation
    │    └─ 失败且可降级 → NativeToolCallMode 关闭，回退
    └─ 旧版 JSON 协议（run_agent_legacy_json_step）：
         LLM 直接返回 {"action":"execute_command"|"final_answer",...}

    按 action 分支：
      final_answer      → 结束循环
      execute_command   → assess_agent_command_risk（模型风险 + 本地启发式风险取 max）
                          resolve_agent_command_target（定位目标终端）
                          decide_agent_command_execution（按执行模式/权限决定）
                          ├─ 需审批 → AgentApprovalManager.register 等待前端 respond_agent_step
                          ├─ 执行（前台 capture / 后台 SSH/本地命令）
                          └─ 回填 observation → 下一轮
```

**命令安全（本地启发式）**：`is_root_rm_command`（`rm -rf /` 等）、`is_dangerous_dd_command`（`dd of=` 写盘）等危险模式静态识别，与模型自评风险取较高者。

### 5.2 Codex（app-server 集成，codex.rs）

- `CodexAppServerManager`：管理 codex CLI 的 app-server 生命周期
- 能力：CLI 探测（`detect_cli`，多候选路径）、账号状态（`account_read`）、登录流程（`login_start`/`login_cancel`，device auth）、登出
- 会话：`thread_mode` 支持 Persistent（复用 thread id）/ Ephemeral
- 工具集成：`tool_integration_mode = "nyaterm_mcp"`，通过 MCP 命名空间暴露终端工具
- `run_codex_stream`：把 AiChatRequest 转成 codex turn，解析其 JSON 事件流为统一的 `ExternalAgentEvent`

### 5.3 Claude Code（stream-json CLI 集成，external/claude_code.rs）

- `ClaudeCodeRuntime`：以 `claude -p --output-format stream-json` 子进程方式运行
- 能力：CLI 探测、`auth_status`、构建调用（`build_claude_invocation`，含 system context / permission mode）
- 解析：`extract_text_delta` / `extract_session_id` / `extract_error_message` 等逐行解析 stream-json 输出

### 5.4 外部 Agent 统一抽象（external/mod.rs）

```rust
#[async_trait]
pub trait ExternalAgentRuntime: Send + Sync {
    async fn detect(&self) -> AppResult<AgentDetectionResult>;
    async fn get_auth_status(&self) -> AppResult<AgentAuthStatus>;
    async fn list_models(&self) -> AppResult<Vec<AgentModel>>;
    async fn start_turn(...) -> AppResult<ExternalAgentTurnHandle>;
    async fn resume_turn(...) -> AppResult<ExternalAgentTurnHandle>;
    async fn cancel_turn(&self, turn_id: &str) -> AppResult<()>;
}
```

统一事件枚举 `ExternalAgentEvent`（SessionStarted/TextDelta/ToolCallStarted/CommandStarted/ApprovalRequested/UsageUpdated/Completed/Failed 等）供两种外部 Agent 归一化输出。

---

## 六、模型与提供商抽象（model.rs）

- **模型解析** `resolve_request_model`：请求指定 model_id → 默认模型 → 首个启用模型；Codex 模型强制走 codex 通道（genai 路径直接拒绝）
- **提供商推断**：从模型 id 前缀推断（`openai:`、`deepseek:`、`openai_compatible:` 等）
- **凭据解析**：显式 credential_id 优先，否则按 provider_kind 匹配首个启用凭据；校验（Ollama 免 key、OpenaiCompatible 允许空 key、其余必须 api_key）
- **genai Client 构建** `build_client`：
  - `adapter_kind` 映射：OpenAI/OpenaiCompatible→OpenAI、Anthropic→Anthropic、Gemini→Gemini、Deepseek→DeepSeek、Groq→Groq、Ollama→Ollama；**Xai/Cohere/Mimo/Zai 走 OpenAI 适配器**（本质是 OpenAI 兼容协议）
  - `ServiceTargetResolver`：注入自定义 base_url + api_key（实现自建/中转/本地 Ollama）
  - `WebConfig`：自定义 User-Agent（默认伪装 `codex-tui/0.125.0`）
- **模型发现** `list_model_names`：对 OpenAI 兼容的自定义凭据，用 `reqwest` 直接请求 `/v1/models`（因 genai 的 `all_model_names` 不经过 resolver，无法应用自定义 endpoint）

---

## 七、输出解析（parser.rs）

- `parse_model_output`：优先尝试从原文抽取 JSON（`extract_json_object`，容忍 ```json 围栏），失败则回退纯文本
- 结构：`AiModelOutput { text, reasoning, command_cards }`
- 推理提取：`<think>...</think>` 块提取（thinking 模型）；**当正文为空时把 reasoning 提升为正文**（Qwen3 等把答案全放 reasoning 通道）
- `bind_command_card_targets`：把命令卡绑定到目标终端（单目标直接绑定 / 多目标按 target_terminal_session_id 匹配）

---

## 八、敏感信息脱敏（redaction.rs）

基于正则，在发送 LLM 前对上下文与用户输入脱敏：
私钥（PEM）、`Authorization: Bearer`、password/passwd/pwd、token/api_key/secret_key/access_key、AWS AKIA 访问密钥、`postgres://`/`mysql://`/`mongodb://` 连接串。另有 `redact_marker_values` 用于 CLI 输出中按标记名脱敏。

---

## 九、配置与密钥安全（config/settings/ai.rs）

- **静态加密**：api_key 落盘前 `encrypt_ai_settings`，读取后 `decrypt_ai_settings`
- **脱敏回传**：`mask_ai_settings` 把密钥替换为 `MASKED_SECRET_VALUE` 再发给前端
- **合并保真**：`merge_masked_ai_settings` 识别掩码值，保留后端已存的真实密钥（前端回传掩码不覆盖真实值）
- **迁移**：`normalize_ai_settings` 处理 schema 版本迁移（v2/v3 → v5）、legacy profile → credentials/models 归一化

---

## 十、持久化（history.rs）

- JSON 文件存储 `AiHistoryFile`：sessions（会话，含 scope/agent_kind/external_session_id/backend_metadata）+ messages（用户/助手/系统）+ audit logs
- 命令：get_ai_sessions / get_ai_messages / clear_ai_history / delete_ai_session / rebind_ai_session / append_ai_audit / get_ai_audit_logs
- 审计日志 `AiAuditLog`：记录 action、user_input、generated_command、risk_level、inserted_to_terminal、executed、blocked 等，形成 AI 操作的可追溯链

---

## 十一、前端架构

| 文件 | 职责 |
|---|---|
| `components/panel/ai/AIAssistantPanel.tsx`（1930 行） | 主面板：运行模式（ask/nyaterm_agent/codex_agent/claude_code_agent）、流监听、Agent 步骤映射、命令卡渲染、快捷命令保存 |
| `components/panel/ai/AgentStepView.tsx` | Agent 单步展示（thought/action/observation/status） |
| `components/panel/ai/AICommandCardView.tsx` | 命令卡片（风险标识 + 一键插入终端） |
| `components/panel/ai/AssistantReasoning.tsx` / `AssistantResponse.tsx` / `MarkdownContent.tsx` | 推理折叠 / 正文 / Markdown 渲染 |
| `components/panel/ai/ModelCombobox.tsx` | 模型选择器 |
| `components/dialog/ai/*` | 自动执行确认、清空历史等对话框 |
| `components/settings/AiTab.tsx`（1312 行） | 提供商/模型/密钥/Codex/Claude Code 配置 |
| `lib/aiSettings.ts` | 内置提供商列表、模型 id 生成、模型发现合并 |
| `lib/aiEvents.ts` | `nyaterm:ai-open`（任意位置唤起 AI）、`nyaterm:ai-error-detected` 跨组件事件 |
| `lib/terminalContext.ts` | 采集终端上下文（连接/主机/cwd/os/输出/选中文本） |

**流式事件消费**：`listen("ai-stream-{id}")`，通过判断 payload 是否含 `stepIndex` 区分「普通消息事件」与「Agent 步骤事件」；start/delta/reasoning_delta/done/error 分别驱动 UI 增量更新。

---

## 十二、关键文件清单

| 层 | 文件 | 行数 | 职责 |
|---|---|---|---|
| 后端-命令 | `src-tauri/src/cmd/ai.rs` | 332 | 18 个 Tauri command 入口 |
| 后端-配置 | `src-tauri/src/config/settings/ai.rs` | 930 | 配置模型 + 密钥加解密 + 迁移 |
| 后端-核心 | `core/ai/stream.rs` | 516 | 流编排/取消/事件发射 |
| 后端-核心 | `core/ai/agent.rs` | 2455 | 内置 ReAct Agent |
| 后端-核心 | `core/ai/model.rs` | 795 | 模型解析 + genai Client |
| 后端-核心 | `core/ai/prompt.rs` | 949 | 4 语言提示词 |
| 后端-核心 | `core/ai/parser.rs` | 363 | 输出解析 |
| 后端-核心 | `core/ai/redaction.rs` | 113 | 脱敏 |
| 后端-核心 | `core/ai/history.rs` | 470 | 会话/审计持久化 |
| 后端-核心 | `core/ai/codex.rs` | 1445 | Codex 集成 |
| 后端-核心 | `core/ai/external/claude_code.rs` | 865 | Claude Code 集成 |
| 前端 | `components/panel/ai/AIAssistantPanel.tsx` | 1930 | AI 主面板 |
| 前端 | `components/settings/AiTab.tsx` | 1312 | AI 设置页 |
| 前端 | `lib/aiSettings.ts` | 727 | 提供商/模型配置 |

---

## 十三、设计亮点与观察

1. **一套协议、三种后端**：内置/Codex/Claude Code 统一为 `ai-stream-{id}` 事件流，前端解耦。
2. **原生 Tool Calling + 降级兜底**：优先原生 function calling，失败自动回退旧版 JSON 协议，兼容不支持 tool 的模型。
3. **多层安全防线**：本地启发式危险命令识别 + 模型自评风险取 max + 三级权限策略（Confirm/Smart/Auto）+ 前端确认 + 审计日志 + 发送前脱敏。
4. **密钥零泄露**：落盘加密、回传脱敏、掩码合并保真，前端拿不到明文 key。
5. **模型 id 命名空间化**：`provider:model` 或 `credentialId:model`，天然支持同名模型多凭据（如多个 OpenAI 兼容中转）。
6. **深度思考模型适配**：reasoning_effort 映射、`<think>` 提取、空正文时 reasoning 提升为正文。
