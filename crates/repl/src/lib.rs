//! Long-lived Python REPL runtime used by the RLM loop and by inline
//! `` ```repl `` block execution in the agent loop.

pub mod runtime;
pub mod sandbox;

pub use runtime::{
    BatchResp, PythonRuntime, ReplRound, RpcDispatcher, RpcRequest, RpcResponse, SingleResp,
};
pub use sandbox::{ReplBlock, extract_repl_blocks, has_repl_block};

use std::path::PathBuf;
use uuid::Uuid;

pub fn write_context_file(body: &str) -> std::io::Result<PathBuf> {
    let dir = std::env::temp_dir().join("deepseek_rlm_ctx");
    std::fs::create_dir_all(&dir)?;
    let path = dir.join(format!(
        "session_{}_{}.txt",
        std::process::id(),
        Uuid::new_v4().simple()
    ));
    std::fs::write(&path, body)?;
    Ok(path)
}
