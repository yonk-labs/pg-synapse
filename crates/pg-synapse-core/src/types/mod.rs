//! Data types exchanged across kernel trait boundaries.
//!
//! Submodules mirror the trait modules: [`message`] for conversation rows,
//! [`tool`] for tool I/O, [`llm`] for completion requests/responses,
//! [`embedding`] for vectors, [`memory`] for memory entries, [`compression`]
//! for compression I/O, [`context`] for [`ExecutionContext`], [`outcome`] for
//! [`ExecutorOutcome`], and [`profile`] for the SQL profile-row views.

pub mod compression;
pub mod context;
pub mod embedding;
pub mod llm;
pub mod memory;
pub mod message;
pub mod outcome;
pub mod profile;
pub mod tool;
pub mod trace;

pub use compression::{Compressed, CompressionBudget};
pub use context::ExecutionContext;
pub use embedding::EmbeddingVector;
pub use llm::{
    CompletionChunk, CompletionRequest, CompletionResponse, ToolCall, ToolDefinition, Usage,
};
pub use memory::{MemoryEntry, MemoryId, MemoryScope, MemorySnapshot};
pub use message::{Message, Role};
pub use outcome::{ExecutorOutcome, OutcomeStatus};
pub use profile::{AgentRow, EmbeddingProfileRow, LlmProfileRow};
pub use tool::{ToolCtx, ToolOutput, ToolSchema};
pub use trace::{EventKind, ExecutionEvent, TraceLevel};
