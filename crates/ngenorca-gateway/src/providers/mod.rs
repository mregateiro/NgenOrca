//! LLM Model Provider implementations.
//!
//! Each provider translates [`ChatCompletionRequest`] into the appropriate
//! HTTP calls and maps responses back to [`ChatCompletionResponse`].

pub mod anthropic;
pub mod ollama;
pub mod openai_compat;
pub mod registry;

pub use registry::ProviderRegistry;
