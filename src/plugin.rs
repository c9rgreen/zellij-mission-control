use crate::notify::NOTIFY_SH;
use crate::palette::{dispatch, parse_query, Catalog, Dispatch, Entry, Query, Scope, Target};
use crate::state::{self, format_age, fuzzy_match, Alert, Badge, HookEvent, Session, Tracker};
use crate::tabs::{tab_name, IconSet, TabNames};
use std::cmp::Reverse;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::ops::Range;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};
use zellij_tile::prelude::actions::Action;
use zellij_tile::prelude::*;
use zellij_utils::input::layout::RunPluginOrAlias;

/// Hook events arrive here; see hooks/mission-control-hook.sh.
const PIPE_EVENT: &str = "mission-control::event";
/// Sent by a keybinding to show or hide the dashboard.
const PIPE_TOGGLE: &str = "mission-control::toggle";
/// The alias the plugin is configured under in config.kdl.
const ALIAS: &str = "mission-control";
/// Sessions saved across plugin reloads. Zellij keeps /data per Zellij session
/// and plugin instance, which a reload keeps, so other sessions' panes never
/// show up here.
const STATE_FILE: &str = "/data/sessions.json";
/// Written first, then renamed over STATE_FILE, so a reload mid-write never
/// leaves a torn file.
const STATE_TMP: &str = "/data/sessions.json.tmp";
/// Recent actions shown under the sessions before anything's typed.
const RECENT_SHOWN: usize = 5;
/// Recent actions remembered, which also break ties in the search.
const RECENT_KEPT: usize = 20;

#[derive(Default)]
pub struct Dashboard {
    tracker: Tracker,
    notify: bool,
    /// Put session status icons in tab names.
    tab_markers: bool,
    icons: IconSet,
    /// Working icons show their lit frame; alternates each second to blink them.
    lit: bool,
    /// Each tab's own name, and the names we gave it.
    tab_names: TabNames,
    own_id: u32,
    /// Terminal pane id → tab position.
    pane_tab: HashMap<u32, usize>,
    /// Tab position → its focused panes.
    tab_focus: HashMap<usize, Focus>,
    tabs: Vec<TabInfo>,
    /// pane_tab and tab_focus match the tabs' current positions. Zellij reports
    /// tabs before panes, so after tabs move, close or open, the pane maps are
    /// stale until the next PaneUpdate; so are they after a reload.
    panes_current: bool,
    active_tab: Option<usize>,
    /// The user is renaming a tab, so leave tab names alone.
    renaming_tab: bool,
    /// Tab position holding our own pane, which stays put while hidden.
    own_tab: Option<usize>,
    /// Our pane is shown, unless it floats in a tab whose floating panes are hidden.
    own_shown: bool,
    own_floating: bool,
    /// The user has asked to see the dashboard since it loaded.
    toggled: bool,
    selected: Option<Pick>,
    /// The first line shown, when they don't all fit.
    scroll: usize,
    /// What the user has typed to filter the dashboard.
    query: String,
    /// Zellij's actions, for the palette.
    catalog: Catalog,
    /// What was focused when the dashboard opened, which actions run on.
    target: Target,
    /// Recent actions and the base mode.
    palette: state::Palette,
    /// Why the last action didn't run.
    error: Option<String>,
    /// For the palette's key hints.
    keybinds: KeybindsVec,
    /// The last state written to STATE_FILE, to skip writes when nothing changed.
    saved: String,
    /// State changed since the last save. Saving waits for the next timer tick,
    /// so a burst of tool calls doesn't write the file for each one.
    dirty: bool,
    /// The last save failed; it's retried each tick, but logged once.
    save_failing: bool,
}

/// The focused pane in each of a tab's layers; Zellij reports focus per layer.
#[derive(Default)]
struct Focus {
    /// A terminal pane; tiled plugins (bars, file pickers) never hold a session.
    tiled: Option<u32>,
    floating: Option<Floating>,
}

enum Floating {
    Terminal(u32),
    /// A plugin such as this dashboard, which hides the session behind it.
    Plugin,
}

/// What a row stands for, to keep the selection across renders.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Pick {
    Session(u32),
    /// By tab id.
    Tab(usize),
    /// By catalog index.
    Action(usize),
    /// The command line being typed.
    Command,
}

/// A row that matches the query, and what to highlight.
struct Item<'a> {
    pick: Pick,
    kind: Kind<'a>,
    /// Matched char positions in each searched cell.
    hits: Vec<Vec<usize>>,
}

