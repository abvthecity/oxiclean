# Contributing

Everything is plain `cargo`. You need Rust 1.89 or newer.

## Build and run

```bash
cargo build
cargo run -- --help
cargo run -- import-bloat --root ../some-ts-project
```

`cargo run` builds the `oxiclean` CLI from source. Add `--release` when timing
it against a large project.

## Before opening a PR

CI runs these, so run them locally first:

```bash
cargo fmt --all
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace
```

## Benchmarks

`scripts/bench.sh` runs every checker against pinned Next.js and Expo
checkouts and compares the results with a recorded baseline. See
[bench/README.md](bench/README.md).
