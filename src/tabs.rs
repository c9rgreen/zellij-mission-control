//! Icons, and the tab names they go in. Free of zellij-tile so it can be unit
//! tested natively.

use crate::state::Badge;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Which characters mark sessions. The plugin can't tell what the terminal's font
/// can draw, so this is configured.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum IconSet {
    /// Nerd Font (Font Awesome) glyphs.
    #[default]
    Nerd,
    /// Symbols most fonts have, without emoji forms that draw double width.
    Unicode,
    Ascii,
}

impl IconSet {
    pub fn parse(name: &str) -> Option<Self> {
        match name {
            "nerd" => Some(IconSet::Nerd),
            "unicode" => Some(IconSet::Unicode),
            "ascii" => Some(IconSet::Ascii),
            _ => None,
        }
    }

    /// The icon for a badge. Working blinks where the set can draw it; `lit`
    /// picks the frame, and the other badges have only the one.
    pub fn icon(self, badge: Badge, lit: bool) -> char {
        match (self, badge) {
            (IconSet::Nerd, Badge::NeedsInput) => '\u{f071}', // nf-fa-warning
            (IconSet::Nerd, Badge::ReadyUnseen) => '\u{f058}', // nf-fa-check_circle
            (IconSet::Nerd, Badge::Working) if lit => '\u{f192}', // nf-fa-circle_dot
            (IconSet::Nerd, Badge::Working) => '\u{f10c}',    // nf-fa-circle_o
            (IconSet::Nerd, Badge::ReadySeen) => '\u{f10c}',  // nf-fa-circle_o
            (IconSet::Unicode, Badge::NeedsInput) => '!',
            (IconSet::Unicode, Badge::ReadyUnseen) => '✓',
            (IconSet::Unicode, Badge::Working) => '◔',
            (IconSet::Unicode, Badge::ReadySeen) => '○',
            (IconSet::Ascii, Badge::NeedsInput) => '!',
            (IconSet::Ascii, Badge::ReadyUnseen) => '*',
            (IconSet::Ascii, Badge::Working) => '~',
            (IconSet::Ascii, Badge::ReadySeen) => '.',
        }
    }

    /// Marks a tab in the palette.
    pub fn tab(self) -> char {
        match self {
            IconSet::Nerd => '\u{f2d0}', // nf-fa-window_maximize
            IconSet::Unicode | IconSet::Ascii => '#',
        }
    }

    /// Marks a Zellij action in the palette.
    pub fn action(self) -> char {
        match self {
            IconSet::Nerd => '\u{f120}', // nf-fa-terminal
            IconSet::Unicode => '›',
            IconSet::Ascii => '>',
        }
    }
}

/// A tab name with icons in front for its sessions' badges, most urgent
/// first, in the `lit` frame of their blink.
pub fn tab_name(base: &str, badges: &[Badge], icons: IconSet, lit: bool) -> String {
    if badges.is_empty() {
        return base.to_string();
    }
    let marks: Vec<String> = badges
        .iter()
        .map(|b| icons.icon(*b, lit).to_string())
        .collect();
    format!("{} {base}", marks.join(" "))
}

/// Nerd Font icons the plugin has put in tab names, including the cog, robot,
/// spinner and user gear earlier versions used for Working.
const NERD_ICONS: [char; 8] = [
    '\u{f071}', '\u{f058}', '\u{f192}', '\u{f10c}', '\u{f013}', '\u{ee0d}', '\u{f110}', '\u{edc8}',
];

/// A name without the Nerd Font icons at its front, for tabs named before
/// the plugin kept records (or whose records were lost). Nobody types these glyphs, so stripping them is safe;
/// it also handles icons with no spaces between them, as older versions wrote.
fn strip_nerd_icons(name: &str) -> &str {
    let mut rest = name;
    while let Some(after) = rest.strip_prefix(NERD_ICONS) {
        // Only eat a space that separates two icons, not the one before the name
        rest = after
            .strip_prefix(' ')
            .filter(|next| next.starts_with(NERD_ICONS))
            .unwrap_or(after);
    }
    if rest.len() == name.len() {
        return name;
    }
    rest.strip_prefix(' ').unwrap_or(rest)
}

