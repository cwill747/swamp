## 1. Launch behavior

- [x] 1.1 Use the foreground attach path for existing repo sessions in all terminal contexts.
- [x] 1.2 Use the foreground layout launch path for new repo sessions in all terminal contexts.
- [x] 1.3 Remove the obsolete client-switching and originating-tab cleanup helpers and tests.

## 2. Documentation

- [x] 2.1 Document Zellij 0.45 or later as a requirement and describe native nested-session behavior.

## 3. Verification

- [x] 3.1 Run Rust formatting and Clippy checks through the Nix development shell.
- [x] 3.2 Run the fast local build with `nix build`.
- [x] 3.3 Manually verify Zellij's nested-session UI from an outer Zellij 0.45 session.
