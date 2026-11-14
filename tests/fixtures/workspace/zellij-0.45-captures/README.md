# Isolated Zellij captures

Captured from generated dashboard sessions on Zellij 0.45.0 at 120x36 and
160x48. The sessions ran with a private `/tmp/swamp-herdr-final` runtime,
config, registry, and two fixture repositories. The sidebar received reported
Waiting, Working, and subagent fixture records; agent panes stayed suspended.
No user sessions or repositories were used.

`dashboard-{120,160}-panes.json` are `list-panes --all --json` outputs; the
matching sidebar, details, and shell files are pane `dump-screen` output. The
ANSI recordings retain the whole terminal frame, including the actual stock
compact-bar strip. Render the fixed viewport with:

```sh
python3 tests/fixtures/workspace/zellij-0.45-captures/render_tty_capture.py \
  tests/fixtures/workspace/zellij-0.45-captures/capture120-tty.txt 36 120 \
  tests/fixtures/workspace/zellij-0.45-composed-120x36.txt
```

Use `capture160-tty.txt 48 160` for the other size. The renderer applies the
recorded cursor movements and strips SGR/OSC styling, so the `.txt` frames are
plain-text captures; inspect the ANSI inputs for colors. It does not invent
terminal text or bar labels. These frames show one Zellij split divider and one
visible bottom compact-bar row.
