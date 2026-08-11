# swamp

## Building

Always run project toolchain commands through the flake dev shell using
`nix develop --command ...`.

Always use `nix build` to verify changes, not `cargo build` or
`cargo check`.

The default output is the fast local/PR build. It uses cargo's `dev` profile
(opt-level 0, no LTO, parallel codegen) instead of the heavy `[profile.release]`
the shipped binary uses, so it compiles much faster:

```
nix build
```

`#dev` remains an alias for the fast build. Use the fast build for local
verification; main-branch CI and release workflows build `#release`.

## Linting

Formatting and Clippy are enforced in CI (`.github/workflows/lint.yml`) and must
be clean:

```
nix develop --command cargo fmt --all --check
nix develop --command cargo clippy --all-targets --all-features -- -D warnings
```

Enable the local pre-commit hook (runs `cargo fmt --check` before each commit):

```
git config core.hooksPath .githooks
```
