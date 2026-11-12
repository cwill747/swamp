# swamp

## Building

Always run project toolchain commands through the flake dev shell using
`nix develop --command ...`.

Always use `nix build` to verify changes, not `cargo build` or
`cargo check`.

The default output is the optimized release build with shell completions:

```
nix build
```

For fast local/PR verification, use `nix build .#dev`. It uses cargo's `dev`
profile (opt-level 0, no LTO, parallel codegen) instead of the heavy
`[profile.release]` the shipped binary uses. Main-branch CI and release
workflows build `#release`, which selects the same package as the default.

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
