# swamp

## Building

Always use `nix build` to verify changes, not `cargo build` or `cargo check`.

In a bare-repo worktree setup, Nix can't resolve the flake via the worktree's
relative `.git` file. Always build with `nix build path:.` to point Nix at the
current directory explicitly.

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
nix develop --command cargo fmt --all --check
nix develop --command cargo clippy --all-targets --all-features -- -D warnings
```

Enable the local pre-commit hook (runs `cargo fmt --check` before each commit):

```
git config core.hooksPath .githooks
```
