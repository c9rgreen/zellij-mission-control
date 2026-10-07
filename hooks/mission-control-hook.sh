#!/bin/sh
# Forwards a Claude Code hook event to the zellij-mission-control plugin.
# Install as a hook command for every event the plugin tracks; it reads the
# hook JSON on stdin and exits immediately.

[ -n "${ZELLIJ_PANE_ID:-}" ] || exit 0
command -v jq >/dev/null 2>&1 || exit 0
zellij_bin=$(command -v zellij) || exit 0

# One jq pass over the hook JSON, which can be megabytes of tool_input: a line
# naming the event and tool, then the trimmed payload. It stamps the time first.
# Each event is piped by its own process and may arrive out of order; Claude
# Code waits for turn-boundary hooks, so the plugin can drop anything older
# than the latest boundary
out=$(jq -r --argjson pane "$ZELLIJ_PANE_ID" '
    now as $at
    | "\(.hook_event_name // "-") \(.tool_name // "-")",
    ({
        event: .hook_event_name,
        pane: $pane,
        cwd: (.cwd // ""),
        tool: .tool_name,
        tool_use_id: .tool_use_id,
        agent_id: .agent_id,
        kind: .notification_type,
        message: .message,
        source: .source,
        at: $at
    } | tojson)') || exit 0
nl='
'
meta=${out%%"$nl"*}
payload=${out#*"$nl"}
set -f
# shellcheck disable=SC2086
set -- $meta
event=$1 tool=$2

# Find the pane's pty on the nearest ancestor with a terminal (the Claude
# process). The plugin writes OSC notifications there. Only events that can
# alert need it.
pane_tty=
case $event:$tool in
PreToolUse:AskUserQuestion | PreToolUse:ExitPlanMode | \
    SessionStart:* | PermissionRequest:* | Notification:* | Stop:* | StopFailure:*)
    pid=$$
    while [ "${pid:-1}" -gt 1 ]; do
        t=$(ps -o tty= -p "$pid" 2>/dev/null | tr -d ' ')
        case $t in
        '' | '?' | '??') pid=$(ps -o ppid= -p "$pid" 2>/dev/null | tr -d ' ') ;;
        *) pane_tty=/dev/$t && break ;;
        esac
    done
    ;;
esac

# A tty path needs no JSON escaping
case $pane_tty in
/dev/[A-Za-z0-9/]*) payload="${payload%\}},\"tty\":\"$pane_tty\"}" ;;
esac

# Detach so a missing or slow plugin never delays Claude
"$zellij_bin" pipe --name mission-control::event -- "$payload" >/dev/null 2>&1 </dev/null &
