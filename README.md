# zellij-mission-control

[![CI](https://github.com/c9rgreen/zellij-mission-control/actions/workflows/ci.yml/badge.svg)](https://github.com/c9rgreen/zellij-mission-control/actions/workflows/ci.yml)

A Zellij plugin that lists every Claude Code session in the current Zellij
session, shows whether each one is working, ready, or waiting on you, and
sends a terminal notification when one needs you. It also puts each session's
status in front of its tab's name, so the tab bar always shows what needs you.

<!-- screenshot: dashboard open over a few Claude panes, tab bar showing markers -->

This is an independent project, not affiliated with Anthropic or the Zellij
project.

## Tab markers

Each session in a tab puts an icon in front of the tab's name, most urgent
first, separated by spaces. The dashboard uses the same icons. The plugin can't tell
what your terminal's font can draw, so pick a set with the `icons` option:

| Meaning | `nerd` (default) | `unicode` | `ascii` |
| --- | --- | --- | --- |
| blocked on you: a permission prompt or a question | `` warning | `!` | `!` |
| finished, and you haven't focused the pane since | `` check circle | `✓` | `*` |
| working | `` dotted circle, its dot blinking | `◔` | `~` |
| finished, already seen | `` hollow circle | `○` | `.` |

`nerd` needs a Nerd Font, or a terminal with the symbols built in, like
Ghostty. `unicode` avoids symbols that some terminals draw as wide emoji.

The plugin remembers each tab's own name and the names it gave the tab, so renaming
a tab yourself works as usual, even to a name that starts with something that
looks like an icon. It leaves names alone while you're in rename-tab mode.

## How it works

Claude Code hooks run `hooks/mission-control-hook.sh`, which forwards a trimmed
event to the plugin with `zellij pipe --name mission-control::event`. The plugin keeps per-pane
state and renders the dashboard. It saves that state, along with each tab's
own name, to its Zellij `/data` folder, so reloading the plugin (as
`just install` does) keeps your sessions.

Notifications are terminal escape codes only. The plugin writes `OSC 777` into
the Claude pane's pty; Zellij forwards it to the outer terminal as `OSC 9`, so
it works the same locally and over SSH. The terminal decides whether and how to
show it (Ghostty, iTerm2, WezTerm and kitty all support OSC 9).

To find the pty, the plugin asks Zellij for the pane's process, then reads that
process's terminal from `/proc`, or asks `ps` where there is no `/proc`, as on
macOS. Only a pty is ever written to. Linux and macOS are supported; other
systems name terminals differently in `ps`, so there the notification is
refused rather than sent somewhere wrong.

## Install

Requires Zellij 0.44 or newer, which can tell a plugin a pane's process, and
`jq`. Linux and macOS are supported. There are two halves: the Zellij plugin,
and a Claude Code hook that reports to it.

### Zellij

Add this to `~/.config/zellij/config.kdl`. Zellij downloads the plugin from
the release URL the first time it loads and caches it by URL, so to upgrade,
change the version in the URL:

```kdl
plugins {
    mission-control location="https://github.com/c9rgreen/zellij-mission-control/releases/download/v0.1.0/zellij-mission-control.wasm" {
        // notify "on" | "off"
        // tab_markers "on" | "off"
        // icons "nerd" | "unicode" | "ascii"
    }
}
load_plugins {
    mission-control
}
keybinds {
    shared_among "normal" "locked" {
        bind "Alt a" { MessagePlugin "mission-control" { name "mission-control::toggle"; }; }
    }
}
```

The first time a Zellij session loads the plugin it asks for five permissions:
read application state (to see panes and tabs), change application state (to
rename tabs and focus panes), run commands (to look up a pane's pty with `ps`
and write the notification to it), read CLI pipes (to receive the hook's
events), and run actions as you (for the command palette).

### Claude Code

Install the hooks as a Claude Code plugin. This repository is its own
marketplace:

```sh
claude plugin marketplace add c9rgreen/zellij-mission-control
claude plugin install zellij-mission-control@zellij-mission-control
```

Inside a running session, `/plugin marketplace add c9rgreen/zellij-mission-control`
then `/plugin install zellij-mission-control@zellij-mission-control` does the
same through the plugin browser. Either way, run `/reload-plugins` in any
session that was already open, or start a new one. Nothing in
`~/.claude/settings.json` needs editing.

### Alternatives

To build the plugin from source you also need Rust with the `wasm32-wasip1`
target and [just](https://github.com/casey/just). `just install` builds the
`.wasm`, copies it and the hook script into place, and reloads the plugin.
Point Zellij at the local file instead of the URL:

```kdl
    mission-control location="file:~/.config/zellij/plugins/zellij-mission-control.wasm" {
```

To register the hooks by hand instead of through the plugin, download
`mission-control-hook.sh` from the
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

## Keys

The dashboard is also a command palette. Before you type, it lists your
sessions, most urgent first, then the actions you ran recently. Typing
fuzzy-searches sessions (by project, tab and status), tabs, and every
`zellij action`; space-separated terms must all match. Start with `@` to search
only sessions, `#` only tabs, or `>` only actions. Actions show the keys your
config binds them to.

Arrows (or Ctrl-j/k, Ctrl-n/p when Zellij doesn't claim them) move. Enter jumps
to the session, switches to the tab, or runs the action on the pane you opened
the dashboard over. Backspace, Ctrl-w and Ctrl-u edit the search, and Esc
clears it or, when empty, closes.

An action marked `…` needs an argument. Enter (or Tab, for any action) puts
its command line in the search, eg. `>rename-tab `, to finish as you would
after `zellij action`: `>rename-tab api server`. Below the command line, the
action's other forms complete what you've typed, and the line under the list
shows its usage, or why it didn't run. A few actions print to the terminal or
can't be sent from a plugin, so they're left out.

## License

MIT. See [LICENSE](LICENSE).
