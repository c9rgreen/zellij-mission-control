//! The command palette's Zellij actions. They come from Zellij's own
//! `zellij action` CLI, so the palette offers every action and follows Zellij
//! upgrades. Free of zellij-tile so it can be unit tested natively.

use clap::{Arg, ColorChoice, Command, FromArgMatches, Subcommand};
use std::path::PathBuf;
use std::rc::Rc;
use zellij_utils::cli::CliAction;
use zellij_utils::data::{InputMode, KeybindsVec};
use zellij_utils::input::actions::Action;
use zellij_utils::plugin_api::action::ProtobufAction;

/// Subcommands a plugin can't use: they print to the CLI's stdout, can't be
/// sent from a plugin, or (send-keys) only encode keys in native builds.
const SKIP: &[&str] = &[
    "are-floating-panes-visible",
    "current-tab-info",
    "dump-layout",
    "list-clients",
    "list-panes",
    "list-tabs",
    "pipe",
    "query-tab-names",
    "send-keys",
];

/// Subcommands whose --pane-id or --tab-id means something other than "act on
/// this pane or tab".
const NO_TARGET: &[&str] = &["edit", "launch-or-focus-plugin", "launch-plugin", "new-pane", "switch-session"];

/// Common flag combinations worth their own entries.
const VARIANTS: &[(&str, &[&str])] = &[(
    "new-pane",
    &["--direction right", "--direction down", "--floating", "--stacked", "--in-place"],
)];

/// The pane and tab focused when the palette opened, which actions run on
/// rather than on the palette's own pane.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Target {
    pub pane: Option<u32>,
    pub tab: Option<usize>,
}

/// One action, possibly with some of its arguments filled in.
#[derive(Debug, Clone)]
pub struct Entry {
    /// The subcommand, eg. "move-focus".
    pub name: String,
    /// The arguments it fills in, eg. ["left"].
    pub args: Vec<String>,
    /// What's shown and searched, eg. "move focus left".
    pub label: String,
    pub about: String,
    /// A required argument is left for the user to type.
    pub needs_input: bool,
    /// The key that runs it, from the user's keybindings.
    pub hint: Option<String>,
    /// What it runs when nothing is targeted, to match against keybindings.
    plain: Vec<Action>,
}

impl Entry {
    /// The command line it stands for, eg. "move-focus left".
    pub fn command(&self) -> String {
        std::iter::once(self.name.as_str())
            .chain(self.args.iter().map(String::as_str))
            .collect::<Vec<_>>()
            .join(" ")
    }
}

/// How an action reaches Zellij from a plugin.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dispatch {
    /// Through `run_action`.
    RunAction,
    /// `run_action` can't carry it, but a plugin command does the same.
    Command,
    Unsupported,
}

pub fn dispatch(action: &Action) -> Dispatch {
    if ProtobufAction::try_from(action.clone()).is_ok() {
        return Dispatch::RunAction;
    }
    match action {
        Action::Paste { pane_id: Some(_), .. } | Action::RenamePaneByPaneId { pane_id: Some(_), .. } => Dispatch::Command,
        Action::FocusPaneByPaneId { .. }
        | Action::GoToTabById { .. }
        | Action::CloseTabById { .. }
        | Action::RenameTabById { .. }
        | Action::SaveSession
        | Action::StackPanes { .. }
        | Action::ChangeFloatingPaneCoordinates { .. }
        | Action::TogglePaneBorderless { .. }
        | Action::SetPaneBorderless { .. }
        | Action::SwitchSession { .. }
        | Action::SetPaneColor { .. } => Dispatch::Command,
        _ => Dispatch::Unsupported,
    }
}

pub struct Catalog {
    /// `zellij action`, to parse command lines with.
    cli: Command,
    pub entries: Vec<Entry>,
    /// The keybindings and base mode the hints came from, to skip
    /// recomputing them.
    keybinds: Option<(KeybindsVec, InputMode)>,
}

impl Default for Catalog {
    fn default() -> Self {
        Self::new()
    }
}

