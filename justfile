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
