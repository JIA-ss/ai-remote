//! The ai-remote controller.
//!
//! - The **agent** is a long-lived per-user daemon: it owns the controller identity and pairings,
//!   keeps one connection per host, and serves local requests (pair, list hosts, MCP sessions).
//! - The **MCP shim** (`ai-remote mcp`) is what an MCP client such as Claude Code starts: it relays
//!   stdio to the agent, which serves an aggregated MCP server over all paired hosts. Many shims
//!   can run at once; they share the agent's connections.

mod agent;
mod aggregator;
mod hosts;
mod shim;

pub use agent::{AgentRequest, run_agent};
pub use hosts::KnownHost;
pub use shim::{connect_agent, run_shim};
