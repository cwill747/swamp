# Zellij 0.45.0 pane fixture

`zellij-0.45-list-panes-all.json` records the fields returned by
`zellij --session <isolated-session> action list-panes --all --json`. The live
probe used a uniquely named `swamp-pane-fixture-*` session with two shell panes
and an automatically loaded plugin pane. It confirmed that the plugin and a
terminal can both have numeric ID `0`; focus IDs therefore need the
`plugin_0`/`terminal_0` namespace. `tab_id`, `tab_position`, `tab_name`,
`terminal_command`, and the `is_plugin` flag were present on Zellij 0.45.0.

The isolated session was attached from two clients simultaneously; `list-clients`
reported both clients. Renaming the agent pane changed its `title` but retained
its terminal command. The fixture includes that renamed-title shape and a
second tab's agent to cover tab scoping. Swamp does not infer roles from a
harness process command: a renamed or ambiguous pane uses tab-only fallback.

`focus-pane-id terminal_1` succeeded in the isolated session. The external
session-targeted command does not identify a particular attached client; Swamp
therefore uses the local `zellij action` after switching the invoking client,
and keeps tab-only fallback if pane focus cannot be confirmed.

Fresh generated-dashboard captures were then made in separate isolated 0.45.0
sessions at 120x36 and 160x48. Their `list-panes --all --json` records and pane
buffers are in `tests/fixtures/workspace/zellij-0.45-captures/`; the composed
terminal view is checked in beside the workspace fixtures. The 160x48 capture
includes the raw `script(1)` ANSI recording, which renders to a single visible
bottom `compact-bar` strip. These sessions used the fixture runtime and config
under `/tmp/swamp-herdr-final`, never a user session. The stock layout uses the
explicit `zellij:compact-bar` URI because the short alias was blank when
`config.kdl` did not define aliases.
