# zellij-mission-control

A Zellij plugin that lists every Claude Code session in the current Zellij
session, shows whether each one is working, ready, or waiting on you, and
sends a terminal notification when one needs you. It also puts each session's
status in front of its tab's name, so the tab bar always shows what needs you.

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
process's terminal from `/proc` on Linux or asks `ps` on macOS. Other systems
haven't been tried.

## Install

Requires Rust with the `wasm32-wasip1` target and `jq`.

```sh
just install   # builds, copies the .wasm and hook script into place
```

Zellij config (`~/.config/zellij/config.kdl`):

```kdl
plugins {
    mission-control location="file:~/.config/zellij/plugins/zellij-mission-control.wasm" {
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

Register `sh ~/.claude/hooks/mission-control-hook.sh` in `~/.claude/settings.json`
for SessionStart, UserPromptSubmit, PreToolUse, PostToolUse, PostToolUseFailure,
PermissionRequest, PermissionDenied, Notification, Stop, StopFailure, and
SessionEnd.

The first time a Zellij session loads the plugin it asks for permissions.

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
