# Publishing checklist (operator-run)

This is a manual checklist. The CI does not publish. Run these steps when you
decide to release to crates.io. Order matters: a crate cannot be published
until every crate it depends on is already on crates.io.

## Pre-flight

- [ ] `cargo fmt --all -- --check` clean
- [ ] `cargo clippy --workspace --all-targets -- -D warnings` clean
- [ ] `cargo test --workspace` green (excluding `--features live-tests`)
- [ ] `cargo pgrx test --package pg-synapse-pgrx pg17` green (pg15, pg16 too if available)
- [ ] `CHANGELOG.md` has the release entry with today's date
- [ ] Workspace `version` in `Cargo.toml` is the release version
- [ ] `git status` clean, on `main`
- [ ] You are logged in: `cargo login <token>`

## Publish order

Each path dependency carries a `version = "0.1.0"` field, so once an upstream
crate is on crates.io the downstream `cargo package` verification passes. Until
then, downstream `cargo package` fails locally; that is expected, not a defect.

1. [ ] `cargo publish -p pg-synapse-macros`
       then verify: `cargo info pg-synapse-macros` shows the new version
2. [ ] `cargo publish -p pg-synapse-core`
       then verify: `cargo info pg-synapse-core`
3. [ ] `cargo publish -p pg-synapse-provider-openai`
4. [ ] `cargo publish -p pg-synapse-embeddings-ort`
5. [ ] `cargo publish -p pg-synapse-tools-http`
6. [ ] `cargo publish -p pg-synapse-tools-sql`
7. [ ] `cargo publish -p pg-synapse-pgrx`
       Note: pgrx extension crates publish the source. Consumers build the
       extension with `cargo pgrx`. The crate still publishes fine.

After each step, wait for the crates.io index to update (usually under a
minute) before publishing the next crate, or the dependency resolution for
the downstream crate will fail.

## Deferred crates (not in this release)

- `pg-synapse-provider-anthropic`, `pg-synapse-provider-llama-cpp`,
  `pg-synapse-sidecar`: planned for v0.1.x. Add them to this checklist when
  they ship.

## Tagging

```
git tag -s v0.1.0 -m "pg_synapse v0.1.0"
git push origin v0.1.0      # only when the operator is ready for a public remote
```

The v0.1.0 tag is created locally during release prep. Pushing to a remote
is a separate, explicit operator action.

## Post-publish smoke test

In a scratch directory outside the workspace:

```
cargo new pgsynapse-smoke && cd pgsynapse-smoke
cargo add pg-synapse-core
cargo build
```

It should resolve `pg-synapse-core` from crates.io and compile.
