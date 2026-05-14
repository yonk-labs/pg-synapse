//! ONNX Runtime backed embeddings plugin for pg_synapse.
//!
//! Bundles a Hugging Face tokenizer with an ONNX Runtime [`Session`] and
//! exposes the pair as an [`EmbeddingProvider`]. Out of the box this targets
//! BGE-small-en-v1.5 (384-dim) and BGE-base-en-v1.5 (768-dim) from BAAI but
//! works with any compatible BERT-architecture model (MiniLM, E5 family) whose
//! `tokenizer.json` + `model.onnx` are point at via the profile `params`.
//!
//! ## Native dependency
//!
//! This crate pins `ort = 2.0.0-rc.12`, which targets **ONNX Runtime 1.24.x**.
//! We use the `load-dynamic` feature so `libonnxruntime.so` (or the platform
//! equivalent) must be discoverable at runtime, and its `ORT_API_VERSION` must
//! be 24. Set `ORT_DYLIB_PATH` to override the default lookup:
//!
//! ```sh
//! # Hugging Face / pip ships a self-contained 1.24.x build:
//! export ORT_DYLIB_PATH="$(python3 -c 'import onnxruntime, os; print(os.path.join(os.path.dirname(onnxruntime.__file__), \"capi\", \"libonnxruntime.so.1.24.4\"))')"
//!
//! # Or download the release tarball from microsoft/onnxruntime:
//! #   https://github.com/microsoft/onnxruntime/releases/tag/v1.24.0
//! ```
//!
//! Distro `libonnxruntime-dev` packages may ship an older version that this
//! `ort` build will reject with a "GetVersionString" mismatch. In that case
//! either pin to a matching tarball or install via `pip install onnxruntime`.
//!
//! ## Profile keys
//!
//! | Key                          | Required | Meaning                                                        |
//! |------------------------------|----------|----------------------------------------------------------------|
//! | `params.model_path`          | yes      | Filesystem path to the `.onnx` file.                           |
//! | `params.tokenizer_path`      | yes      | Filesystem path to `tokenizer.json`.                           |
//! | `params.max_seq_len`         | no       | Sequence-length cap (default `512`).                           |
//! | `params.intra_threads`       | no       | ONNX Runtime intra-op thread count (default: ORT default).     |
//!
//! With the `download` feature the [`download`] module can pre-populate
//! `~/.cache/pg-synapse/models/<repo>/` for known BGE models.
//!
//! [`Session`]: ort::session::Session

#![forbid(unsafe_code)]
#![warn(missing_docs)]

#[cfg(feature = "download")]
pub mod download;
pub mod factory;
pub mod provider;

pub use factory::OrtEmbeddingFactory;
pub use provider::OrtEmbeddingProvider;
