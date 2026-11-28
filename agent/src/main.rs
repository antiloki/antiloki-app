//! antiloki-agent — the sidecar every code-agent pane runs. Three subcommands, one binary:
//!
//!   antiloki-agent hook        Claude Code / Codex PreToolUse hook: the edit whitelist enforced BEFORE
//!                              the write, the context-pack nudge, and the audit line per tool call
//!   antiloki-agent mcp         the stdio MCP server the pane's CLI spawns (live tools + delegation)
//!   antiloki-agent statusline  Claude Code's status line: usage always visible, mirrored to usage.json
//!   antiloki-agent pty-host    RFC 0007: the terminal host, which owns the PTYs so they outlive the serve
//!   antiloki-agent pty         RFC 0007: a small client for the host (ls, spawn, snapshot, send, kill)
//!
//! Ported from antiloki-BE/src/mcp/*.ts (2026-09-18) so the standalone needs no Node: the bake
//! writes this binary's path into the engine configs (cockpit-pane-context-bake.rs).
mod hook;
mod mcp;
mod newer_build;
mod statusline;
#[cfg(unix)]
use antiloki_ptyhost as pty_host;

fn main() {
    let sub = std::env::args().nth(1).unwrap_or_default();
    let code = match sub.as_str() {
        "hook" => hook::main(),
        "mcp" => mcp::main(),
        "statusline" => statusline::main(),
        #[cfg(unix)]
        "pty-host" => pty_host::host_main(&std::env::args().skip(2).collect::<Vec<_>>()),
        #[cfg(unix)]
        "pty" => pty_host::ctl_main(&std::env::args().skip(2).collect::<Vec<_>>()),
        #[cfg(not(unix))]
        "pty-host" | "pty" => { eprintln!("antiloki-agent: the terminal host is not available on this platform yet (RFC 0007, M7)"); 2 }
        "--version" | "-V" => { println!("antiloki-agent {}", env!("CARGO_PKG_VERSION")); 0 }
        _ => { eprintln!("usage: antiloki-agent <hook|mcp|statusline|pty-host|pty>"); 2 }
    };
    std::process::exit(code);
}