/// Each tab's own name and the names the plugin gave it, by tab id. Comparing a
/// tab's name to what the plugin wrote tells the plugin's icons from the user's text,
/// so any characters can be icons and a user's rename is kept as typed.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(transparent)]
pub struct TabNames {
    /// Ordered, so the saved JSON only changes when the records do.
    tabs: BTreeMap<usize, TabName>,
    /// Every tab has been recorded once, so an unrecorded tab is a new one
    /// the plugin never named, and its name is all its own.
    #[serde(skip)]
    settled: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct TabName {
    /// The tab's name without the plugin's icons.
    base: String,
    /// Names the plugin gave the tab, newest last. Renames apply asynchronously, so
    /// a TabUpdate can still carry an earlier one.
    written: Vec<String>,
    /// A name the plugin asked for that the tab hasn't shown yet.
    #[serde(skip)]
    pending: Option<String>,
}

/// How many recent names to recognize as the plugin's: a few states' worth, at
/// two frames each.
const KEEP_WRITTEN: usize = 16;

impl TabNames {
    /// The tab's own name, given what it's called now.
    pub fn base(&self, tab_id: usize, current: &str) -> String {
        match self.tabs.get(&tab_id) {
            Some(t) if t.written.iter().any(|w| w == current) => t.base.clone(),
            // The user renamed it; Zellij's rename mode starts from a blank name,
            // so all of it is theirs
            Some(_) => current.to_string(),
            None if self.settled => current.to_string(),
            None => strip_nerd_icons(current).to_string(),
        }
    }

    /// Record that the tab, whose own name is `base`, is now called one of
    /// `names`, the frames of its icons' blink. Returns whether the record changed.
    pub fn wrote(&mut self, tab_id: usize, base: &str, names: &[String]) -> bool {
        let mut changed = false;
        let t = self.tabs.entry(tab_id).or_insert_with(|| {
            changed = true;
            TabName {
                base: base.to_string(),
                written: Vec::new(),
                pending: None,
            }
        });
        if t.base != base {
            // Names from before a rename aren't the plugin's any more
            t.base = base.to_string();
            t.written.clear();
            changed = true;
        }
        // Blinking alternates between the frames; that's no change
        if t.written.ends_with(names) {
            return changed;
        }
        t.written.retain(|w| !names.contains(w));
        t.written.extend_from_slice(names);
        let over = t.written.len().saturating_sub(KEEP_WRITTEN);
        t.written.drain(..over);
        true
    }

    /// Whether to ask the tab, called `current`, to take `name`. Renames apply
    /// asynchronously, so ask once until the tab shows it, but ask again if the
    /// name later changes away from it.
    pub fn request(&mut self, tab_id: usize, current: &str, name: &str) -> bool {
        let Some(t) = self.tabs.get_mut(&tab_id) else {
            return current != name;
        };
        if t.pending.as_deref() == Some(current) {
            t.pending = None;
        }
        if current == name || t.pending.as_deref() == Some(name) {
            return false;
        }
        t.pending = Some(name.to_string());
        true
    }

    /// Forget names asked for and not yet seen, so they're asked for again
    /// if the tab doesn't show them. A rename can land unobserved, while
    /// names are left alone, and then be undone.
    pub fn clear_pending(&mut self) {
        for t in self.tabs.values_mut() {
            t.pending = None;
        }
    }

    /// Every current tab is recorded; see `settled`.
    pub fn settle(&mut self) {
        self.settled = true;
    }

    pub fn is_empty(&self) -> bool {
        self.tabs.is_empty()
    }

    /// Stop tracking the tab, returning its own name if it still has one the plugin gave it.
    pub fn forget(&mut self, tab_id: usize, current: &str) -> Option<String> {
        let t = self.tabs.remove(&tab_id)?;
        t.written.iter().any(|w| w == current).then_some(t.base)
    }