impl Catalog {
    pub fn new() -> Self {
        let cli = CliAction::augment_subcommands(
            Command::new("action")
                .no_binary_name(true)
                .subcommand_required(true)
                .color(ColorChoice::Never),
        );
        let mut catalog = Catalog {
            cli,
            entries: Vec::new(),
            keybinds: None,
        };
        let mut entries = Vec::new();
        for sub in catalog.cli.get_subcommands() {
            let name = sub.get_name();
            if SKIP.contains(&name) {
                continue;
            }
            let about = sub
                .get_about()
                .map(|a| a.to_string())
                .and_then(|a| a.lines().next().map(|l| l.trim().trim_end_matches('.').to_string()))
                .unwrap_or_default();
            for (args, needs_input) in expand(sub) {
                // Some need one of several flags, which only a parse finds
                let plain = match needs_input {
                    true => None,
                    false => catalog.actions_for(name, args.clone(), Target::default(), Box::new(|| "/".into())).ok(),
                };
                let needs_input = plain.is_none();
                let plain = plain.unwrap_or_default();
                entries.push(Entry {
                    name: name.to_string(),
                    label: label(name, &args),
                    args,
                    about: about.clone(),
                    needs_input,
                    hint: None,
                    plain,
                });
            }
        }
        catalog.entries = entries;
        catalog
    }

    /// An action the palette offers, by its subcommand name.
    pub fn is_command(&self, name: &str) -> bool {
        !SKIP.contains(&name) && self.subcommand(name).is_some()
    }

    /// The entry for a command line, or failing that for its bare subcommand.
    pub fn find(&self, name: &str, rest: &str) -> Option<usize> {
        let full = format!("{name} {}", rest.trim());
        let full = full.trim_end();
        self.entries
            .iter()
            .position(|e| e.command() == full)
            .or_else(|| self.entries.iter().position(|e| e.name == name && e.args.is_empty()))
    }

    pub fn usage(&self, name: &str) -> String {
        let Some(sub) = self.subcommand(name) else { return String::new() };
        let usage = sub.clone().render_usage().to_string();
        usage.strip_prefix("Usage: ").unwrap_or(&usage).trim().to_string()
    }

    /// The actions a command line runs, eg. ("rename-tab", "api server").
    ///
    /// Actions run on the target pane or tab when Zellij lets a plugin send
    /// that form; otherwise they run on focus, which the caller moves there.
    pub fn actions(
        &self,
        name: &str,
        rest: &str,
        target: Target,
        cwd: Box<dyn Fn() -> PathBuf>,
    ) -> Result<Vec<Action>, String> {
        let words = split_words(rest)?;
        let cwd: Rc<dyn Fn() -> PathBuf> = Rc::from(cwd);
        let run = |target_all: bool| -> Result<Vec<Action>, String> {
            let cli = match self.parse(name, words.clone(), target, target_all) {
                Ok(cli) => cli,
                Err(e) => {
                    // A lone free-text argument can be typed without quotes, eg.
                    // a tab name with spaces
                    let lone = self
                        .subcommand(name)
                        .is_some_and(|sub| sub.get_arguments().filter(|a| a.is_positional()).count() == 1);
                    if !lone || words.len() < 2 || words.iter().any(|w| w.starts_with('-')) {
                        return Err(e);
                    }
                    self.parse(name, vec![words.join(" ")], target, target_all).map_err(|_| e)?
                }
            };
            let cwd = cwd.clone();
            Action::actions_from_cli(cli, Box::new(move || cwd()), None)
        };
        match run(true) {
            Ok(actions) if actions.iter().all(|a| dispatch(a) != Dispatch::Unsupported) => Ok(actions),
            _ => run(false),
        }
    }

    fn actions_for(
        &self,
        name: &str,
        words: Vec<String>,
        target: Target,
        cwd: Box<dyn Fn() -> PathBuf>,
    ) -> Result<Vec<Action>, String> {
        Action::actions_from_cli(self.parse(name, words, target, false)?, cwd, None)
    }

    fn subcommand(&self, name: &str) -> Option<&Command> {
        self.cli.get_subcommands().find(|s| s.get_name() == name)
    }

