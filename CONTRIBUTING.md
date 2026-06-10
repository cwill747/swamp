# Contributing

## Development environment

This repo uses Nix for reproducible builds and development tools.

```bash
nix develop
```

The dev shell provides `cargo`, `rustc`, `rustfmt`, `clippy`,
`rust-analyzer`, `pkg-config`, and `cmake`.

## Building

Verify changes with `nix build`, not raw `cargo build`:

```bash
nix build path:.
./result/bin/swamp --help
```

The default package is a fast local/PR build. Before main-branch cache work or
release validation, build the optimized release output:

```bash
nix build path:.#release
```

Use `path:.` when building from a worktree. In a bare-repo worktree layout, each
worktree's `.git` is a relative file, not a directory, and Nix may not resolve
the flake through it without the explicit path.

## Testing

Run tests from the Nix dev shell:

```bash
nix develop
cargo test
```

Run formatting and lint checks before opening a PR:

```bash
cargo fmt --check
cargo clippy --all-targets -- -D warnings
```

## Release artifacts

Tagged releases build binaries through `.github/workflows/release.yml`.

The release workflow currently publishes:

- `swamp-x86_64-linux`
- `swamp-aarch64-linux`
- `swamp-aarch64-darwin`

Linux release artifacts are built with the flake's static packages.

## Agent status hooks

`swamp init` installs or updates the recommended Claude Code hooks in
`~/.claude/settings.json` and configures Codex `notify` in
`~/.codex/config.toml`.

The Claude hooks call:

```bash
swamp hook working
swamp hook waiting
swamp hook idle
```

They also pass Claude's session name and session id when available so swamp can
show conversation titles and resume Claude sessions after a later `swamp`
launch.

Codex emits `agent-turn-complete` events through `notify`; swamp records those
as `idle` status updates. Codex does not emit a turn-started event, so Codex
panes do not show a live `working` state.
