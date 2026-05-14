//! Auto-download tokenizer + ONNX model from Hugging Face.
//!
//! Cache layout:
//!
//! ```text
//! $XDG_CACHE_HOME/pg-synapse/models/<repo>/
//!     tokenizer.json
//!     model.onnx
//! ```
//!
//! `<repo>` keeps the upstream slash (e.g. `BAAI/bge-small-en-v1.5`), so
//! `BAAI` becomes a subdirectory. Files are downloaded once and reused across
//! processes; partial files are not resumed (we redownload on missing).
//!
//! Only available with the `download` feature.

use std::io::Write;
use std::path::PathBuf;

use reqwest::blocking::Client;

use pg_synapse_core::error::EmbeddingError;

/// Description of a Hugging Face repository hosting an ONNX embedding model.
#[derive(Debug, Clone)]
pub struct DownloadConfig {
    /// Hugging Face repo id, e.g. `"BAAI/bge-small-en-v1.5"`.
    pub repo: String,
    /// Git revision (branch, tag, or commit). Defaults to `"main"`.
    pub revision: String,
    /// Tokenizer file path inside the repo. Defaults to `"tokenizer.json"`.
    pub tokenizer: String,
    /// Model file path inside the repo. Defaults to `"onnx/model.onnx"`.
    pub model: String,
}

impl DownloadConfig {
    /// BGE-small-en-v1.5 (384-dim).
    pub fn bge_small_en_v15() -> Self {
        Self {
            repo: "BAAI/bge-small-en-v1.5".into(),
            revision: "main".into(),
            tokenizer: "tokenizer.json".into(),
            model: "onnx/model.onnx".into(),
        }
    }

    /// BGE-base-en-v1.5 (768-dim).
    pub fn bge_base_en_v15() -> Self {
        Self {
            repo: "BAAI/bge-base-en-v1.5".into(),
            revision: "main".into(),
            tokenizer: "tokenizer.json".into(),
            model: "onnx/model.onnx".into(),
        }
    }
}

/// Cache root: `$XDG_CACHE_HOME/pg-synapse/models` (or `/tmp/pg-synapse/models`
/// if no user cache dir is discoverable).
pub fn cache_root() -> PathBuf {
    dirs::cache_dir()
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join("pg-synapse")
        .join("models")
}

/// Make sure `tokenizer.json` and `model.onnx` exist on disk for `cfg`,
/// downloading them from `https://huggingface.co/<repo>/resolve/<revision>/`
/// if they are absent. Returns `(model_path, tokenizer_path)`.
pub fn ensure_files(cfg: &DownloadConfig) -> Result<(PathBuf, PathBuf), EmbeddingError> {
    let dir = cache_root().join(&cfg.repo);
    std::fs::create_dir_all(&dir)
        .map_err(|e| EmbeddingError::Provider(format!("create cache dir: {e}")))?;

    let model_dest = dir.join("model.onnx");
    let tokenizer_dest = dir.join("tokenizer.json");

    let base = format!(
        "https://huggingface.co/{}/resolve/{}",
        cfg.repo, cfg.revision
    );

    if !model_dest.exists() {
        download_file(&format!("{base}/{}", cfg.model), &model_dest)?;
    }
    if !tokenizer_dest.exists() {
        download_file(&format!("{base}/{}", cfg.tokenizer), &tokenizer_dest)?;
    }
    Ok((model_dest, tokenizer_dest))
}

fn download_file(url: &str, dest: &std::path::Path) -> Result<(), EmbeddingError> {
    tracing::info!(%url, dest = %dest.display(), "pg-synapse-embeddings-ort: downloading");
    let resp = Client::new()
        .get(url)
        .send()
        .map_err(|e| EmbeddingError::Network(e.to_string()))?
        .error_for_status()
        .map_err(|e| EmbeddingError::Network(format!("HTTP {url}: {e}")))?;
    let bytes = resp
        .bytes()
        .map_err(|e| EmbeddingError::Network(e.to_string()))?;
    let mut file = std::fs::File::create(dest)
        .map_err(|e| EmbeddingError::Provider(format!("create {}: {e}", dest.display())))?;
    file.write_all(&bytes)
        .map_err(|e| EmbeddingError::Provider(format!("write {}: {e}", dest.display())))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_root_under_pg_synapse_models() {
        let p = cache_root();
        let s = p.display().to_string();
        assert!(s.ends_with("pg-synapse/models"), "got: {s}");
    }

    #[test]
    fn bge_small_config_points_at_v15_repo() {
        let cfg = DownloadConfig::bge_small_en_v15();
        assert_eq!(cfg.repo, "BAAI/bge-small-en-v1.5");
        assert_eq!(cfg.tokenizer, "tokenizer.json");
        assert!(cfg.model.ends_with("model.onnx"));
    }

    #[test]
    fn bge_base_config_points_at_v15_repo() {
        let cfg = DownloadConfig::bge_base_en_v15();
        assert_eq!(cfg.repo, "BAAI/bge-base-en-v1.5");
    }
}
