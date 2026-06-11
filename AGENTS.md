# swamp

## Building

Always run project toolchain commands through the flake dev shell using
`nix develop path:. --command ...`.

Always use `nix build path:.` to verify changes, not `cargo build` or
`cargo check`.

In a bare-repo worktree setup, Nix can't resolve the flake via the worktree's
relative `.git` file. Always pass `path:.` so Nix points at the current
directory explicitly.

The default output is the fast local/PR build. It uses cargo's `dev` profile
(opt-level 0, no LTO, parallel codegen) instead of the heavy `[profile.release]`
the shipped binary uses, so it compiles much faster:

```
nix build path:.
```

`#dev` remains an alias for the fast build. Use the fast build for local
verification; main-branch CI and release workflows build `#release`.

## Linting

Formatting and Clippy are enforced in CI (`.github/workflows/lint.yml`) and must
be clean:

```
nix develop path:. --command cargo fmt --all --check
nix develop path:. --command cargo clippy --all-targets --all-features -- -D warnings
```

Enable the local pre-commit hook (runs `cargo fmt --check` before each commit):

```
git config core.hooksPath .githooks
```
