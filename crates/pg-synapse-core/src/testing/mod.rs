//! Scriptable mock providers, tools, and the cassette conformance harness.
//!
//! These types live in the public surface of `pg-synapse-core` so plugin
//! authors and host crates can drive realistic kernel paths without standing
//! up real backends.
//!
//! Two complementary surfaces ship here:
//!
//! * Scriptable mocks ([`MockLlmProvider`], [`MockEmbeddingProvider`],
//!   [`MockTool`], plus factory variants) for unit tests that need to drive
//!   specific provider responses.
//! * The cassette conformance harness ([`Cassette`], [`CassetteProvider`],
//!   [`RecordingProvider`], [`run_conformance`], [`default_conformance_cassette`])
//!   for the PS-5 deterministic conformance suite. See the [`cassette`]
//!   module docs for the full picture.

pub mod cassette;
pub mod mock_embedding;
pub mod mock_factories;
pub mod mock_llm;
pub mod mock_tool;

pub use cassette::{
    Cassette, CassetteEntry, CassetteOutcome, CassetteProvider, ConformanceError,
    RecordingProvider, default_conformance_cassette, run_conformance,
};
pub use mock_embedding::MockEmbeddingProvider;
pub use mock_factories::{MockEmbeddingFactory, MockLlmFactory};
pub use mock_llm::{MockLlmProvider, MockResponse};
pub use mock_tool::MockTool;
