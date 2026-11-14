//! Long-lived Python REPL runtime used by the RLM loop and by inline
//! `` ```repl `` block execution in the agent loop.

pub mod fence;
pub mod runtime;

pub use fence::{ReplBlock, extract_repl_blocks, has_repl_block};
pub use runtime::{
    BatchResp, INLINE_ROUND_TIMEOUT, PythonRuntime, ReplRound, RpcDispatcher, RpcRequest,
    RpcResponse, SingleResp,
};
