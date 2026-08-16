//! In-process MCP (Model Context Protocol) server.
//!
//! Exposes NyaTerm's live terminal sessions to external MCP clients over a
//! streamable HTTP endpoint, so an external AI can operate the *same* terminals
//! the user is looking at. All execution reuses the existing
//! [`SessionManager`] / marker-capture pipeline — no PTY logic is duplicated.

mod server;
mod tools;

pub use server::{McpServerManager, McpServerStatus, auto_start};
