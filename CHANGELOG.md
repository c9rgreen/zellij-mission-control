# Changelog

All notable changes to this project are recorded here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the project uses
[Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- A Claude Code plugin manifest and marketplace, so the hooks install with
  `claude plugin marketplace add c9rgreen/zellij-mission-control` and
  `claude plugin install zellij-mission-control@zellij-mission-control`
  instead of hand-editing `settings.json`.

- `just bump <version>` sets the version in `Cargo.toml`, `Cargo.lock`, the
  plugin manifest and the README's release URLs in one go. The release
  workflow refuses a tag those files don't agree with.

### Changed

- The README now has Zellij load the plugin straight from the release URL,
  with the local `file:` location kept for builds from source.
- The README is reordered for first-time readers: install, a check that it
  works, upgrading, using the dashboard, configuration and troubleshooting
  come before the internals. The build-from-source and manual-hook paths are
  collapsed behind a details block.

## [0.1.0] - 2026-10-19

First public release.

### Added

- A dashboard that lists every Claude Code session in the current Zellij
  session, most urgent first, and shows whether each is working, finished, or
  waiting on a permission prompt or question.
- A command palette in the same dashboard: fuzzy search over sessions, tabs,
  and every `zellij action`, with `@`, `#` and `>` prefixes to narrow the
  search and the bound keys shown next to each action.
- Tab markers that put each session's status in front of its tab name, with
  `nerd`, `unicode` and `ascii` icon sets chosen by the `icons` option.
- Terminal notifications when a session needs you, sent as `OSC 777` into the
  Claude pane's pty so they reach the outer terminal locally and over SSH.
  Linux and macOS are supported.
- State persisted to the plugin's Zellij `/data` folder, so reloading the
  plugin keeps your sessions and each tab's own name.
- `hooks/mission-control-hook.sh`, a Claude Code hook that trims each event
  and forwards it to the plugin with `zellij pipe`.
- `notify`, `tab_markers` and `icons` plugin options.

[Unreleased]: https://github.com/c9rgreen/zellij-mission-control/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/c9rgreen/zellij-mission-control/releases/tag/v0.1.0
