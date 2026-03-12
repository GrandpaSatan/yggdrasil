# Contributing to Yggdrasil

Thank you for your interest in contributing to Yggdrasil.

## Getting Started

1. Fork the repository
2. Create a feature branch from `main`
3. Copy `configs/*/config.example.json` to `configs/*/config.json` and fill in your local values
4. Run `cargo check` to verify your setup compiles

## Development

### Build

```bash
cargo build           # debug build
cargo build --release # release build (required for ONNX/ort crate)
```

### Test

```bash
cargo test            # run all tests
cargo test -p mimir   # test a specific crate
```

### Lint

```bash
cargo clippy --workspace -- -D warnings
```

### Important Notes

- **Rust Edition 2024** -- `gen` is a reserved keyword. Use `rng.r#gen::<T>()` if needed.
- **ort pinning** -- The `ort` crate must be pinned to `=2.0.0-rc.12`. Do not change this version.
- **sqlx runtime binding** -- We use `sqlx::query()` with runtime parameter binding, NOT the `query!` macro.
- **No glibc debug builds** -- If your machine has glibc < 2.42, use `cargo check --release` instead of `cargo check` for crates depending on `ort`.

## Pull Requests

- Keep PRs focused on a single change
- Include tests for new functionality
- Run `cargo clippy` and `cargo test` before submitting
- Update documentation if your change affects APIs or configuration

## Code Style

- Follow existing patterns in the codebase
- Use `thiserror` for error types
- Use `tracing` for logging (not `println!` or `log`)
- All async code uses `tokio`
- Database access goes through `ygg-store`, not direct `sqlx` calls from service crates

## Reporting Issues

Open an issue on GitHub with:
- What you expected to happen
- What actually happened
- Steps to reproduce
- Your Rust version (`rustc --version`) and OS

## License

By contributing, you agree that your contributions will be licensed under the same [BSL 1.1](LICENSE) license as the project.
