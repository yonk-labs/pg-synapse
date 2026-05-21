# Provider Conformance Suite (PS-5)

A deterministic, hermetic test harness that proves every `LlmProvider` impl
satisfies the same contract: declares an honest model name, declares honest
capabilities (PS-1), and reproduces recorded outcomes faithfully on replay.

If your plugin advertises a capability, the suite holds you to it. If your
plugin's serde shape drifts, the golden fixture catches it before the live
endpoint does.

## What the suite proves

Three layered tests per provider, each catching a different failure mode:

1. **Static conformance** (`*_static_conformance`). Always runs. Builds the
   real provider with a placeholder model name and a zero-entry cassette,
   then asserts `run_conformance` agrees on identity and capabilities. If
   you ship a provider whose `capabilities()` impl drifts away from your
   test's mirror, this test fails with `CapabilityMismatch`.
2. **Golden cassette replay** (`*_golden_cassette_replays`,
   `*_golden_cassette_matches_canonical`). Always runs. Loads a committed
   `tests/fixtures/conformance-default.json`, replays it through
   `CassetteProvider`, and pins it byte-for-byte against the canonical
   `default_conformance_cassette` Rust helper. If the serde shape of
   `Cassette`, `CompletionRequest`, `CompletionResponse`, `ToolCall`, or
   `LlmError` changes, the drift check fails with a full diff and a
   pointer to the regenerator.
3. **Live record + replay** (`*_live_record_then_replay`). Feature-gated
   behind `live-tests`, skips without the provider-specific env var.
   Wraps the real provider in `RecordingProvider`, drives one live
   `complete`, then replays the recorded cassette through
   `run_conformance` to prove record/replay fidelity against a real
   endpoint.

The three layers are uncoupled: each can fail without the others, and each
maps to a different cause of contract breakage.

## The pieces

All under `pg_synapse_core::testing`:

| Type | Role |
| --- | --- |
| `Cassette` | Ordered record of provider interactions plus declared identity + capabilities. |
| `CassetteEntry` | One request/outcome pair. |
| `CassetteOutcome` | `Ok(CompletionResponse)` or `Err(LlmError)`. |
| `CassetteProvider` | Replays a `Cassette` as an `LlmProvider`. No network. |
| `RecordingProvider` | Wraps a real `LlmProvider`, captures every `complete` call. |
| `run_conformance` | Drives a provider through a cassette's requests and asserts outcomes match. |
| `ConformanceError` | `ModelMismatch` / `CapabilityMismatch` / `OutcomeMismatch`. |
| `default_conformance_cassette` | Canonical three-entry cassette: text reply, tool call, auth error. |

## The canonical cassette

`default_conformance_cassette(model, capabilities)` is the single source of
truth for every provider's golden fixture. It returns a `Cassette` with
three entries, each pinning a distinct outcome shape:

1. `Ok` with `content = Some("pong")` and empty `tool_calls`.
2. `Ok` with `content = None` and one `ToolCall { name: "echo", ... }`.
3. `Err(LlmError::Auth("conformance-provider"))`.

Provider plugins parameterize only `model` and `capabilities`; everything
else is fixed across the three wired providers so divergence is a
serde-shape change in the shared types, not noise.

## Adding your provider to the suite

Follow the recipe verbatim. The three wired plugins
(`plugins/pg-synapse-provider-openai`, `-llama-cpp`, `-anthropic`) are the
canonical examples; mirror their `tests/conformance.rs` and
`tests/fixtures/conformance-default.json`.

### 1. Write the conformance test file

Create `tests/conformance.rs` in your plugin crate:

```rust
use pg_synapse_core::llm::ProviderCapabilities;
use pg_synapse_core::testing::{
    Cassette, CassetteProvider, default_conformance_cassette, run_conformance,
};
use my_provider_crate::MyProvider;

fn fixture_path() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/conformance-default.json")
}

/// Mirrors `impl LlmProvider::capabilities` exactly. If your impl
/// changes, change this too; the static test below catches drift.
fn my_caps() -> ProviderCapabilities {
    ProviderCapabilities {
        tool_use: true,
        // ...
        ..Default::default()
    }
}

#[tokio::test]
async fn my_static_conformance() {
    let provider = MyProvider::new("conformance-model", "http://unused.invalid");
    let expected = Cassette {
        model: "conformance-model".into(),
        capabilities: my_caps(),
        entries: vec![],
    };
    run_conformance(&provider, &expected).await.unwrap();
}

#[tokio::test]
async fn my_golden_cassette_replays() {
    let path = fixture_path();
    let cassette = Cassette::load(&path).unwrap();
    let replay = CassetteProvider::new(cassette);
    let expected = Cassette::load(&path).unwrap();
    run_conformance(&replay, &expected).await.unwrap();
}

#[test]
fn my_golden_cassette_matches_canonical() {
    let canonical = default_conformance_cassette("conformance-model", my_caps()).to_json();
    let committed = std::fs::read_to_string(fixture_path()).unwrap();
    assert_eq!(canonical, committed.trim_end_matches('\n'));
}

#[test]
#[ignore]
fn regenerate_my_golden_cassette() {
    let canonical = default_conformance_cassette("conformance-model", my_caps()).to_json();
    let mut bytes = canonical.into_bytes();
    bytes.push(b'\n');
    std::fs::write(fixture_path(), bytes).unwrap();
}
```

### 2. Generate the fixture

Run the regenerator once to produce the committed JSON, then commit it:

```text
cargo test -p my-provider-crate --test conformance \
    regenerate_my_golden_cassette -- --ignored
git add tests/fixtures/conformance-default.json
```

### 3. (Optional) Add the live record/replay test

If your provider has a live endpoint you can hit in CI behind a feature
flag, follow the slice-2b/2d pattern in any wired plugin. The shape:
feature-gate the test with `#[cfg(feature = "live-tests")]`, skip when the
required env var is unset, wrap the real provider in `RecordingProvider`,
then replay through `run_conformance`.

## When fixtures drift

The drift check fails like this:

```text
assertion `left == right` failed: fixture drift: run
`cargo test -p my-provider-crate --test conformance
 regenerate_my_golden_cassette -- --ignored` to refresh
```

Causes, ranked by likelihood:

* You hand-edited the fixture JSON. Don't. Re-run the regenerator.
* The kernel's `default_conformance_cassette` changed (a new entry, a
  reshape). Re-run the regenerator; review the resulting fixture diff to
  confirm the shape change is what you expected.
* A serde-tagged type changed (`CompletionResponse`, `LlmError`,
  `ToolCall`, ...). Re-run the regenerator; the fixture diff will show
  the new shape and you can decide whether it should be reviewed as a
  breaking change.

## What the suite does not prove

* **Live correctness.** The suite proves your provider correctly replays
  recorded interactions. It does not prove a real backend would return
  those interactions if given the canonical requests. That is the
  `*_live_record_then_replay` test's job.
* **Streaming.** Per kernel decision D8, streaming is deferred in v0.1.
  When `stream()` graduates, the suite will gain a streaming-conformance
  layer.
* **Embeddings.** PS-5 covers `LlmProvider` only. `EmbeddingProvider`
  conformance is a separate effort.

## See also

* `crates/pg-synapse-core/src/testing/cassette.rs` for the module-level
  rustdoc.
* `docs/plugin-development.md` for the broader plugin authoring guide.
