# swamp

## Building

Always use `nix build` to verify changes, not `cargo build` or `cargo check`.

In a bare-repo worktree setup, Nix can't resolve the flake via the worktree's
relative `.git` file. Always build with `nix build path:.` to point Nix at the
current directory explicitly.

```
nix build path:.
```

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