    /// Forget tabs that closed. Returns whether any were.
    pub fn retain(&mut self, live: impl Fn(usize) -> bool) -> bool {
        let before = self.tabs.len();
        self.tabs.retain(|id, _| live(*id));
        self.tabs.len() != before
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL: [Badge; 4] = [
        Badge::NeedsInput,
        Badge::ReadyUnseen,
        Badge::Working,
        Badge::ReadySeen,
    ];

    #[test]
    fn icons_per_set() {
        assert_eq!(
            tab_name("api", &ALL, IconSet::Nerd, true),
            "\u{f071} \u{f058} \u{f192} \u{f10c} api"
        );
        assert_eq!(tab_name("api", &ALL, IconSet::Unicode, true), "! ✓ ◔ ○ api");
        assert_eq!(tab_name("api", &ALL, IconSet::Ascii, true), "! * ~ . api");
        assert_eq!(tab_name("api", &[], IconSet::Ascii, true), "api");
        assert_eq!(IconSet::parse("unicode"), Some(IconSet::Unicode));
        assert_eq!(IconSet::parse("emoji"), None);
    }

    #[test]
    fn working_blinks_in_nerd_icons() {
        assert_eq!(
            tab_name("api", &[Badge::Working], IconSet::Nerd, false),
            "\u{f10c} api"
        );
        assert_eq!(tab_name("api", &ALL, IconSet::Ascii, false), "! * ~ . api");
        let frames =
            [true, false].map(|lit| tab_name("api", &[Badge::Working], IconSet::Nerd, lit));
        let mut t = TabNames::default();
        assert!(t.wrote(1, "api", &frames));
        // Both frames are the plugin's, and blinking between them changes nothing
        assert_eq!(t.base(1, &frames[0]), "api");
        assert_eq!(t.base(1, &frames[1]), "api");
        assert!(!t.wrote(1, "api", &frames));
    }

    #[test]
    fn tracks_base_through_plugin_renames() {
        let mut t = TabNames::default();
        let name = tab_name("api", &[Badge::Working], IconSet::Ascii, true);
        t.wrote(1, "api", std::slice::from_ref(&name));
        assert_eq!(t.base(1, &name), "api");
        // A slow rename: the tab still shows the plugin's previous name
        let next = tab_name("api", &[Badge::NeedsInput], IconSet::Ascii, true);
        t.wrote(1, "api", std::slice::from_ref(&next));
        assert_eq!(t.base(1, &name), "api");
        assert_eq!(t.base(1, &next), "api");
        // Switching icon sets: the old name is still recognized
        assert_eq!(t.base(1, "~ api"), "api");
    }

    #[test]
    fn keeps_user_renames_as_typed() {
        let mut t = TabNames::default();
        t.wrote(1, "api", &["! api".into()]);
        // Characters that look like icons stay when the user typed them
        assert_eq!(t.base(1, "! urgent"), "! urgent");
        assert_eq!(t.base(1, "* scratch"), "* scratch");
        t.wrote(2, "web", &["web".into()]);
        assert_eq!(t.base(2, "! web"), "! web");
        // A tab the plugin hasn't seen keeps everything but Nerd Font icons
        assert_eq!(t.base(3, "! todo"), "! todo");
        // Once the rename is recorded, older names aren't the plugin's
        t.wrote(1, "urgent", &["! urgent".into()]);
        assert_eq!(t.base(1, "! api"), "! api");
    }

    #[test]
    fn strips_nerd_icons_from_unrecorded_tabs() {
        let t = TabNames::default();
        assert_eq!(t.base(1, "\u{f071} \u{edc8} api"), "api");
        assert_eq!(t.base(1, "\u{f10c}\u{f013} api"), "api");
        assert_eq!(t.base(1, "\u{ee0d} \u{f110} \u{f10c} api"), "api");
        assert_eq!(t.base(1, "\u{f192} \u{f10c} api"), "api");
        assert_eq!(t.base(1, "\u{f071}  api"), " api");
        assert_eq!(t.base(1, " spaced"), " spaced");
        // Tabs opened after the first pass keep icons from a layout
        let mut t = TabNames::default();
        t.settle();
        assert_eq!(t.base(1, "\u{f013} config"), "\u{f013} config");
    }

    #[test]
    fn request_and_forget() {
        let mut t = TabNames::default();
        assert!(t.wrote(1, "api", &["api".into()]));
        assert!(t.wrote(1, "api", &["! api".into()]));
        assert!(!t.wrote(1, "api", &["! api".into()]));
        // Ask once while the tab still shows the old name
        assert!(t.request(1, "api", "! api"));
        assert!(!t.request(1, "api", "! api"));
        // Once it lands, ask again if the tab goes back to an old name
        assert!(!t.request(1, "! api", "! api"));
        assert!(t.request(1, "api", "! api"));
        // A rename that landed unseen and was undone is asked for again
        assert!(!t.request(1, "api", "! api"));
        t.clear_pending();
        assert!(t.request(1, "api", "! api"));
        // Markers off: hand back the own name only if the plugin's name is still on
        assert_eq!(t.forget(1, "api"), Some("api".into()));
        assert_eq!(t.forget(1, "api"), None);
        t.wrote(2, "web", &["! web".into()]);
        assert_eq!(t.forget(2, "renamed"), None);
    }

    #[test]
    fn forgets_closed_tabs_and_old_names() {
        let mut t = TabNames::default();
        for i in 0..=KEEP_WRITTEN {
            t.wrote(1, "api", &[format!("{i} api")]);
        }
        assert_eq!(t.base(1, "0 api"), "0 api");
        assert_eq!(t.base(1, "1 api"), "api");
        t.retain(|id| id != 1);
        assert_eq!(t.base(1, "1 api"), "1 api");
    }
}