    /// Parse a command line, giving it the target's --pane-id and --tab-id
    /// where they're required, or with `target_all` wherever they're taken.
    fn parse(&self, name: &str, words: Vec<String>, target: Target, target_all: bool) -> Result<CliAction, String> {
        let sub = self.subcommand(name).ok_or_else(|| format!("no action named {name}"))?;
        let mut argv = vec![name.to_string()];
        let targets = [("pane_id", target.pane.map(|p| format!("terminal_{p}"))), ("tab_id", target.tab.map(|t| t.to_string()))];
        for (id, value) in targets {
            let Some(value) = value else { continue };
            let Some(arg) = sub.get_arguments().find(|a| a.get_id() == id && !a.is_positional()) else { continue };
            let wanted = arg.is_required_set() || (target_all && !NO_TARGET.contains(&name));
            // Flags go first, ahead of any trailing command
            if let (Some(long), true, false) = (arg.get_long(), wanted, given(arg, &words)) {
                argv.extend([format!("--{long}"), value]);
            }
        }
        argv.extend(words);
        let matches = self.cli.clone().try_get_matches_from(argv).map_err(clap_error)?;
        CliAction::from_arg_matches(&matches).map_err(clap_error)
    }

    /// Look up each entry's keys: the shortest sequence from the base mode,
    /// counting the keys that switch into other modes on the way.
    pub fn set_keybinds(&mut self, keybinds: &KeybindsVec, base: Option<InputMode>) -> bool {
        let base = base.unwrap_or(InputMode::Normal);
        if self.keybinds.as_ref().is_some_and(|(k, b)| k == keybinds && *b == base) {
            return false;
        }
        let in_mode = |mode: InputMode| {
            keybinds
                .iter()
                .filter(move |(m, _)| *m == mode)
                .flat_map(|(_, binds)| binds.iter())
        };
        // The keys that reach each mode from the base mode, fewest first
        let mut paths: Vec<(InputMode, Vec<String>)> = vec![(base, Vec::new())];
        let mut i = 0;
        while i < paths.len() {
            let (mode, path) = paths[i].clone();
            for (key, acts) in in_mode(mode) {
                if let [Action::SwitchToMode { input_mode }] = acts.as_slice() {
                    if !paths.iter().any(|(m, _)| m == input_mode) {
                        let mut path = path.clone();
                        path.push(key.to_string());
                        paths.push((*input_mode, path));
                    }
                }
            }
            i += 1;
        }
        for e in &mut self.entries {
            e.hint = None;
            if e.plain.is_empty() {
                continue;
            }
            e.hint = paths.iter().find_map(|(mode, path)| {
                let (key, _) = in_mode(*mode).find(|(_, acts)| runs(acts, &e.plain))?;
                Some(path.iter().cloned().chain([key.to_string()]).collect::<Vec<_>>().join(", "))
            });
        }
        self.keybinds = Some((keybinds.clone(), base));
        true
    }
}

/// The argument lists to make entries from: one per known value of each
/// positional argument, or one left for the user to type. Bool says whether
/// a required argument is left.
fn expand(sub: &Command) -> Vec<(Vec<String>, bool)> {
    let mut combos = vec![(Vec::new(), false)];
    for arg in sub.get_arguments().filter(|a| a.is_positional()) {
        let values = known_values(sub.get_name(), arg);
        let required = arg.is_required_set();
        combos = combos
            .into_iter()
            .flat_map(|(args, needs)| {
                if needs || values.is_empty() {
                    return vec![(args, needs || required)];
                }
                let mut out = if required { Vec::new() } else { vec![(args.clone(), false)] };
                out.extend(values.iter().map(|v| {
                    let mut a = args.clone();
                    a.push(v.clone());
                    (a, false)
                }));
                out
            })
            .collect();
    }
    for (_, flags) in VARIANTS.iter().filter(|(name, _)| *name == sub.get_name()) {
        combos.extend(flags.iter().map(|f| (f.split(' ').map(String::from).collect(), false)));
    }
    combos
}

