# zellij-mission-control

[![CI](https://github.com/c9rgreen/zellij-mission-control/actions/workflows/ci.yml/badge.svg)](https://github.com/c9rgreen/zellij-mission-control/actions/workflows/ci.yml)

A Zellij plugin for people who run several Claude Code panes at once. It marks
each pane's tab with what Claude is doing there (working, finished, or waiting
on you), sends a terminal notification the moment one needs you, and gives you
a dashboard that lists every Claude pane, most urgent first, so you can jump
straight to it.

<!-- TODO: screenshot of the dashboard open over a few Claude panes, with markers in the tab bar -->

This is an independent project, not affiliated with Anthropic or Zellij.

## Install

Setting up takes two steps: Zellij loads the plugin, and Claude Code gets a
hook that reports to it. Nothing is downloaded by hand.

Requirements:

- Zellij 0.44 or newer
- [Claude Code](https://docs.claude.com/en/docs/claude-code)
- `jq` (used by the hook script)
- Linux or macOS

### 1. Zellij

Open `~/.config/zellij/config.kdl` and add the lines marked `+`. If your file
already has a `plugins`, `load_plugins` or `keybinds` block, add the lines
inside the block you have rather than adding a second one.

```diff
plugins {
    // ...your other plugins...
+   mission-control location="https://github.com/c9rgreen/zellij-mission-control/releases/download/v0.1.0/zellij-mission-control.wasm"
}
load_plugins {
+   mission-control
}
keybinds {
    shared_among "normal" "locked" {
+       bind "Alt a" { MessagePlugin "mission-control" { name "mission-control::toggle"; }; }
    }
}
```

Then start a new Zellij session, or load the plugin into the current one:

```sh
zellij action start-or-reload-plugin mission-control
```

Zellij downloads the plugin from that URL once and caches it. On first load it
asks you to grant five permissions. Accept them; each one is used for a single
thing:

| Permission | Used for |
| --- | --- |
| Read application state | seeing which panes and tabs exist |
| Change application state | renaming tabs and focusing a pane |
| Run commands | finding a pane's terminal with `ps` and writing the notification to it |
| Read CLI pipes | receiving events from the Claude Code hook |
| Run actions as the user | the dashboard's command palette |

### 2. Claude Code

The hook installs as a Claude Code plugin from this repository:

```sh
claude plugin marketplace add c9rgreen/zellij-mission-control
claude plugin install zellij-mission-control@zellij-mission-control
```

Any Claude Code session that was already open picks it up after
`/reload-plugins`; new sessions pick it up automatically. Your
`~/.claude/settings.json` is not touched. (The same two steps work as
`/plugin` commands inside a session, if you prefer.)

### 3. Check it

Start Claude Code in a Zellij pane and give it something to do. Its tab name
gains a marker, and when Claude finishes or asks you something, you get a
notification. Press `Alt a` to open the dashboard.

If nothing happens, see [Troubleshooting](#troubleshooting).

### Upgrading

A release changes both halves, and they should match:

- Zellij: change the version in the plugin URL in `config.kdl`, then start a
  new session or run `zellij action start-or-reload-plugin mission-control`.
  Zellij caches each URL separately, so the new version downloads on first use.
- Claude Code: `claude plugin update zellij-mission-control@zellij-mission-control`,
  then restart Claude Code.

<details>
<summary>Build from source, or register the hook by hand</summary>

**From source.** You need Rust with the `wasm32-wasip1` target and
[just](https://github.com/casey/just). `just install` builds the plugin,
copies it to `~/.config/zellij/plugins/`, copies the hook script to
`~/.claude/hooks/`, and reloads the plugin if you're inside Zellij. In
`config.kdl`, point at the local file instead of the release URL:

```kdl
mission-control location="file:~/.config/zellij/plugins/zellij-mission-control.wasm"
```

**Hook without the plugin.** Download `mission-control-hook.sh` from the
[latest release](https://github.com/c9rgreen/zellij-mission-control/releases/latest)
into `~/.claude/hooks/`, make it executable, and merge this into the `hooks`
object of `~/.claude/settings.json`. The same command runs for each of the
eleven events the plugin tracks:

```json
{
  "hooks": {
    "SessionStart":       [{ "matcher": "*", "hooks": [{ "type": "command", "command": "sh ~/.claude/hooks/mission-control-hook.sh", "timeout": 5 }] }],
    "UserPromptSubmit":   [{ "matcher": "*", "hooks": [{ "type": "command", "command": "sh ~/.claude/hooks/mission-control-hook.sh", "timeout": 5 }] }],
    "PreToolUse":         [{ "matcher": "*", "hooks": [{ "type": "command", "command": "sh ~/.claude/hooks/mission-control-hook.sh", "timeout": 5 }] }],
    "PostToolUse":        [{ "matcher": "*", "hooks": [{ "type": "command", "command": "sh ~/.claude/hooks/mission-control-hook.sh", "timeout": 5 }] }],
    "PostToolUseFailure": [{ "matcher": "*", "hooks": [{ "type": "command", "command": "sh ~/.claude/hooks/mission-control-hook.sh", "timeout": 5 }] }],
    "PermissionRequest":  [{ "matcher": "*", "hooks": [{ "type": "command", "command": "sh ~/.claude/hooks/mission-control-hook.sh", "timeout": 5 }] }],
    "PermissionDenied":   [{ "matcher": "*", "hooks": [{ "type": "command", "command": "sh ~/.claude/hooks/mission-control-hook.sh", "timeout": 5 }] }],
    "Notification":       [{ "matcher": "*", "hooks": [{ "type": "command", "command": "sh ~/.claude/hooks/mission-control-hook.sh", "timeout": 5 }] }],
    "Stop":               [{ "matcher": "*", "hooks": [{ "type": "command", "command": "sh ~/.claude/hooks/mission-control-hook.sh", "timeout": 5 }] }],
    "StopFailure":        [{ "matcher": "*", "hooks": [{ "type": "command", "command": "sh ~/.claude/hooks/mission-control-hook.sh", "timeout": 5 }] }],
    "SessionEnd":         [{ "matcher": "*", "hooks": [{ "type": "command", "command": "sh ~/.claude/hooks/mission-control-hook.sh", "timeout": 5 }] }]
  }
}
```

Don't do both: a session reporting through the plugin and through
`settings.json` sends every event twice.

</details>

## Using the dashboard

`Alt a` opens and closes the dashboard. Before you type anything, it lists
your Claude panes, most urgent first, then the actions you've run recently.
Enter jumps to the selected pane.

The dashboard is also a command palette. Typing fuzzy-searches Claude panes
(by project, tab and status), tabs, and every `zellij action`; space-separated
terms must all match. Start with `@` to search only panes, `#` only tabs, or
`>` only actions. Actions show the keys your config binds them to, and run on
the pane you opened the dashboard over.

| Key | Does |
| --- | --- |
| Up / Down, or Ctrl-j / Ctrl-k, or Ctrl-n / Ctrl-p | move |
| Enter | jump to the pane, switch to the tab, or run the action |
| Tab | put the action's command line in the search, to add arguments |
| Backspace, Ctrl-w, Ctrl-u | edit the search |
| Esc | clear the search, or close when it's empty |

An action marked `…` needs an argument. Enter or Tab puts its command line in
the search, eg. `>rename-tab `, and you finish it as you would after
`zellij action`: `>rename-tab api server`. Below the command line, the action's
other forms complete what you've typed, and the line under the list shows its
usage, or why it didn't run. A few actions print to the terminal or can't be
sent from a plugin, so they're left out.

## Configuration

Options go inside the `mission-control` block in `config.kdl`:

```kdl
mission-control location="https://github.com/c9rgreen/zellij-mission-control/releases/download/v0.1.0/zellij-mission-control.wasm" {
    notify "on"        // "on" | "off": terminal notification when a pane needs you
    tab_markers "on"   // "on" | "off": status icon in front of tab names
    icons "nerd"       // "nerd" | "unicode" | "ascii": which icon set to draw
}
```

### Tab markers

Each Claude pane in a tab puts an icon in front of the tab's name, most urgent
first, separated by spaces. The dashboard uses the same icons. The plugin can't
tell what your terminal's font can draw, so pick a set with the `icons` option.
(The `nerd` glyphs are private-use characters and may show as blank boxes on
this web page; the text next to each one says what it looks like.)

| Meaning | `nerd` (default) | `unicode` | `ascii` |
| --- | --- | --- | --- |
| blocked on you: a permission prompt or a question | `` warning | `!` | `!` |
| finished, and you haven't focused the pane since | `` check circle | `✓` | `*` |
| working | `` dotted circle, its dot blinking | `◔` | `~` |
| finished, already seen | `` hollow circle | `○` | `.` |

`nerd` needs a Nerd Font, or a terminal with the symbols built in, like
Ghostty. `unicode` avoids symbols that some terminals draw as wide emoji.

Renaming a tab yourself keeps working as usual, even to a name that starts
with something that looks like an icon: the plugin remembers each tab's own
name separately from the names it gave it, and leaves names alone while you're
in rename-tab mode.

## Troubleshooting

- **No marker appears and the dashboard is empty.** The hook isn't reaching
  the plugin. Check that `jq` is installed, that Claude Code is running inside
  a Zellij pane (the hook exits quietly if `ZELLIJ_PANE_ID` is unset), and that
  `/hooks` inside Claude Code lists `mission-control-hook.sh`. If the plugin
  was installed while a session was open, run `/reload-plugins` there.
- **Markers work but no notification arrives.** The notification is an
  `OSC 9` escape sequence, which your terminal must support and have enabled.
  Ghostty, iTerm2, WezTerm and kitty do. Over SSH, the local terminal is the
  one that decides.
- **Icons look wrong.** Set `icons "unicode"` or `icons "ascii"`.
- **The plugin didn't ask for permissions, or you declined.** Zellij remembers
  the answer in its cache dir (`zellij setup --check` prints the path) in
  `permissions.kdl`; remove the plugin's entry and reload it.

## How it works

Claude Code runs `hooks/mission-control-hook.sh` for each hook event. The
script trims the event JSON to a few fields and sends it to the plugin with
`zellij pipe --name mission-control::event`, then exits without waiting. The
plugin keeps per-pane state from those events and renders the dashboard. It
saves that state, along with each tab's own name, to its Zellij `/data`
folder, so reloading the plugin keeps your sessions.

Notifications are terminal escape codes only. The plugin writes `OSC 777` into
the Claude pane's pty; Zellij forwards it to the outer terminal as `OSC 9`, so
it works the same locally and over SSH, and the terminal decides whether and
how to show it.

To find the pty, the plugin asks Zellij for the pane's process, then reads that
process's terminal from `/proc`, or asks `ps` where there is no `/proc`, as on
macOS. Only a pty is ever written to. Other systems name terminals differently
in `ps`, so there the notification is refused rather than sent somewhere wrong.

## License

MIT. See [LICENSE](LICENSE).
