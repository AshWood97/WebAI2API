//! Rust-owned site workflows.
//!
//! Adapters issue generic browser RPC operations through [`PageClient`]. Site
//! decisions and response parsing stay in Rust; the browser runtime does not
//! load a site adapter.

mod claude_text;
mod client;
mod rpc_page;
pub mod sites;
mod test_adapter;

pub use claude_text::{ClaudeTextAdapter, CLAUDE_MODELS, CLAUDE_TARGET_URL};
pub use client::{
    AdapterError, AdapterOutput, BrowserEvent, DownloadResult, GenerateRequest, PageClient,
    SiteAdapter,
};
pub use rpc_page::RpcPage;
pub use test_adapter::{TestAdapter, TEST_MODELS};