/// Values a positional argument can take, where they're few enough to list.
fn known_values(sub: &str, arg: &Arg) -> Vec<String> {
    let values: Vec<String> = arg
        .get_possible_values()
        .iter()
        .filter(|v| !v.is_hide_set())
        .map(|v| v.get_name().to_string())
        .collect();
    if !values.is_empty() {
        return values;
    }
    // These parse from strings, so clap doesn't know their values
    let list: &[&str] = match (sub, arg.get_id().as_str()) {
        ("move-tab", "direction") => &["left", "right"],
        (_, "direction") => &["left", "right", "up", "down"],
        ("resize", "resize") => &["increase", "decrease"],
        _ => &[],
    };
    list.iter().map(|v| v.to_string()).collect()
}

/// "new-pane --direction right" reads as "new pane right".
fn label(name: &str, args: &[String]) -> String {
    std::iter::once(name)
        .chain(args.iter().map(String::as_str).filter(|a| *a != "--direction"))
        .map(|w| w.trim_start_matches('-').replace('-', " "))
        .collect::<Vec<_>>()
        .join(" ")
}

/// The user typed this flag themselves.
fn given(arg: &Arg, words: &[String]) -> bool {
    words.iter().any(|w| {
        arg.get_long().is_some_and(|l| w.strip_prefix("--").is_some_and(|f| f == l || f.starts_with(&format!("{l}="))))
            || arg.get_short().is_some_and(|s| *w == format!("-{s}"))
    })
}

/// A keybinding's actions do what an entry does. Bindings often switch back
/// to the base mode afterwards, which doesn't count.
fn runs(bound: &[Action], plain: &[Action]) -> bool {
    let switches = |a: &&Action| matches!(a, Action::SwitchToMode { .. });
    let bound: Vec<&Action> = match plain.iter().any(|a| switches(&a)) {
        true => bound.iter().collect(),
        false => bound.iter().filter(|a| !switches(a)).collect(),
    };
    bound.len() == plain.len() && bound.iter().zip(plain).all(|(b, p)| b.shallow_eq(p))
}

/// clap's message on one line, without its "error: " prefix and usage tail.
fn clap_error(e: clap::Error) -> String {
    let text = e.render().to_string();
    let message = text.split("\n\n").next().unwrap_or_default();
    let message = message.split_whitespace().collect::<Vec<_>>().join(" ");
    message.strip_prefix("error: ").unwrap_or(&message).to_string()
}

/// Split a command line into words, shell style: quotes group words, and a
/// backslash escapes the next char.
pub fn split_words(s: &str) -> Result<Vec<String>, String> {
    let mut words = Vec::new();
    let mut word = String::new();
    let mut in_word = false;
    let mut quote: Option<char> = None;
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (None | Some('"'), '\\') => {
                word.extend(chars.next());
                in_word = true;
            }
            (Some(_), c) => word.push(c),
            (None, '"' | '\'') => {
                quote = Some(c);
                in_word = true;
            }
            (None, c) if c.is_whitespace() => {
                if std::mem::take(&mut in_word) {
                    words.push(std::mem::take(&mut word));
                }
            }
            (None, c) => {
                word.push(c);
                in_word = true;
            }
        }
    }
    if quote.is_some() {
        return Err("unclosed quote".into());
    }
    if in_word {
        words.push(word);
    }
    Ok(words)
}

/// What the palette's query asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    All,
    Sessions,
    Tabs,
    Actions,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Query<'a> {
    /// Fuzzy search.
    Search(Scope, &'a str),
    /// A command line: ">" then an action's name, a space, and its arguments.
    Command { name: &'a str, rest: &'a str },
}

