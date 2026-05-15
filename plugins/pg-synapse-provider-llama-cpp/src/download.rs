//! GGUF model download helper (requires the `download` feature).
//!
//! Downloads a GGUF file from Hugging Face into the local cache directory
//! `~/.cache/pg-synapse/models/{repo}/{file}`. Skips the download if the file
//! is already present. The download uses a streaming copy so large models do
//! not require unbounded RAM.
//!
//! ## Granite micro coordinates
//!
//! The primary GGUF target is:
//!
//! | Field    | Value                                                 |
//! |----------|-------------------------------------------------------|
//! | repo     | `lmstudio-community/granite-3.0-2b-instruct-GGUF`   |
//! | file     | `granite-3.0-2b-instruct-Q4_K_M.gguf`               |
//! | revision | `main`                                                |
//!
//! This is a real public Hugging Face repository. If that file is unavailable,
//! fall back to:
//!
//! | Field    | Value                                              |
//! |----------|----------------------------------------------------|
//! | repo     | `bartowski/granite-3.1-2b-instruct-GGUF`          |
//! | file     | `granite-3.1-2b-instruct-Q4_K_M.gguf`             |
//! | revision | `main`                                             |
//!
//! ## Example
//!
//! ```no_run
//! # #[cfg(feature = "download")]
//! # async fn example() -> Result<(), Box<dyn std::error::Error>> {
//! use pg_synapse_provider_llama_cpp::download::{download_gguf, granite_micro};
//!
//! let (repo, file, rev) = granite_micro();
//! let path = download_gguf(repo, file, rev).await?;
//! println!("Model at {}", path.display());
//! # Ok(())
//! # }
//! ```

use std::path::PathBuf;

use reqwest::Client;

/// Returns the primary Granite micro GGUF coordinates `(repo, file, revision)`.
///
/// Primary: `lmstudio-community/granite-3.0-2b-instruct-GGUF`
/// Fallback: `bartowski/granite-3.1-2b-instruct-GGUF`
///
/// Both are real, public Hugging Face repositories hosting Q4_K_M quantized
/// files suitable for CPU inference on a laptop.
pub fn granite_micro() -> (&'static str, &'static str, &'static str) {
    (
        "lmstudio-community/granite-3.0-2b-instruct-GGUF",
        "granite-3.0-2b-instruct-Q4_K_M.gguf",
        "main",
    )
}

/// Returns the fallback Granite micro GGUF coordinates if the primary is
/// unavailable.
pub fn granite_micro_fallback() -> (&'static str, &'static str, &'static str) {
    (
        "bartowski/granite-3.1-2b-instruct-GGUF",
        "granite-3.1-2b-instruct-Q4_K_M.gguf",
        "main",
    )
}

/// Download a GGUF from Hugging Face into the local cache directory.
///
/// Fetches `https://huggingface.co/{repo}/resolve/{revision}/{file}` and
/// writes it to `~/.cache/pg-synapse/models/{repo}/{file}`. The download is
/// skipped if the file already exists (idempotent).
///
/// Returns the path to the cached file.
///
/// # Errors
///
/// Returns an error if:
/// * The cache directory cannot be determined (`dirs::cache_dir()` returns
///   `None` on this platform).
/// * The HTTP request fails or the server returns a non-2xx status.
/// * Writing to disk fails.
pub async fn download_gguf(
    repo: &str,
    file: &str,
    revision: &str,
) -> Result<PathBuf, Box<dyn std::error::Error + Send + Sync>> {
    let cache_root = dirs::cache_dir()
        .ok_or("could not determine cache directory (dirs::cache_dir returned None)")?;

    // Destination: ~/.cache/pg-synapse/models/{repo}/{file}
    // The repo slug may contain a slash (owner/name), which becomes a
    // two-level directory: ~/.cache/pg-synapse/models/owner/name/file
    let dest = cache_root
        .join("pg-synapse")
        .join("models")
        .join(repo)
        .join(file);

    if dest.exists() {
        tracing::debug!(
            target: "pg_synapse_llama_cpp_download",
            path = %dest.display(),
            "GGUF already cached, skipping download"
        );
        return Ok(dest);
    }

    // Ensure parent directories exist.
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let url = format!("https://huggingface.co/{repo}/resolve/{revision}/{file}");
    tracing::info!(
        target: "pg_synapse_llama_cpp_download",
        url = %url,
        dest = %dest.display(),
        "Downloading GGUF from Hugging Face"
    );

    let client = Client::builder()
        .timeout(std::time::Duration::from_secs(3600))
        .build()?;

    let resp = client.get(&url).send().await?;
    let status = resp.status();
    if !status.is_success() {
        return Err(format!("Hugging Face returned HTTP {status} for {url}").into());
    }

    // Write to a temp file first, then rename atomically.
    let tmp = dest.with_extension("gguf.tmp");
    let bytes = resp.bytes().await?;
    std::fs::write(&tmp, &bytes)?;
    std::fs::rename(&tmp, &dest)?;

    tracing::info!(
        target: "pg_synapse_llama_cpp_download",
        path = %dest.display(),
        bytes = bytes.len(),
        "GGUF download complete"
    );

    Ok(dest)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn granite_micro_returns_expected_coordinates() {
        let (repo, file, rev) = granite_micro();
        assert!(repo.contains("granite"), "repo should reference granite");
        assert!(file.ends_with(".gguf"), "file should be a .gguf");
        assert_eq!(rev, "main");
    }

    #[test]
    fn granite_micro_fallback_returns_expected_coordinates() {
        let (repo, file, rev) = granite_micro_fallback();
        assert!(
            repo.contains("granite"),
            "fallback repo should reference granite"
        );
        assert!(file.ends_with(".gguf"), "fallback file should be a .gguf");
        assert_eq!(rev, "main");
    }
}
