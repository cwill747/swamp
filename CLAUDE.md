# swamp

## Building

Always use `nix build` to verify changes, not `cargo build` or `cargo check`.

In a bare-repo worktree setup, Nix can't resolve the flake via the worktree's
relative `.git` file. Always build with `nix build path:.` to point Nix at the
current directory explicitly.

```
nix build path:.
```
