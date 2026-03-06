# Contributing to NgenOrca

Thank you for your interest in contributing! This guide covers the development
workflow, coding standards, and submission process.

## Getting Started

### Prerequisites

- **Rust 1.85+** (edition 2024) — install via [rustup](https://rustup.rs/)
- **SQLite 3** headers (or use the `bundled` feature, which is the default)
- **Docker** (optional, for container builds)

### Clone and Build

```bash
git clone https://github.com/ngenorca/ngenorca.git
cd ngenorca
cargo build --workspace
```

### Run Tests

```bash
cargo test --workspace
```

All tests must pass before submitting a PR. The CI pipeline runs the full suite.

### Project Structure

```
crates/
  ngenorca-core/        # Shared types, error types, traits
  ngenorca-bus/         # Event bus with SQLite-backed log
  ngenorca-config/      # TOML configuration and validation
  ngenorca-identity/    # Hardware fingerprinting, owner pairing
  ngenorca-memory/      # Three-tier memory (working/episodic/semantic)
  ngenorca-plugin-sdk/  # Plugin + ChannelAdapter traits, LLM types
  ngenorca-sandbox/     # Process sandboxing and execution
  ngenorca-gateway/     # HTTP/WS server, providers, orchestration
  ngenorca-cli/         # CLI binary (pair, status, run)
docs/                   # Additional documentation
```

## Development Workflow

1. **Fork** the repository and create a feature branch from `main`.
2. **Write code** following the style guidelines below.
3. **Add tests** for any new functionality — aim for the pattern used in
   existing modules (unit tests in `#[cfg(test)] mod tests`).
4. **Run `cargo clippy --workspace`** and fix any warnings.
5. **Run `cargo fmt --all`** to ensure consistent formatting.
6. **Run `cargo test --workspace`** to verify nothing is broken.
7. **Commit** with a clear, descriptive message (see below).
8. **Open a Pull Request** against `main`.

## Coding Standards

### Rust Style

- Follow standard `rustfmt` defaults (run `cargo fmt`).
- Use `clippy` at the default lint level — no `#[allow]` without justification.
- Prefer `thiserror` derive macros for error types.
- Use `async_trait` for async trait methods.
- Document public items with `///` doc comments.
- Keep functions focused — if a function exceeds ~80 lines, consider splitting.

### Naming Conventions

| Item | Convention | Example |
|------|-----------|---------|
| Crates | `ngenorca-*` kebab-case | `ngenorca-memory` |
| Structs | PascalCase | `SessionManager` |
| Traits | PascalCase | `ChannelAdapter` |
| Functions | snake_case | `build_context()` |
| Constants | SCREAMING_SNAKE | `API_VERSION` |
| Modules | snake_case | `event_log` |

### Testing

- Every public function or method should have at least one test.
- Use `#[cfg(test)] mod tests { ... }` within the same file.
- Async tests use `#[tokio::test]`.
- Tests should be deterministic — avoid relying on timing or external services.
- Use unique temp directories for SQLite-backed tests (see `state.rs` pattern).

### Commit Messages

Use [Conventional Commits](https://www.conventionalcommits.org/):

```
feat(memory): add episodic search with FTS5 scoring
fix(gateway): prevent SQLite lock contention in parallel tests
docs: add Docker quick-start guide
test(identity): add Ed25519 signature verification tests
refactor(bus): extract event log pruning into background task
```

Scope should match the crate name without the `ngenorca-` prefix when applicable.

## Adding a Channel Adapter

1. Create `crates/ngenorca-gateway/src/channels/<name>.rs`.
2. Define a struct with the channel's config fields.
3. Implement `Plugin` (manifest, init, handle_message, health_check, shutdown).
4. Implement `ChannelAdapter` (start_listening, send_message, channel_kind).
5. Add 3 standard tests: manifest, health_check, channel_kind.
6. Add `pub mod <name>;` in `channels/mod.rs`.
7. Wire registration in `register_adapters()`.

See `channels/discord.rs` or `channels/webchat.rs` for reference.

## Adding an LLM Provider

1. Create a module in `crates/ngenorca-gateway/src/providers/`.
2. Implement the `ModelProvider` trait (`send`, `models`, `name`).
3. Add error mapping via `map_provider_http_error()`.
4. Register in `ProviderRegistry::from_config()`.
5. Add tests for model prefix stripping, message conversion, and provider name.

See `providers/ollama.rs` for reference.

## Pull Request Checklist

- [ ] All tests pass (`cargo test --workspace`)
- [ ] No clippy warnings (`cargo clippy --workspace`)
- [ ] Code is formatted (`cargo fmt --all --check`)
- [ ] New public items have doc comments
- [ ] CHANGELOG.md updated under `[Unreleased]`
- [ ] No secrets or credentials in committed code

## Code of Conduct

Be respectful and constructive. We follow the
[Rust Code of Conduct](https://www.rust-lang.org/policies/code-of-conduct).

## License

By contributing, you agree that your contributions will be licensed under the
MIT License as specified in the repository.