/// "@" searches sessions, "#" tabs and ">" actions; anything else searches all.
pub fn parse_query<'a>(query: &'a str, catalog: &Catalog) -> Query<'a> {
    let (scope, text) = match query.chars().next() {
        Some('@') => (Scope::Sessions, &query[1..]),
        Some('#') => (Scope::Tabs, &query[1..]),
        Some('>') => (Scope::Actions, &query[1..]),
        _ => (Scope::All, query),
    };
    if scope == Scope::Actions {
        if let Some((name, rest)) = text.trim_start().split_once(char::is_whitespace) {
            if catalog.is_command(name) {
                return Query::Command { name, rest };
            }
        }
    }
    Query::Search(scope, text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use zellij_utils::data::{BareKey, KeyModifier, KeyWithModifier, PaneId};

    fn entry<'a>(c: &'a Catalog, command: &str) -> &'a Entry {
        c.entries.iter().find(|e| e.command() == command).unwrap_or_else(|| panic!("no entry {command}"))
    }

    fn run(c: &Catalog, name: &str, rest: &str, target: Target) -> Result<Vec<Action>, String> {
        c.actions(name, rest, target, Box::new(|| "/".into()))
    }

    #[test]
    fn covers_the_cli_and_expands_values() {
        let c = Catalog::new();
        assert!(c.is_command("toggle-fullscreen"));
        assert!(!c.is_command("list-panes"));
        assert!(!c.entries.iter().any(|e| SKIP.contains(&e.name.as_str())));
        assert_eq!(entry(&c, "move-focus left").label, "move focus left");
        // A required argument with known values only comes filled in
        assert!(!c.entries.iter().any(|e| e.name == "move-focus" && e.args.is_empty()));
        // An optional one also comes bare
        assert!(entry(&c, "move-pane").args.is_empty());
        assert!(entry(&c, "resize increase").args.len() == 1);
        assert!(entry(&c, "resize decrease up").args.len() == 2);
        assert!(entry(&c, "switch-mode locked").plain == vec![Action::SwitchToMode { input_mode: InputMode::Locked }]);
        assert_eq!(entry(&c, "new-pane --direction right").label, "new pane right");
        assert!(entry(&c, "rename-tab").needs_input);
        assert!(!entry(&c, "close-pane").needs_input);
        assert!(entry(&c, "override-layout").needs_input);
    }

    #[test]
    fn every_ready_entry_runs_from_a_plugin() {
        let c = Catalog::new();
        for e in c.entries.iter().filter(|e| !e.needs_input) {
            let actions = run(&c, &e.name, &e.args.join(" "), Target { pane: Some(3), tab: Some(1) }).unwrap_or_else(|err| panic!("{}: {err}", e.command()));
            for a in &actions {
                assert_ne!(dispatch(a), Dispatch::Unsupported, "{}: {a:?}", e.command());
            }
        }
    }

    #[test]
    fn targets_the_pane_and_tab_where_plugins_can() {
        let c = Catalog::new();
        let target = Target { pane: Some(7), tab: Some(2) };
        let none = Target::default();
        assert_eq!(run(&c, "toggle-pane-borderless", "", target).unwrap(), vec![Action::TogglePaneBorderless { pane_id: PaneId::Terminal(7) }]);
        assert_eq!(
            run(&c, "toggle-pane-borderless", "--pane-id terminal_9", target).unwrap(),
            vec![Action::TogglePaneBorderless { pane_id: PaneId::Terminal(9) }]
        );
        assert!(run(&c, "toggle-pane-borderless", "", none).is_err());
        assert_eq!(run(&c, "rename-tab", "api", target).unwrap(), vec![Action::RenameTabById { id: 2, name: "api".into() }]);
        assert_eq!(run(&c, "close-tab", "", target).unwrap(), vec![Action::CloseTabById { id: 2 }]);
        assert_eq!(
            run(&c, "rename-pane", "web", target).unwrap(),
            vec![Action::RenamePaneByPaneId { pane_id: Some(PaneId::Terminal(7)), name: b"web".to_vec() }]
        );
        // Plugins can't send the targeted form, so it runs on focus
        assert_eq!(run(&c, "close-pane", "", target).unwrap(), run(&c, "close-pane", "", none).unwrap());
        assert!(run(&c, "close-pane", "-p 7", none).unwrap().iter().all(|a| dispatch(a) == Dispatch::Unsupported));
        // new-pane's --pane-id needs --in-place, and means the pane to replace
        assert!(run(&c, "new-pane", "", target).unwrap().iter().all(|a| dispatch(a) == Dispatch::RunAction));
    }

    #[test]
    fn free_text_needs_no_quotes() {
        let c = Catalog::new();
        let target = Target { pane: None, tab: Some(1) };
        let quoted = run(&c, "rename-tab", "\"api server\"", target).unwrap();
        assert_eq!(quoted, vec![Action::RenameTabById { id: 1, name: "api server".into() }]);
        assert_eq!(run(&c, "rename-tab", "api server", target).unwrap(), quoted);
        assert_eq!(run(&c, "rename-tab", "", target).unwrap_err(), "the following required arguments were not provided: <NAME>");
        assert!(run(&c, "move-focus", "sideways", target).is_err());
    }

    #[test]
    fn hints_come_from_keybindings() {
        let mut c = Catalog::new();
        let key = |k: char, m: &[KeyModifier]| KeyWithModifier::new_with_modifiers(BareKey::Char(k), m.iter().copied().collect());
        let fullscreen = entry(&c, "toggle-fullscreen").plain.clone();
        let focus_left = entry(&c, "move-focus left").plain.clone();
        let mut pane_mode = focus_left.clone();
        pane_mode.push(Action::SwitchToMode { input_mode: InputMode::Normal });
        let keybinds: KeybindsVec = vec![
            (
                InputMode::Normal,
                vec![
                    (key('f', &[KeyModifier::Alt]), fullscreen),
                    (key('p', &[KeyModifier::Ctrl]), vec![Action::SwitchToMode { input_mode: InputMode::Pane }]),
                ],
            ),
            (InputMode::Pane, vec![(key('h', &[]), pane_mode)]),
        ];
        assert!(c.set_keybinds(&keybinds, Some(InputMode::Normal)));
        assert!(!c.set_keybinds(&keybinds, Some(InputMode::Normal)));
        assert_eq!(entry(&c, "toggle-fullscreen").hint.as_deref(), Some("Alt f"));
        assert_eq!(entry(&c, "move-focus left").hint.as_deref(), Some("Ctrl p, h"));
        assert_eq!(entry(&c, "switch-mode pane").hint.as_deref(), Some("Ctrl p"));
        assert_eq!(entry(&c, "close-pane").hint, None);
        // From locked, through normal
        let mut locked = keybinds.clone();
        locked.push((InputMode::Locked, vec![(key('g', &[KeyModifier::Ctrl]), vec![Action::SwitchToMode { input_mode: InputMode::Normal }])]));
        assert!(c.set_keybinds(&locked, Some(InputMode::Locked)));
        assert_eq!(entry(&c, "move-focus left").hint.as_deref(), Some("Ctrl g, Ctrl p, h"));
        assert_eq!(entry(&c, "toggle-fullscreen").hint.as_deref(), Some("Ctrl g, Alt f"));
    }

    #[test]
    fn queries_pick_a_scope_or_a_command_line() {
        let c = Catalog::new();
        assert_eq!(parse_query("api", &c), Query::Search(Scope::All, "api"));
        assert_eq!(parse_query("@api", &c), Query::Search(Scope::Sessions, "api"));
        assert_eq!(parse_query("#2", &c), Query::Search(Scope::Tabs, "2"));
        assert_eq!(parse_query(">rename", &c), Query::Search(Scope::Actions, "rename"));
        assert_eq!(parse_query(">rename-tab", &c), Query::Search(Scope::Actions, "rename-tab"));
        assert_eq!(parse_query(">rename-tab my tab", &c), Query::Command { name: "rename-tab", rest: "my tab" });
        assert_eq!(parse_query(">move-focus ", &c), Query::Command { name: "move-focus", rest: "" });
        assert_eq!(parse_query(">no such", &c), Query::Search(Scope::Actions, "no such"));
        assert_eq!(c.find("move-focus", "left "), Some(c.entries.iter().position(|e| e.command() == "move-focus left").unwrap()));
        assert_eq!(c.find("rename-tab", "api"), Some(c.entries.iter().position(|e| e.command() == "rename-tab").unwrap()));
        assert!(c.usage("rename-tab").contains("<NAME>"));
    }

    #[test]
    fn splits_words_like_a_shell() {
        let split = |s| split_words(s).unwrap();
        assert_eq!(split("  a  b "), ["a", "b"]);
        assert_eq!(split(r#"a "b c" 'd e' f\ g"#), ["a", "b c", "d e", "f g"]);
        assert_eq!(split(r#"x "" y"#), ["x", "", "y"]);
        assert!(split_words("\"open").is_err());
    }
}
