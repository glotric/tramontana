//! Model provider adapters.
//!
//! `ARCHITECTURE.md` §2: the dependency direction points inward. This crate
//! knows about HTTP, JSON wire formats, and one provider's idea of a streaming
//! response. `tramontana-core` knows none of that, and nothing here decides
//! whether a request *should* be made — routing, permission, and disclosure
//! are the core's, and an adapter that made those choices would put them
//! somewhere the audit record cannot see.
//!
//! Every adapter here passes the same `model_provider_contract` suite the
//! in-process fake passes, over a real socket.

mod anthropic;
mod openai;
mod sse;
mod transport;

pub use anthropic::AnthropicProvider;
pub use openai::{OpenAiCompatibleProvider, ProviderAuth};
