# Oxiclean Monorepo - Just Commands

# Default recipe to display help
default:
    @just --list

# Build all workspace members
build:
    cargo build

# Quick check compilation without building
check:
    cargo check --workspace --all-targets

# Run all tests
test:
    cargo test --workspace

# Run tests with output
test-verbose:
    cargo test --workspace -- --nocapture

# Run tests for a specific package
test-package package:
    cargo test -p {{package}}

# Clean build artifacts
clean:
    cargo clean

# Format code
fmt:
    cargo fmt --all

# Check code formatting
fmt-check:
    cargo fmt --all -- --check

# Run clippy (allow warnings)
lint:
    cargo clippy --workspace --all-targets --all-features

# Run clippy with strict mode (deny warnings)
lint-strict:
    cargo clippy --workspace --all-targets --all-features -- -D warnings

# Fix clippy warnings automatically where possible
lint-fix:
    cargo clippy --workspace --all-targets --all-features --fix

# Run the main oxiclean application
run-oxiclean *ARGS:
    cargo run -p oxiclean -- {{ARGS}}

# Build release binaries
release:
    cargo build --release --workspace

# Build release binaries (optimized)
release-optimized:
    RUSTFLAGS="-C target-cpu=native" cargo build --release --workspace

# Install oxiclean CLI to cargo bin
install:
    cargo install --path apps/oxiclean

# Run all quality checks (allows clippy warnings)
ci: fmt-check lint test
    @echo "✓ All checks passed!"

# Run all quality checks with strict linting
ci-strict: fmt-check lint-strict test
    @echo "✓ All strict checks passed!"

# Update dependencies
update:
    cargo update

# Check for outdated dependencies (requires cargo-outdated)
outdated:
    cargo outdated

# Run security audit (requires cargo-audit)
audit:
    cargo audit

# Generate documentation
docs:
    cargo doc --workspace --no-deps --open

# Generate documentation for all dependencies
docs-all:
    cargo doc --workspace --open

# Run benchmarks (if any)
bench:
    cargo bench --workspace

# Watch for changes and run tests (requires cargo-watch)
watch:
    cargo watch -x test

# Watch for changes and run check (requires cargo-watch)
watch-check:
    cargo watch -x check

# Show workspace tree
tree:
    cargo tree --workspace

# Show duplicate dependencies
tree-duplicates:
    cargo tree --workspace --duplicates

# Expand macros for debugging
expand package file:
    cargo expand -p {{package}} {{file}}

# Run cargo bloat to analyze binary size (requires cargo-bloat)
bloat binary:
    cargo bloat --release --bin {{binary}}

# Profile build time (requires cargo-timings)
timings:
    cargo build --release --workspace --timings

# Check for unused dependencies (requires cargo-udeps, nightly toolchain)
udeps:
    cargo +nightly udeps --workspace

# Initialize or update git hooks
init-hooks:
    #!/usr/bin/env bash
    mkdir -p .git/hooks
    cat > .git/hooks/pre-commit << 'EOF'
    #!/bin/sh
    just fmt-check && just lint
    EOF
    chmod +x .git/hooks/pre-commit
    echo "✓ Git hooks installed"

# Run all checks before committing
pre-commit: fmt-check lint test
    @echo "✓ Ready to commit!"

# Full clean including cargo cache for this project
clean-all: clean
    rm -rf ~/.cargo/registry/index/*
    rm -rf ~/.cargo/git/db/*
