plugin_dir := env("HOME") / ".config/zellij/plugins"
hook_dir := env("HOME") / ".claude/hooks"

# Build the plugin
build:
    cargo build --release --target wasm32-wasip1

# Run the state-tracking unit tests natively
test:
    cargo test

# Install the plugin and hook script, then reload the plugin in this session
install: build
    mkdir -p {{plugin_dir}} {{hook_dir}}
    cp target/wasm32-wasip1/release/zellij-mission-control.wasm {{plugin_dir}}/zellij-mission-control.wasm
    install -m 755 hooks/mission-control-hook.sh {{hook_dir}}/mission-control-hook.sh
    -[ -n "${ZELLIJ:-}" ] && zellij action start-or-reload-plugin mission-control

# Set the release version everywhere it's written down: Cargo.toml, Cargo.lock,
# the Claude Code plugin manifest, and the README's release URLs. Commit, then
# tag v<version>; the release workflow checks that they agree.
bump version:
    perl -pi -e 's/^version = "[^"]*"/version = "{{version}}"/' Cargo.toml
    perl -pi -e 's/"version": "[^"]*"/"version": "{{version}}"/' .claude-plugin/plugin.json
    perl -pi -e 's{releases/download/v[^/]+/}{releases/download/v{{version}}/}g' README.md
    cargo update --workspace
    @git --no-pager diff --stat