enum Kind<'a> {
    Session { session: &'a Session, tab: String },
    Tab(String),
    Action(&'a Entry),
    Command(&'a str),
}

/// The rows to show, in order.
struct Listing<'a> {
    items: Vec<Item<'a>>,
    /// Where the recent actions start, under the sessions, to head them.
    recent_from: Option<usize>,
    /// The row selected until the user picks one.
    default: usize,
}

/// A row's text, built a cell at a time, and where each style goes.
#[derive(Default)]
struct Line {
    text: String,
    len: usize,
    hits: Vec<usize>,
    dim: Vec<Range<usize>>,
    color: Vec<Range<usize>>,
    glyph: Option<(Range<usize>, Option<Badge>)>,
}

impl Line {
    /// Add a cell, padded to `width`; returns where its text went.
    fn push(&mut self, s: &str, width: usize) -> Range<usize> {
        let n = s.chars().count();
        let start = self.len;
        self.text.push_str(s);
        self.text
            .extend(std::iter::repeat_n(' ', width.saturating_sub(n)));
        self.len += n.max(width);
        start..start + n
    }

    fn hit(&mut self, s: &str, width: usize, hits: &[usize]) {
        let at = self.push(s, width).start;
        self.hits.extend(hits.iter().map(|h| at + h));
    }

    /// Draw the row at `y`, full width. Matched runs are drawn as their own
    /// pieces, since only a whole piece can take a background: the theme's
    /// selected one, or on the selected row, which already has that, the
    /// plain one.
    fn print(mut self, y: usize, selected: bool, cols: usize) {
        self.push("", cols.saturating_sub(self.len));
        let chars: Vec<char> = self.text.chars().collect();
        let hit = |i: usize| self.hits.contains(&i);
        let mut start = 0;
        while start < chars.len() {
            let matched = hit(start);
            let end = (start..chars.len())
                .find(|&i| hit(i) != matched)
                .unwrap_or(chars.len());
            let piece = self.piece(&chars, start..end);
            let piece = match (matched, selected) {
                (true, true) => piece.opaque(),
                (false, false) => piece,
                _ => piece.selected(),
            };
            print_text_with_coordinates(piece, start, y, Some(end - start), None);
            start = end;
        }
    }

    /// The chars in `range`, with the styles that fall in it.
    fn piece(&self, chars: &[char], range: Range<usize>) -> Text {
        let clip = |r: &Range<usize>| {
            let (a, b) = (r.start.max(range.start), r.end.min(range.end));
            (a < b).then(|| a - range.start..b - range.start)
        };
        let hits = self
            .hits
            .iter()
            .filter(|h| range.contains(h))
            .map(|h| h - range.start)
            .collect();
        let mut text =
            Text::new(chars[range.clone()].iter().collect::<String>()).color_indices(3, hits);
        for r in self.dim.iter().filter_map(clip) {
            text = text.dim_range(r);
        }
        for r in self.color.iter().filter_map(clip) {
            text = text.color_range(2, r);
        }
        let Some((r, badge)) = &self.glyph else {
            return text;
        };
        let Some(r) = clip(r) else { return text };
        match badge {
            Some(Badge::NeedsInput) => text.error_color_range(r),
            Some(Badge::ReadyUnseen) => text.success_color_range(r),
            Some(Badge::Working) => text.color_range(2, r),
            Some(Badge::ReadySeen) => text.dim_range(r),
            None => text.color_range(1, r),
        }
    }
}

fn now() -> u64 {
    now_ms() / 1000
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Wake just after the next second starts. A reload can leave the old
/// instance's timer running alongside the new one's, so ticks don't count
/// anything; they read the clock, and every timer agrees on which second it is.
fn tick() {
    let ms = now_ms();
    set_timeout((1000 - ms % 1000 + 20) as f64 / 1000.0);
}

/// Whether Working icons show their lit frame: on even seconds, rounding
/// so a tick that fires a little early still lands in its second.
fn lit_at(ms: u64) -> bool {
    ((ms + 500) / 1000).is_multiple_of(2)
}

/// Keep control characters from ending an escape sequence early.
fn strip_controls(s: &str) -> String {
    s.chars().filter(|c| !c.is_control()).collect()
}

impl ZellijPlugin for Dashboard {
    fn load(&mut self, config: BTreeMap<String, String>) {
        self.notify = config.get("notify").map(String::as_str) != Some("off");
        self.tab_markers = config.get("tab_markers").map(String::as_str) != Some("off");
        self.icons = match config.get("icons") {
            None => IconSet::default(),
            Some(name) => IconSet::parse(name).unwrap_or_else(|| {
                eprintln!("mission-control: unknown icons {name:?}; use \"nerd\", \"unicode\" or \"ascii\"");
                IconSet::default()
            }),
        };
        self.own_id = get_plugin_ids().plugin_id;
        // Panes that closed while we were gone drop out on the first PaneUpdate
        if let Ok(json) = std::fs::read_to_string(STATE_FILE) {
            (self.tracker, self.tab_names, self.palette) = state::restore(&json);
            self.saved = json;
        }
        request_permission(&[
            PermissionType::ReadApplicationState,
            PermissionType::ChangeApplicationState,
            PermissionType::RunCommands,
            PermissionType::ReadCliPipes,
            PermissionType::RunActionsAsUser,
        ]);
        subscribe(&[
            EventType::PermissionRequestResult,
            EventType::TabUpdate,
            EventType::PaneUpdate,
            EventType::ModeUpdate,
            EventType::Key,
            EventType::Timer,
            EventType::RunCommandResult,
            EventType::InitialKeybinds,
        ]);
        tick();
    }

    fn update(&mut self, event: Event) -> bool {
        match event {
            Event::PermissionRequestResult(PermissionStatus::Granted) => {
                rename_plugin_pane(self.own_id, "mission control");
                // The first-run prompt showed the pane; tuck it away again.
                // This comes on every load, so after a reload it can land
                // just after the user opened the dashboard; leave that open.
                if !self.toggled {
                    hide_self();
                }
                true
            }
            Event::TabUpdate(tabs) => {
                let layout = |t: &TabInfo| (t.position, t.tab_id);
                // The first TabUpdate has nothing to compare; panes_current
                // already waits for a PaneUpdate if none has come
                if !self.tabs.is_empty()
                    && !tabs.iter().map(layout).eq(self.tabs.iter().map(layout))
                {
                    self.panes_current = false;
                }
                self.active_tab = tabs.iter().find(|t| t.active).map(|t| t.position);
                self.tabs = tabs;
                self.mark_focused_seen();
                self.sync_tab_names();
                true
            }
            Event::PaneUpdate(manifest) => {
                self.on_panes(manifest);
                self.panes_current = true;
                self.mark_focused_seen();
                self.sync_tab_names();
                true
            }
            Event::ModeUpdate(mode) => {
                let renaming = mode.mode == InputMode::RenameTab;
                if renaming != self.renaming_tab {
                    self.renaming_tab = renaming;
                    self.tab_names.clear_pending();
                    self.sync_tab_names();
                }
                if mode.base_mode.is_some() && mode.base_mode != self.palette.base_mode {
                    self.palette.base_mode = mode.base_mode;
                    self.dirty = true;
                }
                self.catalog
                    .set_keybinds(&self.keybinds, self.palette.base_mode)
                    && self.own_visible()
            }
            // Sent once subscribed and after the config changes; ModeUpdate
            // leaves keybindings out once a plugin asks for these
            Event::InitialKeybinds(keybinds) => {
                self.keybinds = keybinds;
                self.catalog
                    .set_keybinds(&self.keybinds, self.palette.base_mode)
                    && self.own_visible()
            }
            Event::Key(key) => self.on_key(key),
            Event::RunCommandResult(code, _, stderr, _) => {
                // Lands in the Zellij log; there's nowhere better to surface it
                if code != Some(0) {
                    eprintln!(
                        "mission-control: notify exited {code:?}: {}",
                        String::from_utf8_lossy(&stderr)
                    );
                }
                false
            }
            Event::Timer(_) => {
                tick();
                self.lit = lit_at(now_ms());
                self.sync_tab_names();
                self.persist();
                // Only the ages and blinking icons change, which matter only while shown
                self.own_visible()
            }
            _ => false,
        }
    }

    fn pipe(&mut self, msg: PipeMessage) -> bool {
        // Let `zellij pipe` exit instead of waiting on us
        if let PipeSource::Cli(id) = &msg.source {
            unblock_cli_pipe_input(id);
            cli_pipe_output(id, "");
        }
        match msg.name.as_str() {
            PIPE_EVENT => {
                let Some(ev) = msg
                    .payload
                    .as_deref()
                    .and_then(|p| serde_json::from_str::<HookEvent>(p).ok())
                else {
                    return false;
                };
                if let Some(alert) = self.tracker.apply(ev, now()) {
                    self.send_alert(&alert);
                }
                self.dirty = true;
                self.mark_focused_seen();
                self.sync_tab_names();
                true
            }
            PIPE_TOGGLE => {
                self.toggle();
                false
            }
            _ => false,
        }
    }

    fn render(&mut self, rows: usize, cols: usize) {
        let listing = self.list();
        let current = self.current(&listing);
        let at = current.and_then(|c| listing.items.iter().position(|i| i.pick == c));
        // A line per row, with a heading over the recent actions
        let mut lines: Vec<Option<usize>> = Vec::new();
        for i in 0..listing.items.len() {
            if listing.recent_from == Some(i) {
                lines.push(None);
            }
            lines.push(Some(i));
        }
        let fit = rows.saturating_sub(4).max(1);
        // Scroll just enough to keep the selection in view
        let pos = at
            .and_then(|a| lines.iter().position(|l| *l == Some(a)))
            .unwrap_or(0);
        let scroll = self
            .scroll
            .min(pos)
            .max((pos + 1).saturating_sub(fit))
            .min(lines.len().saturating_sub(fit));

        let prompt = if self.query.is_empty() {
            Text::new("/ search · @ sessions · # tabs · > actions").dim_range(2..)
        } else {
            Text::new(format!("/ {}", self.query))
        };
        print_text_with_coordinates(
            prompt.color_range(2, ..1),
            1,
            0,
            Some(cols.saturating_sub(2)),
            None,
        );

        let query = parse_query(&self.query, &self.catalog);
        if listing.items.is_empty() {
            let msg = match query {
                Query::Search(Scope::All | Scope::Sessions, text)
                    if text.trim().is_empty() && self.tracker.is_empty() =>
                {
                    "No Claude sessions in this Zellij session"
                }
                _ => "No matches",
            };
            print_text_with_coordinates(
                Text::new(msg).dim_all(),
                1,
                2,
                Some(cols.saturating_sub(2)),
                None,
            );
        }
        // Session cells line up with each other
        let mut widths = [0; 3];
        for item in &listing.items {
            if let Kind::Session { session, tab } = &item.kind {
                let cells = [
                    session.project().chars().count(),
                    tab.chars().count(),
                    session.status.label().chars().count(),
                ];
                for (w, n) in widths.iter_mut().zip(cells) {
                    *w = (*w).max(n);
                }
            }
        }
        let now = now();
        for (y, line) in lines.iter().skip(scroll).take(fit).enumerate() {
            match line {
                None => print_text_with_coordinates(
                    Text::new(" recent").dim_all(),
                    0,
                    2 + y,
                    Some(cols),
                    None,
                ),
                Some(i) => {
                    let item = &listing.items[*i];
                    self.line(item, widths, now, cols).print(
                        2 + y,
                        current == Some(item.pick),
                        cols,
                    );
                }
            }
        }

        let detail = if let Some(error) = &self.error {
            Some(Text::new(error).error_color_all())
        } else if let Query::Command { name, .. } = query {
            Some(Text::new(format!("usage: {}", self.catalog.usage(name))).dim_all())
        } else if let Some(Pick::Action(i)) = current {
            let about = &self.catalog.entries[i].about;
            (!about.is_empty()).then(|| Text::new(about).dim_all())
        } else {
            None
        };
        if let Some(detail) = detail {
            print_text_with_coordinates(
                detail,
                1,
                rows.saturating_sub(2),
                Some(cols.saturating_sub(2)),
                None,
            );
        }
        let more = match at {
            Some(i) if lines.len() > fit => format!("{}/{} · ", i + 1, listing.items.len()),
            _ => String::new(),
        };
        let help = format!("{more}↑↓ move · Enter go · Tab edit · Esc clear/close");
        print_text_with_coordinates(
            Text::new(help).dim_all(),
            1,
            rows.saturating_sub(1),
            Some(cols.saturating_sub(2)),
            None,
        );
        drop(listing);
        self.scroll = scroll;
    }
}

impl Dashboard {
    /// Save state if it changed since the last save.
    fn persist(&mut self) {
        if !std::mem::take(&mut self.dirty) {
            return;
        }
        let json = state::save(&self.tracker, &self.tab_names, &self.palette);
        if json == self.saved {
            return;
        }
        match std::fs::write(STATE_TMP, &json).and_then(|()| std::fs::rename(STATE_TMP, STATE_FILE))
        {
            Ok(()) => {
                self.saved = json;
                self.save_failing = false;
            }
            Err(e) => {
                if !std::mem::replace(&mut self.save_failing, true) {
                    eprintln!("mission-control: can't save {STATE_FILE}: {e}");
                }
                self.dirty = true;
            }
        }
    }

    /// The rows for the query. Before anything's typed, that's every session,
    /// most urgent first, then recent actions. A search puts the best match
    /// first.
    fn list(&self) -> Listing<'_> {
        let text = match parse_query(&self.query, &self.catalog) {
            Query::Command { name, rest } => return self.command_list(name, rest),
            Query::Search(Scope::All, text) if text.trim().is_empty() => {
                let mut items: Vec<Item> = self
                    .sessions("")
                    .into_iter()
                    .map(|(_, item)| item)
                    .collect();
                let from = items.len();
                let recent = self
                    .palette
                    .recent
                    .iter()
                    .filter_map(|c| self.catalog.entries.iter().position(|e| e.command() == *c))
                    .take(RECENT_SHOWN)
                    .map(|i| self.action_item(i, Vec::new()));
                items.extend(recent);
                let recent_from = (items.len() > from).then_some(from);
                return Listing {
                    items,
                    recent_from,
                    default: 0,
                };
            }
            Query::Search(scope, text) => (scope, text),
        };
        let (scope, text) = text;
        // Ties go to sessions, then tabs, then the most recent and shortest actions
        let mut scored: Vec<(i32, usize, (usize, usize), Item)> = Vec::new();
        if matches!(scope, Scope::All | Scope::Sessions) {
            scored.extend(
                self.sessions(text)
                    .into_iter()
                    .map(|(score, item)| (score, 0, (0, 0), item)),
            );
        }
        if matches!(scope, Scope::All | Scope::Tabs) {
            let mut tabs: Vec<&TabInfo> = self.tabs.iter().collect();
            tabs.sort_by_key(|t| t.position);
            for t in tabs {
                let label = format!(
                    "{} {}",
                    t.position + 1,
                    self.tab_names.base(t.tab_id, &t.name)
                );
                if let Some((score, hits)) = fuzzy_match(text, &[&label]) {
                    scored.push((
                        score,
                        1,
                        (0, 0),
                        Item {
                            pick: Pick::Tab(t.tab_id),
                            kind: Kind::Tab(label),
                            hits,
                        },
                    ));
                }
            }
        }
        if matches!(scope, Scope::All | Scope::Actions) {
            for (i, e) in self.catalog.entries.iter().enumerate() {
                if let Some((score, mut hits)) = fuzzy_match(text, &[&e.label]) {
                    let recency = self
                        .palette
                        .recent
                        .iter()
                        .position(|c| *c == e.command())
                        .unwrap_or(usize::MAX);
                    scored.push((
                        score,
                        2,
                        (recency, e.label.len()),
                        self.action_item(i, hits.swap_remove(0)),
                    ));
                }
            }
        }
        // Stable, so equal keys keep urgency, position and catalog order
        scored.sort_by_key(|(score, kind, recency, _)| (Reverse(*score), *kind, *recency));
        Listing {
            items: scored.into_iter().map(|(.., item)| item).collect(),
            recent_from: None,
            default: 0,
        }
    }

    /// Sessions matching the query, most urgent first, with their scores.
    fn sessions(&self, query: &str) -> Vec<(i32, Item<'_>)> {
        self.tracker
            .sorted(|p| self.pane_tab.get(&p).copied())
            .into_iter()
            .filter_map(|session| {
                let tab = self.tab_label(session.pane);
                let (score, hits) =
                    fuzzy_match(query, &[session.project(), &tab, &session.status.label()])?;
                let kind = Kind::Session { session, tab };
                Some((
                    score,
                    Item {
                        pick: Pick::Session(session.pane),
                        kind,
                        hits,
                    },
                ))
            })
            .collect()
    }

    fn action_item(&self, i: usize, hits: Vec<usize>) -> Item<'_> {
        Item {
            pick: Pick::Action(i),
            kind: Kind::Action(&self.catalog.entries[i]),
            hits: vec![hits],
        }
    }

    /// The command line being typed, then the action's entries that match
    /// its arguments, to complete them.
    fn command_list<'a>(&'a self, name: &str, rest: &str) -> Listing<'a> {
        let typed = self.query.trim_start_matches('>').trim_start();
        let mut items = vec![Item {
            pick: Pick::Command,
            kind: Kind::Command(typed),
            hits: Vec::new(),
        }];
        for (i, e) in self
            .catalog
            .entries
            .iter()
            .enumerate()
            .filter(|(_, e)| e.name == name)
        {
            // Match the arguments' part of the label
            let head = e.name.replace('-', " ");
            let tail = e.label.strip_prefix(&head).unwrap_or_default().trim_start();
            let skip = e.label.chars().count() - tail.chars().count();
            if let Some((_, hits)) = fuzzy_match(rest, &[tail]) {
                items.push(self.action_item(i, hits[0].iter().map(|h| h + skip).collect()));
            }
        }
        // Start on the command line once it would run
        let runs = self
            .catalog
            .actions(name, rest, self.target, Box::new(|| "/".into()))
            .is_ok();
        let default = if runs || items.len() == 1 { 0 } else { 1 };
        Listing {
            items,
            recent_from: None,
            default,
        }
    }

    /// The selected row, falling back to the default once it's filtered out.
    fn current(&self, listing: &Listing) -> Option<Pick> {
        self.selected
            .filter(|sel| listing.items.iter().any(|i| i.pick == *sel))
            .or_else(|| listing.items.get(listing.default).map(|i| i.pick))
    }

    fn tab_label(&self, pane: u32) -> String {
        self.pane_tab
            .get(&pane)
            .map(|pos| {
                let name = self
                    .tabs
                    .iter()
                    .find(|t| t.position == *pos)
                    .map(|t| self.tab_names.base(t.tab_id, &t.name))
                    .unwrap_or_default();
                format!("{} {name}", pos + 1)
            })
            .unwrap_or_default()
    }

    fn line(&self, item: &Item, widths: [usize; 3], now: u64, cols: usize) -> Line {
        let mut line = Line::default();
        line.push(" ", 1);
        let glyph = match &item.kind {
            Kind::Session { session, .. } => (
                self.icons.icon(session.badge(), self.lit),
                Some(session.badge()),
            ),
            Kind::Tab(_) => (self.icons.tab(), None),
            Kind::Action(_) | Kind::Command(_) => (self.icons.action(), None),
        };
        let at = line.push(&glyph.0.to_string(), 1);
        line.glyph = Some((at, glyph.1));
        line.push(" ", 1);
        match &item.kind {
            Kind::Session { session, tab } => {
                line.hit(session.project(), widths[0] + 2, &item.hits[0]);
                line.hit(tab, widths[1] + 2, &item.hits[1]);
                line.hit(&session.status.label(), widths[2] + 2, &item.hits[2]);
                let age = line.push(&format_age(now.saturating_sub(session.since)), 0);
                line.dim.push(age);
            }
            Kind::Tab(label) => line.hit(label, 0, &item.hits[0]),
            Kind::Action(e) => {
                line.hit(&e.label, 0, &item.hits[0]);
                if e.needs_input {
                    let more = line.push("…", 0);
                    line.dim.push(more);
                }
                // The key goes at the right edge, if there's room
                if let Some(hint) = &e.hint {
                    let n = hint.chars().count();
                    let gap = cols.saturating_sub(line.len + n + 1);
                    if gap >= 2 {
                        line.push("", gap);
                        let at = line.push(hint, 0);
                        line.color.push(at);
                    }
                }
            }
            Kind::Command(typed) => {
                line.push(typed, 0);
            }
        }
        line
    }

    fn on_panes(&mut self, manifest: PaneManifest) {
        self.pane_tab.clear();
        self.tab_focus.clear();
        self.own_tab = None;
        self.own_shown = false;
        for (tab, panes) in &manifest.panes {
            for p in panes {
                if p.is_plugin && p.id == self.own_id {
                    self.own_tab = Some(*tab);
                    self.own_shown = !p.is_suppressed;
                    self.own_floating = p.is_floating;
                }
                if !p.is_plugin {
                    self.pane_tab.insert(p.id, *tab);
                }
                if p.is_focused && !p.is_suppressed {
                    let focus = self.tab_focus.entry(*tab).or_default();
                    match (p.is_floating, p.is_plugin) {
                        (true, true) => focus.floating = Some(Floating::Plugin),
                        (true, false) => focus.floating = Some(Floating::Terminal(p.id)),
                        (false, false) => focus.tiled = Some(p.id),
                        (false, true) => {}
                    }
                }
            }
        }
        let live: HashSet<u32> = self.pane_tab.keys().copied().collect();
        if self.tracker.retain_panes(&live) {
            self.dirty = true;
        }
    }

    fn floating_visible(&self, tab: usize) -> bool {
        self.tabs
            .iter()
            .any(|t| t.position == tab && t.are_floating_panes_visible)
    }

    fn own_visible(&self) -> bool {
        self.own_shown
            && self
                .own_tab
                .is_some_and(|t| !self.own_floating || self.floating_visible(t))
    }

    /// The terminal pane the user is looking at in a tab. A floating pane only
    /// has focus while the tab's floating panes are showing.
    fn focused_terminal(&self, tab: usize) -> Option<u32> {
        let focus = self.tab_focus.get(&tab)?;
        match &focus.floating {
            Some(Floating::Terminal(id)) if self.floating_visible(tab) => Some(*id),
            Some(Floating::Plugin) if self.floating_visible(tab) => None,
            _ => focus.tiled,
        }
    }

    /// The pane the user is looking at counts as seen.
    fn mark_focused_seen(&mut self) {
        if !self.panes_current {
            return;
        }
        let Some(pane) = self.active_tab.and_then(|tab| self.focused_terminal(tab)) else {
            return;
        };
        if self.tracker.mark_seen(pane) {
            self.dirty = true;
        }
    }

    /// Rename tabs so each starts with its sessions' icons. Idempotent: the
    /// tab's own name comes from what we recorded, so a rename by the user
    /// becomes the new base and a reload picks up where it left off.
    fn sync_tab_names(&mut self) {
        if self.renaming_tab || self.tabs.is_empty() {
            return;
        }
        if !self.tab_markers {
            return self.clear_tab_names();
        }
        if !self.panes_current {
            return;
        }
        let list = self.tracker.sorted(|p| self.pane_tab.get(&p).copied());
        let mut changed = false;
        let mut renamed = false;
        for tab in &self.tabs {
            let badges: Vec<Badge> = list
                .iter()
                .filter(|s| self.pane_tab.get(&s.pane) == Some(&tab.position))
                .map(|s| s.badge())
                .collect();
            let base = self.tab_names.base(tab.tab_id, &tab.name);
            let mut frames = vec![tab_name(&base, &badges, self.icons, true)];
            let unlit = tab_name(&base, &badges, self.icons, false);
            if unlit != frames[0] {
                frames.push(unlit);
            }
            changed |= self.tab_names.wrote(tab.tab_id, &base, &frames);
            let want = if self.lit {
                &frames[0]
            } else {
                frames.last().unwrap()
            };
            if self.tab_names.request(tab.tab_id, &tab.name, want) {
                rename_tab_with_id(tab.tab_id as u64, want.clone());
                renamed = true;
            }
        }
        let live: HashSet<usize> = self.tabs.iter().map(|t| t.tab_id).collect();
        changed |= self.tab_names.retain(|id| live.contains(&id));
        self.tab_names.settle();
        self.dirty |= changed;
        // Save now: a reload before the next tick would take the new name,
        // missing from the saved names, for one the user typed
        if renamed {
            self.persist();
        }
    }

    /// With tab markers off, give tabs we marked their own names back, once.
    fn clear_tab_names(&mut self) {
        if self.tab_names.is_empty() {
            return;
        }
        for tab in &self.tabs {
            if let Some(base) = self.tab_names.forget(tab.tab_id, &tab.name) {
                if base != tab.name {
                    rename_tab_with_id(tab.tab_id as u64, base);
                }
            }
        }
        // Whatever's left is for tabs that closed
        self.tab_names = TabNames::default();
        self.dirty = true;
    }

    fn on_key(&mut self, key: KeyWithModifier) -> bool {
        let listing = self.list();
        let list: Vec<Pick> = listing.items.iter().map(|i| i.pick).collect();
        let current = self.current(&listing);
        drop(listing);
        let idx = current.and_then(|sel| list.iter().position(|&p| p == sel));
        let ctrl = key.has_only_modifiers(&[KeyModifier::Ctrl]);
        let typing = key.has_no_modifiers() || key.has_only_modifiers(&[KeyModifier::Shift]);
        match key.bare_key {
            BareKey::Down if key.has_no_modifiers() => self.step(&list, idx, 1),
            BareKey::Char('n' | 'j') if ctrl => self.step(&list, idx, 1),
            BareKey::Up if key.has_no_modifiers() => {
                self.step(&list, idx, list.len().saturating_sub(1))
            }
            BareKey::Char('p' | 'k') if ctrl => self.step(&list, idx, list.len().saturating_sub(1)),
            BareKey::Enter => current.is_some_and(|pick| self.choose(pick)),
            // Edit an action's command line, to add arguments
            BareKey::Tab if key.has_no_modifiers() => match current {
                Some(Pick::Action(i)) => {
                    self.set_query(format!(">{} ", self.catalog.entries[i].command()))
                }
                _ => false,
            },
            BareKey::Esc if !self.query.is_empty() => self.set_query(String::new()),
            BareKey::Esc => {
                self.hide();
                false
            }
            BareKey::Backspace => {
                let mut query = self.query.clone();
                query.pop().is_some() && self.set_query(query)
            }
            BareKey::Char('u') if ctrl => !self.query.is_empty() && self.set_query(String::new()),
            BareKey::Char('w') if ctrl => {
                let kept = self.query.trim_end().rfind(' ').map_or(0, |i| i + 1);
                kept < self.query.len() && self.set_query(self.query[..kept].to_string())
            }
            BareKey::Char(c) if typing => self.set_query(format!("{}{c}", self.query)),
            _ => false,
        }
    }

    /// Act on a row. Returns whether to render, which is only when the
    /// dashboard stays open.
    fn choose(&mut self, pick: Pick) -> bool {
        match pick {
            Pick::Session(pane) => {
                self.hide();
                focus_terminal_pane(pane, false, false);
                false
            }
            Pick::Tab(id) => {
                if let Some(t) = self.tabs.iter().find(|t| t.tab_id == id) {
                    let position = t.position as u32;
                    self.hide();
                    switch_tab_to(position + 1);
                }
                false
            }
            Pick::Action(i) => {
                let e = &self.catalog.entries[i];
                if e.needs_input {
                    return self.set_query(format!(">{} ", e.command()));
                }
                let (name, rest) = (e.name.clone(), e.args.join(" "));
                self.run(&name, &rest)
            }
            Pick::Command => {
                let Query::Command { name, rest } = parse_query(&self.query, &self.catalog) else {
                    return false;
                };
                let (name, rest) = (name.to_string(), rest.to_string());
                self.run(&name, &rest)
            }
        }
    }

    /// Run a command line on the pane the dashboard opened over, or show why
    /// it can't run.
    fn run(&mut self, name: &str, rest: &str) -> bool {
        let target = self.target;
        // Only some actions ask, so fetch the pane's directory lazily
        let cwd = Box::new(move || {
            target
                .pane
                .and_then(|p| get_pane_cwd(PaneId::Terminal(p)).ok())
                .unwrap_or_else(|| PathBuf::from("/"))
        });
        let actions = match self.catalog.actions(name, rest, target, cwd) {
            Ok(actions) => actions,
            Err(e) => {
                self.error = Some(e);
                return true;
            }
        };
        if actions.iter().any(|a| dispatch(a) == Dispatch::Unsupported) {
            self.error = Some(format!("Zellij doesn't let plugins run {name} like this"));
            return true;
        }
        if let Some(i) = self.catalog.find(name, rest) {
            let command = self.catalog.entries[i].command();
            self.palette.recent.retain(|c| *c != command);
            self.palette.recent.insert(0, command);
            self.palette.recent.truncate(RECENT_KEPT);
            self.dirty = true;
        }
        self.hide();
        // Actions run on the focused pane, and hiding doesn't always hand
        // focus back to the one the dashboard opened over
        if let Some(pane) = target.pane {
            focus_terminal_pane(pane, false, false);
        }
        for action in actions {
            self.perform(action);
        }
        false
    }

    /// Move the selection `by` rows, wrapping around.
    fn step(&mut self, list: &[Pick], idx: Option<usize>, by: usize) -> bool {
        if list.is_empty() {
            return false;
        }
        let next = idx.map_or(0, |i| (i + by) % list.len());
        self.selected = Some(list[next]);
        true
    }

    /// Changing the query puts the selection back on the best match.
    fn set_query(&mut self, query: String) -> bool {
        self.query = query;
        self.selected = None;
        self.error = None;
        true
    }

    /// Hide the dashboard, starting the next search fresh.
    fn hide(&mut self) {
        self.set_query(String::new());
        hide_self();
    }

    fn toggle(&mut self) {
        self.toggled = true;
        let Some(active) = self.active_tab else {
            self.target = Target::default();
            return show_self(true);
        };
        if self.own_tab == Some(active) && self.own_visible() {
            return self.hide();
        }
        self.target = Target {
            pane: self.focused_terminal(active),
            tab: self
                .tabs
                .iter()
                .find(|t| t.position == active)
                .map(|t| t.tab_id),
        };
        match self.own_tab {
            // show_self would jump to the pane's old tab, so bring it here
            // instead. break_panes_to_tab_with_index can't: Zellij mixes up tab
            // positions and ids there, and drops the pane once a tab has closed.
            Some(tab) if tab != active => {
                match RunPluginOrAlias::from_url(ALIAS, &None, None, None) {
                    Ok(plugin) => run_action(
                        Action::LaunchOrFocusPlugin {
                            plugin,
                            should_float: true,
                            move_to_focused_tab: true,
                            should_open_in_place: false,
                            close_replaced_pane: false,
                            skip_cache: false,
                            tab_id: None,
                        },
                        BTreeMap::new(),
                    ),
                    Err(e) => eprintln!("mission-control: bad alias {ALIAS}: {e}"),
                }
            }
            _ => show_self(true),
        }
    }

    /// Hand an action to Zellij, through `run_action` or the plugin command that
    /// does the same. `dispatch` says which.
    fn perform(&self, action: Action) {
        match dispatch(&action) {
            Dispatch::RunAction => run_action(action, BTreeMap::new()),
            Dispatch::Command => match action {
                Action::Paste {
                    chars,
                    pane_id: Some(pane_id),
                } => write_to_pane_id(format!("\x1b[200~{chars}\x1b[201~").into_bytes(), pane_id),
                Action::RenamePaneByPaneId {
                    pane_id: Some(pane_id),
                    name,
                } => rename_pane_with_id(pane_id, String::from_utf8_lossy(&name)),
                Action::FocusPaneByPaneId { pane_id } => focus_pane_with_id(pane_id, false, false),
                Action::GoToTabById { id } => {
                    if let Some(t) = self.tabs.iter().find(|t| t.tab_id as u64 == id) {
                        switch_tab_to(t.position as u32 + 1);
                    }
                }
                Action::CloseTabById { id } => close_tab_with_id(id),
                Action::RenameTabById { id, name } => rename_tab_with_id(id, name),
                Action::SaveSession => {
                    if let Err(e) = save_session() {
                        eprintln!("mission-control: can't save the session: {e}");
                    }
                }
                Action::StackPanes { pane_ids } => stack_panes(pane_ids),
                Action::ChangeFloatingPaneCoordinates {
                    pane_id,
                    coordinates,
                } => change_floating_panes_coordinates(vec![(pane_id, coordinates)]),
                Action::TogglePaneBorderless { pane_id } => toggle_pane_borderless(pane_id),
                Action::SetPaneBorderless {
                    pane_id,
                    borderless,
                } => set_pane_borderless(pane_id, borderless),
                Action::SwitchSession {
                    name,
                    tab_position,
                    pane_id,
                    ..
                } => switch_session_with_focus(&name, tab_position, pane_id),
                Action::SetPaneColor { pane_id, fg, bg } => set_pane_color(pane_id, fg, bg),
                _ => {}
            },
            Dispatch::Unsupported => {}
        }
    }

    fn send_alert(&self, alert: &Alert) {
        if !self.notify {
            return;
        }
        // The event, which anything local can send, picks the pane, Claude's
        // or not. Its session comes from the event too, so there's nothing to
        // check the pane against. Zellij picks the process, though, so the
        // write can only reach a pane's pty, where Zellij reads it, not the
        // program in the pane
        let pid = match get_pane_pid(PaneId::Terminal(alert.pane)) {
            Ok(pid) => pid.to_string(),
            Err(e) => {
                eprintln!("mission-control: no process for pane {}: {e}", alert.pane);
                return;
            }
        };
        run_command(
            &[
                "sh",
                "-c",
                NOTIFY_SH,
                "mission-control-notify",
                &pid,
                &strip_controls(&alert.title),
                &strip_controls(&alert.message),
            ],
            BTreeMap::new(),
        );
    }
}
