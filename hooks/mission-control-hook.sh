#!/bin/sh
# Forwards a Claude Code hook event to the zellij-mission-control plugin.
# Install as a hook command for every event the plugin tracks; it reads the
# hook JSON on stdin and exits immediately.

# The release this script came from, so the plugin can say when the two
# differ. `just bump` sets it.
hook_version="0.1.0"

[ -n "${ZELLIJ_PANE_ID:-}" ] || exit 0
command -v jq >/dev/null 2>&1 || exit 0
zellij_bin=$(command -v zellij) || exit 0

# One jq pass over the hook JSON, which can be megabytes of tool_input, down
# to the fields the plugin uses. It stamps the time first. Each event is piped
# by its own process and may arrive out of order; Claude Code waits for
# turn-boundary hooks, so the plugin can drop anything older than the latest
# boundary
payload=$(jq -c --argjson pane "$ZELLIJ_PANE_ID" --arg hook_version "$hook_version" '
    now as $at
    | {
        event: .hook_event_name,
        pane: $pane,
        cwd: (.cwd // ""),
        tool: .tool_name,
        tool_use_id: .tool_use_id,
        agent_id: .agent_id,
        kind: .notification_type,
        message: .message,
        source: .source,
        hook_version: $hook_version,
        at: $at
    }') || exit 0

# Detach so a missing or slow plugin never delays Claude
"$zellij_bin" pipe --name mission-control::event -- "$payload" >/dev/null 2>&1 </dev/null &
