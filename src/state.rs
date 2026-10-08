//! Session tracking, kept free of zellij-tile so it can be unit tested natively.

use crate::tabs::TabNames;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashSet};
use zellij_utils::data::InputMode;

/// One Claude Code hook event, as forwarded by `hooks/mission-control-hook.sh`.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct HookEvent {
    pub event: String,
    pub pane: u32,
    pub cwd: String,
    pub tool: Option<String>,
    pub kind: Option<String>,
    pub message: Option<String>,
    /// What started the session, for SessionStart: startup, resume, clear or compact.
    pub source: Option<String>,
    /// Ties a tool's PreToolUse, PermissionRequest and PostToolUse together.
    pub tool_use_id: Option<String>,
    /// Unix time, fractional, when the hook ran. Each event is piped by its own
    /// process, so they can arrive out of order; 0 means unknown.
    pub at: f64,
    /// The subagent the event came from; None for the main thread.
    pub agent_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Status {
    /// Blocked on the user: a permission prompt or a question.
    NeedsInput(String),
    /// Finished its turn and waiting for the next prompt.
    Ready,
    /// Thinking, or running the named tool.
    Working(Option<String>),
}

impl Status {
    pub fn label(&self) -> String {
        match self {
            Status::NeedsInput(_) => "needs input".into(),
            Status::Ready => "ready".into(),
            Status::Working(Some(tool)) => tool.clone(),
            Status::Working(None) => "thinking".into(),
        }
    }
}

/// How a session is shown: its status, with Ready split by whether the user
/// has seen it. Declared most urgent first, which is the sort order. Each
/// `IconSet` draws these.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Badge {
    NeedsInput,
    ReadyUnseen,
    Working,
    ReadySeen,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    pub pane: u32,
    pub cwd: String,
    pub status: Status,
    /// Unix time of the last status change.
    pub since: u64,
    /// The user has focused the pane since its status last changed.
    pub seen: bool,
    /// While NeedsInput: the prompt it's blocked on.
    #[serde(default)]
    prompt: Option<Prompt>,
    /// Tool calls started and not yet finished, oldest first.
    #[serde(default)]
    calls: Vec<Call>,
    /// `at` of the newest turn boundary: a prompt, Stop or session start.
    #[serde(default)]
    boundary_at: f64,
    /// `at` of the newest event of any kind.
    #[serde(default)]
    latest_at: f64,
}

impl Session {
    pub fn project(&self) -> &str {
        self.cwd.rsplit('/').find(|s| !s.is_empty()).unwrap_or("?")
    }

    pub fn badge(&self) -> Badge {
        match self.status {
            Status::NeedsInput(_) => Badge::NeedsInput,
            Status::Ready if !self.seen => Badge::ReadyUnseen,
            Status::Working(_) => Badge::Working,
            Status::Ready => Badge::ReadySeen,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Call {
    id: String,
    tool: String,
    #[serde(default)]
    agent: Option<String>,
}

/// What a NeedsInput session is blocked on, as far as its events tell.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Prompt {
    kind: PromptKind,
    /// The tool call whose completion answers it.
    call: Option<String>,
    tool: Option<String>,
    /// The subagent that asked; None for the main thread.
    agent: Option<String>,
    /// `at` of the event that raised it.
    at: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
enum PromptKind {
    Permission,
    /// AskUserQuestion or ExitPlanMode, which wait on the user by design.
    Question,
    /// An MCP server's form or URL.
    Elicitation,
    /// A Notification of kind agent_needs_input.
    Agent,
}

impl Prompt {
    /// Whether the two could be one dialog. A permission prompt fires both
    /// PermissionRequest and Notification, and ExitPlanMode fires PreToolUse
    /// then PermissionRequest; neither pair should alert twice.
    fn same(&self, other: &Prompt) -> bool {
        if let (Some(a), Some(b)) = (&self.call, &other.call) {
            return a == b;
        }
        let kinds = self.kind == other.kind
            || matches!(
                (self.kind, other.kind),
                (PromptKind::Permission, PromptKind::Question)
                    | (PromptKind::Question, PromptKind::Permission)
            );
        kinds
            && match (&self.tool, &other.tool) {
                (Some(a), Some(b)) => tools_match(a, b),
                _ => true,
            }
    }
}

/// Tool names match loosely: a Notification may name an MCP tool by its
/// server rather than its full name.
fn tools_match(a: &str, b: &str) -> bool {
    a.contains(b) || b.contains(a)
}

/// The tool a permission Notification names, as in "Claude needs your
/// permission to use Bash".
fn tool_in_message(message: &str) -> Option<String> {
    let rest = message.split_once("permission to use ")?.1;
    let tool = rest
        .split_whitespace()
        .next()?
        .trim_end_matches(|c: char| !c.is_alphanumeric() && c != '_');
    (!tool.is_empty()).then(|| tool.to_string())
}

/// Whether an event at `at` came after one at `than`; unknown times count as later.
fn newer(at: f64, than: f64) -> bool {
    at <= 0.0 || than <= 0.0 || at > than
}

/// A status change the user should hear about.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Alert {
    pub pane: u32,
    pub title: String,
    pub message: String,
}

#[derive(Debug, Default)]
pub struct Tracker {
    sessions: BTreeMap<u32, Session>,
    /// When each pane's last session ended, so late events don't revive it.
    ended: BTreeMap<u32, f64>,
}

impl Tracker {
    /// Apply a hook event. Returns an alert when the session newly needs the user.
    pub fn apply(&mut self, ev: HookEvent, now: u64) -> Option<Alert> {
        if self.is_stale(&ev) {
            return None;
        }
        if ev.event == "SessionEnd" {
            self.sessions.remove(&ev.pane);
            self.ended.insert(ev.pane, ev.at);
            return None;
        }
        let mut next = next_status(&ev)?;

        let prev = self.sessions.get(&ev.pane).map(|s| s.status.clone());
        let session = self.sessions.entry(ev.pane).or_insert_with(|| Session {
            pane: ev.pane,
            cwd: String::new(),
            status: next.clone(),
            since: now,
            seen: false,
            prompt: None,
            calls: Vec::new(),
            boundary_at: 0.0,
            latest_at: 0.0,
        });
        if !ev.cwd.is_empty() {
            session.cwd = ev.cwd.clone();
        }
        let late = ev.at > 0.0 && ev.at < session.latest_at;
        session.latest_at = session.latest_at.max(ev.at);
        if is_boundary(&ev.event) {
            session.boundary_at = session.boundary_at.max(ev.at);
            // The hook stamps the time, but its pipe can be delivered after a
            // newer event's; the newer status and tool calls stand
            if late {
                return None;
            }
        }

        let ends_call = matches!(
            ev.event.as_str(),
            "PostToolUse" | "PostToolUseFailure" | "PermissionDenied"
        );
        let tool_event = ends_call || ev.event == "PreToolUse";
        match (ev.event.as_str(), &ev.tool_use_id, &ev.tool) {
            ("PreToolUse", Some(id), Some(tool)) => session.calls.push(Call {
                id: id.clone(),
                tool: tool.clone(),
                agent: ev.agent_id.clone(),
            }),
            (_, Some(id), _) if ends_call => session.calls.retain(|c| &c.id != id),
            ("UserPromptSubmit" | "Stop" | "StopFailure", _, _) => session.calls.clear(),
            _ => {}
        }
        // Name the tool still running, if any; parallel calls finish in any order
        if ends_call {
            next = Status::Working(session.calls.last().map(|c| c.tool.clone()));
        }
        // The tool call this event is about. PermissionRequest never says, so
        // take the same agent's newest unfinished call of the same tool.
        let about = match ev.event.as_str() {
            "PermissionRequest" => ev.tool.as_ref().and_then(|tool| {
                let c = session
                    .calls
                    .iter()
                    .rev()
                    .find(|c| &c.tool == tool && c.agent == ev.agent_id)?;
                Some(c.id.clone())
            }),
            _ if tool_event => ev.tool_use_id.clone(),
            _ => None,
        };

        // While a prompt is up, tool events from parallel calls and subagents
        // don't answer it. Its own call finishing does, and so does the agent
        // that asked starting a new call: a denied prompt sends no event.
        if let (Status::NeedsInput(_), Some(p)) = (&session.status, session.prompt.clone()) {
            let new_prompt = matches!(next, Status::NeedsInput(_));
            if tool_event && !new_prompt {
                // A call with no id could be any, including the prompt's
                let finished = ends_call && (about.is_none() || about == p.call);
                let moved_on = ev.event == "PreToolUse"
                    && about != p.call
                    && ev.agent_id == p.agent
                    && newer(ev.at, p.at);
                if !finished && !moved_on {
                    return None;
                }
                if moved_on {
                    session.calls.retain(|c| Some(&c.id) != p.call.as_ref());
                }
            }
        }

        let candidate = match &next {
            Status::NeedsInput(_) => Some(Prompt {
                kind: match (ev.event.as_str(), ev.kind.as_deref()) {
                    ("PreToolUse", _) => PromptKind::Question,
                    ("PermissionRequest", _) | (_, Some("permission_prompt")) => {
                        PromptKind::Permission
                    }
                    (_, Some("agent_needs_input")) => PromptKind::Agent,
                    _ => PromptKind::Elicitation,
                },
                call: about.clone(),
                tool: match ev.event.as_str() {
                    "Notification" => ev.message.as_deref().and_then(tool_in_message),
                    _ => ev.tool.clone(),
                },
                agent: ev.agent_id.clone(),
                at: ev.at,
            }),
            _ => None,
        };
        let same_prompt = match (&prev, &session.prompt, &candidate) {
            (Some(Status::NeedsInput(_)), Some(old), Some(new)) => old.same(new),
            // Saved by a version that didn't record prompts
            (Some(Status::NeedsInput(old)), None, Some(_)) => {
                ev.event == "Notification" || *old == next_message(&next)
            }
            _ => false,
        };
        let changed = match &prev {
            None => true,
            Some(p) => {
                std::mem::discriminant(p) != std::mem::discriminant(&next)
                    || (candidate.is_some() && !same_prompt)
            }
        };
        if changed {
            session.since = now;
            session.seen = false;
            session.status = next.clone();
            session.prompt = candidate.map(|mut p| {
                // A Notification can't say which call it's about; guess the
                // newest one of the tool it names
                if p.call.is_none() && ev.event == "Notification" {
                    let c = session
                        .calls
                        .iter()
                        .rev()
                        .find(|c| p.tool.as_ref().is_none_or(|t| tools_match(t, &c.tool)));
                    if let Some(c) = c {
                        p.call = Some(c.id.clone());
                        p.agent = c.agent.clone();
                    }
                }
                p
            });
        } else if let Some(new) = candidate {
            // Keep the first message, unless PermissionRequest can name the
            // tool a Notification couldn't
            let old = session.prompt.take();
            session.prompt = Some(match old {
                Some(old)
                    if ev.event == "PermissionRequest" && old.kind == PromptKind::Permission =>
                {
                    session.status = next.clone();
                    Prompt {
                        call: new.call.or(old.call),
                        ..new
                    }
                }
                Some(old) => old,
                None => new,
            });
        } else {
            session.status = next.clone();
        }
        if !matches!(session.status, Status::NeedsInput(_)) {
            session.prompt = None;
        }

        let alert = match (&next, ev.event.as_str()) {
            _ if !changed => None,
            (Status::NeedsInput(msg), _) => Some(if msg.is_empty() {
                "Needs your input".to_string()
            } else {
                msg.clone()
            }),
            (Status::Ready, "Stop") => Some("Ready for input".to_string()),
            (Status::Ready, "StopFailure") => Some("Stopped on an error".to_string()),
            _ => None,
        };
        alert.map(|message| Alert {
            pane: session.pane,
            title: format!("Claude · {}", session.project()),
            message,
        })
    }

    /// An event from before its session's latest turn boundary, or from before
    /// the pane's last session ended. Claude Code waits for those hooks, so
    /// their times are ordered; tool hooks can run in parallel, so a tool event
    /// a little older than another one is not stale.
    fn is_stale(&self, ev: &HookEvent) -> bool {
        if ev.at <= 0.0 {
            return false;
        }
        let newest = self.sessions.get(&ev.pane).map(|s| s.boundary_at);
        let ended = self.ended.get(&ev.pane).copied();
        newest.into_iter().chain(ended).any(|t| ev.at < t)
    }

    /// Drop sessions whose pane closed without a SessionEnd (e.g. a crash).
    pub fn retain_panes(&mut self, live: &HashSet<u32>) -> bool {
        let before = self.sessions.len();
        self.sessions.retain(|pane, _| live.contains(pane));
        self.ended.retain(|pane, _| live.contains(pane));
        self.sessions.len() != before
    }

    /// Record that the user is looking at the pane. Returns whether anything changed.
    pub fn mark_seen(&mut self, pane: u32) -> bool {
        match self.sessions.get_mut(&pane) {
            Some(s) if !s.seen => {
                s.seen = true;
                true
            }
            _ => false,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.sessions.is_empty()
    }

    pub fn get(&self, pane: u32) -> Option<&Session> {
        self.sessions.get(&pane)
    }

    /// Sessions in display order: most urgent first, then by tab, then pane.
    pub fn sorted(&self, tab_of: impl Fn(u32) -> Option<usize>) -> Vec<&Session> {
        let mut list: Vec<&Session> = self.sessions.values().collect();
        list.sort_by_key(|s| (s.badge(), tab_of(s.pane).unwrap_or(usize::MAX), s.pane));
        list
    }
}

fn is_boundary(event: &str) -> bool {
    matches!(
        event,
        "UserPromptSubmit" | "Stop" | "StopFailure" | "SessionStart" | "SessionEnd"
    )
}

/// The status an event moves its session to, or None for events that say
/// nothing about it.
fn next_status(ev: &HookEvent) -> Option<Status> {
    Some(match ev.event.as_str() {
        // Compaction can happen mid-turn, so it says nothing about status
        "SessionStart" if ev.source.as_deref() == Some("compact") => return None,
        "SessionStart" => Status::Ready,
        // Auto mode denying a call ends it like a failure; Claude carries on
        "UserPromptSubmit" | "PostToolUse" | "PostToolUseFailure" | "PermissionDenied" => {
            Status::Working(None)
        }
        // These tools exist to wait on the user, and fire no permission prompt
        "PreToolUse" => match ev.tool.as_deref() {
            Some("AskUserQuestion") => Status::NeedsInput("Has a question".into()),
            Some("ExitPlanMode") => Status::NeedsInput("Plan ready for review".into()),
            _ => Status::Working(ev.tool.clone()),
        },
        "PermissionRequest" => Status::NeedsInput(permission_message(ev)),
        "Stop" | "StopFailure" => Status::Ready,
        "Notification" => match ev.kind.as_deref() {
            // A reminder that it's still idle; Stop already alerted
            Some("idle_prompt") => Status::Ready,
            Some(
                "permission_prompt"
                | "elicitation_dialog"
                | "elicitation_url_dialog"
                | "agent_needs_input",
            ) => Status::NeedsInput(ev.message.clone().unwrap_or_default()),
            // The user answered an MCP server's question
            Some("elicitation_complete" | "elicitation_response") => Status::Working(None),
            // auth_success, agent_completed, quota_* and anything newer
            _ => return None,
        },
        _ => return None,
    })
}

fn next_message(status: &Status) -> String {
    match status {
        Status::NeedsInput(msg) => msg.clone(),
        _ => String::new(),
    }
}

fn permission_message(ev: &HookEvent) -> String {
    match &ev.tool {
        Some(tool) => format!("Wants to use {tool}"),
        None => "Needs permission".to_string(),
    }
}

/// The palette's part of what survives a plugin reload.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Palette {
    /// Recently run actions, newest first, as command lines.
    #[serde(default)]
    pub recent: Vec<String>,
    /// The mode Zellij returns to, for key hints. Zellij only says when the
    /// mode changes, so a reload would otherwise forget it.
    #[serde(default)]
    pub base_mode: Option<InputMode>,
}

/// What survives a plugin reload.
#[derive(Serialize, Deserialize)]
struct Saved {
    sessions: Vec<Session>,
    #[serde(default)]
    tabs: TabNames,
    #[serde(default)]
    ended: BTreeMap<u32, f64>,
    #[serde(default)]
    palette: Palette,
}

/// Sessions, tab names and the palette's state as JSON, to survive a plugin reload.
pub fn save(tracker: &Tracker, tabs: &TabNames, palette: &Palette) -> String {
    let saved = Saved {
        sessions: tracker.sessions.values().cloned().collect(),
        tabs: tabs.clone(),
        ended: tracker.ended.clone(),
        palette: palette.clone(),
    };
    serde_json::to_string(&saved).unwrap_or_default()
}

/// What `save` wrote, or the bare session list earlier versions wrote.
/// Anything unreadable starts empty; sessions reappear as they fire hook
/// events.
pub fn restore(json: &str) -> (Tracker, TabNames, Palette) {
    let saved = serde_json::from_str::<Saved>(json)
        .or_else(|_| {
            serde_json::from_str(json).map(|sessions| Saved {
                sessions,
                tabs: TabNames::default(),
                ended: BTreeMap::new(),
                palette: Palette::default(),
            })
        })
        .unwrap_or(Saved {
            sessions: Vec::new(),
            tabs: TabNames::default(),
            ended: BTreeMap::new(),
            palette: Palette::default(),
        });
    let tracker = Tracker {
        sessions: saved.sessions.into_iter().map(|s| (s.pane, s)).collect(),
        ended: saved.ended,
    };
    (tracker, saved.tabs, saved.palette)
}

/// Fuzzy-match a query against a row's fields. Each space-separated term must
/// match one field as a case-insensitive subsequence. Returns the total score
/// (higher is better) and, per field, the char positions to highlight.
pub fn fuzzy_match(query: &str, fields: &[&str]) -> Option<(i32, Vec<Vec<usize>>)> {
    let mut total = 0;
    let mut hits = vec![Vec::new(); fields.len()];
    for term in query.split_whitespace() {
        let term: Vec<char> = term.chars().map(fold).collect();
        // On a tie, the earlier field wins
        let (field, score, positions) = fields
            .iter()
            .enumerate()
            .filter_map(|(i, f)| match_term(&term, f).map(|(s, p)| (i, s, p)))
            .max_by(|a, b| a.1.cmp(&b.1).then(b.0.cmp(&a.0)))?;
        total += score;
        hits[field].extend(positions);
    }
    for h in &mut hits {
        h.sort_unstable();
        h.dedup();
    }
    Some((total, hits))
}

fn fold(c: char) -> char {
    c.to_lowercase().next().unwrap_or(c)
}

/// The best-scoring way to find `term` as a subsequence of `text`, trying each
/// place the first char appears and matching the rest greedily from there.
fn match_term(term: &[char], text: &str) -> Option<(i32, Vec<usize>)> {
    let chars: Vec<char> = text.chars().collect();
    let folded: Vec<char> = chars.iter().copied().map(fold).collect();
    let mut best: Option<(i32, Vec<usize>)> = None;
    for start in (0..folded.len()).filter(|&i| folded[i] == term[0]) {
        let mut positions = vec![start];
        let mut i = start + 1;
        for &q in &term[1..] {
            match folded[i..].iter().position(|&c| c == q) {
                Some(off) => {
                    positions.push(i + off);
                    i += off + 1;
                }
                None => break,
            }
        }
        // A later start can't fit the rest either
        if positions.len() < term.len() {
            break;
        }
        let score = match_score(&chars, &positions);
        if best.as_ref().is_none_or(|(b, _)| score > *b) {
            best = Some((score, positions));
        }
    }
    best
}

/// Rewards runs of adjacent matches and matches at word starts; penalizes gaps.
fn match_score(chars: &[char], positions: &[usize]) -> i32 {
    let mut score = 0;
    for (k, &p) in positions.iter().enumerate() {
        score += 1;
        if k > 0 {
            let gap = p - positions[k - 1] - 1;
            score += if gap == 0 { 4 } else { -(gap.min(5) as i32) };
        }
        let word_start = p == 0
            || !chars[p - 1].is_alphanumeric()
            || (chars[p - 1].is_lowercase() && chars[p].is_uppercase());
        if word_start {
            score += 3;
        }
    }
    score
}

pub fn format_age(secs: u64) -> String {
    match secs {
        0..60 => format!("{secs}s"),
        60..3600 => format!("{}m", secs / 60),
        _ => format!("{}h", secs / 3600),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(event: &str) -> HookEvent {
        HookEvent {
            event: event.into(),
            pane: 3,
            cwd: "/Users/me/Developer/my-app".into(),
            ..Default::default()
        }
    }

    #[test]
    fn stop_alerts_once() {
        let mut t = Tracker::default();
        assert_eq!(t.apply(ev("SessionStart"), 0), None);
        assert_eq!(t.apply(ev("UserPromptSubmit"), 1), None);
        let alert = t.apply(ev("Stop"), 2).unwrap();
        assert_eq!(alert.title, "Claude · my-app");
        assert_eq!(alert.message, "Ready for input");
        // The idle reminder doesn't alert again
        let mut idle = ev("Notification");
        idle.kind = Some("idle_prompt".into());
        assert_eq!(t.apply(idle, 60), None);
        assert_eq!(t.get(3).unwrap().since, 2);
    }

    #[test]
    fn permission_alerts_once_and_keeps_tool_message() {
        let mut t = Tracker::default();
        t.apply(ev("UserPromptSubmit"), 0);
        let mut req = ev("PermissionRequest");
        req.tool = Some("Bash".into());
        assert_eq!(t.apply(req, 1).unwrap().message, "Wants to use Bash");
        let mut note = ev("Notification");
        note.kind = Some("permission_prompt".into());
        note.message = Some("Claude needs your permission to use Bash".into());
        assert_eq!(t.apply(note, 2), None);
        assert_eq!(
            t.get(3).unwrap().status,
            Status::NeedsInput("Wants to use Bash".into())
        );
        // Approving runs the tool, which returns it to work
        t.apply(ev("PostToolUse"), 3);
        assert_eq!(t.get(3).unwrap().status, Status::Working(None));
    }

    #[test]
    fn questions_need_input() {
        let mut t = Tracker::default();
        t.apply(ev("UserPromptSubmit"), 0);
        let mut ask = ev("PreToolUse");
        ask.tool = Some("AskUserQuestion".into());
        assert_eq!(t.apply(ask, 1).unwrap().message, "Has a question");
        // Answering completes the tool
        t.apply(ev("PostToolUse"), 2);
        assert_eq!(t.get(3).unwrap().status, Status::Working(None));
    }

    #[test]
    fn only_waiting_notifications_need_input() {
        let mut t = Tracker::default();
        t.apply(ev("UserPromptSubmit"), 0);
        for kind in [
            "auth_success",
            "agent_completed",
            "quota_auto_resume_fired",
            "something_new",
        ] {
            let mut note = ev("Notification");
            note.kind = Some(kind.into());
            assert_eq!(t.apply(note, 1), None, "{kind}");
        }
        assert_eq!(t.get(3).unwrap().status, Status::Working(None));
        let mut ask = ev("Notification");
        ask.kind = Some("elicitation_dialog".into());
        ask.message = Some("Server wants a token".into());
        assert_eq!(t.apply(ask, 2).unwrap().message, "Server wants a token");
        let mut done = ev("Notification");
        done.kind = Some("elicitation_complete".into());
        t.apply(done, 3);
        assert_eq!(t.get(3).unwrap().status, Status::Working(None));
    }

    #[test]
    fn stop_failure_alerts() {
        let mut t = Tracker::default();
        t.apply(ev("UserPromptSubmit"), 0);
        assert_eq!(
            t.apply(ev("StopFailure"), 1).unwrap().message,
            "Stopped on an error"
        );
        assert_eq!(t.get(3).unwrap().status, Status::Ready);
    }

    #[test]
    fn compaction_keeps_status() {
        let mut t = Tracker::default();
        t.apply(ev("UserPromptSubmit"), 0);
        let mut compact = ev("SessionStart");
        compact.source = Some("compact".into());
        assert_eq!(t.apply(compact, 1), None);
        assert_eq!(t.get(3).unwrap().status, Status::Working(None));
        assert_eq!(t.get(3).unwrap().since, 0);
    }

    #[test]
    fn tool_changes_keep_since() {
        let mut t = Tracker::default();
        t.apply(ev("UserPromptSubmit"), 5);
        let mut pre = ev("PreToolUse");
        pre.tool = Some("Read".into());
        t.apply(pre, 9);
        let s = t.get(3).unwrap();
        assert_eq!(s.status.label(), "Read");
        assert_eq!(s.since, 5);
    }

    #[test]
    fn session_end_and_closed_panes_remove() {
        let mut t = Tracker::default();
        t.apply(ev("SessionStart"), 0);
        let mut other = ev("SessionStart");
        other.pane = 7;
        t.apply(other, 0);
        t.apply(ev("SessionEnd"), 1);
        assert!(t.get(3).is_none());
        assert!(t.retain_panes(&HashSet::new()));
        assert!(t.is_empty());
    }

    #[test]
    fn sorts_by_urgency_then_tab() {
        let mut t = Tracker::default();
        for (pane, event) in [
            (1, "UserPromptSubmit"),
            (2, "Stop"),
            (3, "Stop"),
            (4, "PermissionRequest"),
        ] {
            let mut e = ev(event);
            e.pane = pane;
            t.apply(e, 0);
        }
        let order: Vec<u32> = t
            .sorted(|p| Some(if p == 2 { 5 } else { 1 }))
            .iter()
            .map(|s| s.pane)
            .collect();
        assert_eq!(order, vec![4, 3, 2, 1]);
    }

    #[test]
    fn seen_ready_sorts_below_working() {
        let mut t = Tracker::default();
        for (pane, event) in [(1, "Stop"), (2, "UserPromptSubmit"), (3, "Stop")] {
            let mut e = ev(event);
            e.pane = pane;
            t.apply(e, 0);
        }
        assert!(t.mark_seen(1));
        assert!(!t.mark_seen(1));
        let order: Vec<u32> = t.sorted(|_| None).iter().map(|s| s.pane).collect();
        assert_eq!(order, vec![3, 2, 1]);
        // A new turn finishing makes it unseen again
        t.apply(
            HookEvent {
                pane: 1,
                ..ev("UserPromptSubmit")
            },
            1,
        );
        t.apply(
            HookEvent {
                pane: 1,
                ..ev("Stop")
            },
            2,
        );
        assert!(!t.get(1).unwrap().seen);
    }

    fn tool_ev(event: &str, tool: &str, id: Option<&str>) -> HookEvent {
        HookEvent {
            tool: Some(tool.into()),
            tool_use_id: id.map(Into::into),
            ..ev(event)
        }
    }

    #[test]
    fn other_tools_dont_answer_a_prompt() {
        let at = |at: f64, e: HookEvent| HookEvent { at, ..e };
        let sub = |e: HookEvent| HookEvent {
            agent_id: Some("a1".into()),
            ..e
        };
        let mut t = Tracker::default();
        t.apply(at(1.0, ev("UserPromptSubmit")), 0);
        t.apply(at(2.0, tool_ev("PreToolUse", "Bash", Some("b1"))), 1);
        t.apply(at(2.0, tool_ev("PreToolUse", "Read", Some("r1"))), 1);
        // PermissionRequest never has an id; it finds the unfinished Bash call
        assert!(t
            .apply(at(3.0, tool_ev("PermissionRequest", "Bash", None)), 2)
            .is_some());
        // A parallel call finishing, and a subagent's new call, don't answer it
        t.apply(at(4.0, tool_ev("PostToolUse", "Read", Some("r1"))), 3);
        t.apply(at(4.0, sub(tool_ev("PreToolUse", "Grep", Some("g1")))), 3);
        t.apply(at(4.5, sub(tool_ev("PostToolUse", "Grep", Some("g1")))), 3);
        assert_eq!(
            t.get(3).unwrap().status,
            Status::NeedsInput("Wants to use Bash".into())
        );
        assert_eq!(t.get(3).unwrap().since, 2);
        // Approving runs Bash to completion
        t.apply(at(5.0, tool_ev("PostToolUse", "Bash", Some("b1"))), 4);
        assert_eq!(t.get(3).unwrap().status, Status::Working(None));
    }

    #[test]
    fn a_denied_prompt_clears_when_claude_moves_on() {
        let at = |at: f64, e: HookEvent| HookEvent { at, ..e };
        let mut t = Tracker::default();
        t.apply(at(1.0, ev("UserPromptSubmit")), 0);
        t.apply(at(2.0, tool_ev("PreToolUse", "Bash", Some("b1"))), 1);
        t.apply(at(3.0, tool_ev("PermissionRequest", "Bash", None)), 2);
        // A call from the same batch started before the prompt doesn't count,
        // even when its pipe lands after it
        t.apply(at(2.5, tool_ev("PreToolUse", "WebFetch", Some("w1"))), 3);
        assert!(matches!(t.get(3).unwrap().status, Status::NeedsInput(_)));
        // Denied: no event for b1, but Claude starts a new call
        t.apply(at(4.0, tool_ev("PreToolUse", "Edit", Some("e1"))), 4);
        assert_eq!(
            t.get(3).unwrap().status,
            Status::Working(Some("Edit".into()))
        );
        // b1 never finished; it isn't left to name the status
        t.apply(at(5.0, tool_ev("PostToolUse", "Edit", Some("e1"))), 5);
        t.apply(at(5.0, tool_ev("PostToolUse", "WebFetch", Some("w1"))), 5);
        assert_eq!(t.get(3).unwrap().status, Status::Working(None));
    }

    #[test]
    fn prompts_without_a_call_ignore_other_tools() {
        let mut t = Tracker::default();
        t.apply(ev("UserPromptSubmit"), 0);
        t.apply(tool_ev("PreToolUse", "mcp__vault__login", Some("m1")), 1);
        let mut ask = ev("Notification");
        ask.kind = Some("elicitation_dialog".into());
        ask.message = Some("Server wants a token".into());
        assert!(t.apply(ask, 2).is_some());
        // A subagent's tool finishing doesn't close the dialog
        t.apply(
            HookEvent {
                agent_id: Some("a1".into()),
                ..tool_ev("PostToolUse", "Read", Some("r1"))
            },
            3,
        );
        assert_eq!(
            t.get(3).unwrap().status,
            Status::NeedsInput("Server wants a token".into())
        );
        // The MCP call it belongs to finishing does
        t.apply(tool_ev("PostToolUse", "mcp__vault__login", Some("m1")), 4);
        assert_eq!(t.get(3).unwrap().status, Status::Working(None));
    }

    #[test]
    fn a_different_prompt_alerts() {
        let mut t = Tracker::default();
        t.apply(ev("UserPromptSubmit"), 0);
        t.apply(tool_ev("PreToolUse", "AskUserQuestion", Some("q1")), 1);
        let note = |message: &str| HookEvent {
            kind: Some("permission_prompt".into()),
            message: Some(message.into()),
            ..ev("Notification")
        };
        // A subagent's permission prompt, named by its Notification
        let alert = t.apply(note("Claude needs your permission to use Bash"), 2);
        assert_eq!(
            alert.unwrap().message,
            "Claude needs your permission to use Bash"
        );
        // Its own Notification again doesn't
        assert_eq!(
            t.apply(note("Claude needs your permission to use Bash"), 3),
            None
        );
        // Nor does one that can't tell
        assert_eq!(t.apply(note("Claude needs your permission"), 3), None);
        // A PermissionRequest for another tool, with no call to tie it to, does
        assert!(t
            .apply(tool_ev("PermissionRequest", "Edit", None), 4)
            .is_some());
    }

    #[test]
    fn finishing_a_call_names_one_still_running() {
        let mut t = Tracker::default();
        t.apply(ev("UserPromptSubmit"), 0);
        t.apply(tool_ev("PreToolUse", "Bash", Some("b1")), 1);
        t.apply(tool_ev("PreToolUse", "Read", Some("r1")), 1);
        t.apply(tool_ev("PostToolUse", "Read", Some("r1")), 2);
        assert_eq!(t.get(3).unwrap().status.label(), "Bash");
        // Auto mode denying it ends it too
        t.apply(tool_ev("PermissionDenied", "Bash", Some("b1")), 3);
        assert_eq!(t.get(3).unwrap().status.label(), "thinking");
    }

    #[test]
    fn a_new_prompt_replaces_the_old_one() {
        for ids in [(Some("b1"), Some("e1")), (None, None)] {
            let mut t = Tracker::default();
            t.apply(tool_ev("PermissionRequest", "Bash", ids.0), 1);
            let alert = t.apply(tool_ev("PermissionRequest", "Edit", ids.1), 2);
            assert_eq!(alert.unwrap().message, "Wants to use Edit", "{ids:?}");
            assert_eq!(t.get(3).unwrap().since, 2);
        }
    }

    #[test]
    fn one_prompt_alerts_once() {
        // ExitPlanMode fires PreToolUse, then PermissionRequest
        let mut t = Tracker::default();
        t.apply(ev("UserPromptSubmit"), 0);
        assert!(t
            .apply(tool_ev("PreToolUse", "ExitPlanMode", Some("p1")), 1)
            .is_some());
        assert_eq!(
            t.apply(tool_ev("PermissionRequest", "ExitPlanMode", None), 2),
            None
        );
        assert_eq!(
            t.get(3).unwrap().status,
            Status::NeedsInput("Plan ready for review".into())
        );

        // A Notification first, then PermissionRequest names the tool
        let mut t = Tracker::default();
        let mut note = ev("Notification");
        note.kind = Some("permission_prompt".into());
        note.message = Some("Claude needs your permission".into());
        assert!(t.apply(note, 1).is_some());
        assert_eq!(
            t.apply(tool_ev("PermissionRequest", "Bash", Some("b1")), 2),
            None
        );
        assert_eq!(
            t.get(3).unwrap().status,
            Status::NeedsInput("Wants to use Bash".into())
        );
        t.apply(tool_ev("PostToolUse", "Read", Some("r1")), 3);
        assert_eq!(
            t.get(3).unwrap().status,
            Status::NeedsInput("Wants to use Bash".into())
        );
    }

    #[test]
    fn late_events_are_dropped() {
        let at = |event: &str, at: f64| HookEvent { at, ..ev(event) };
        let mut t = Tracker::default();
        t.apply(at("UserPromptSubmit", 1.0), 0);
        t.apply(at("Stop", 3.0), 0);
        assert_eq!(t.apply(at("PostToolUse", 2.0), 0), None);
        assert_eq!(t.get(3).unwrap().status, Status::Ready);
        // /clear ends one session and starts another; the end arrives last
        t.apply(at("SessionStart", 5.0), 0);
        t.apply(at("SessionEnd", 4.0), 0);
        assert!(t.get(3).is_some());
        // Nothing from before an exit revives the session
        t.apply(at("SessionEnd", 7.0), 0);
        t.apply(at("PostToolUse", 6.0), 0);
        assert!(t.get(3).is_none());
        t.apply(at("SessionStart", 8.0), 0);
        assert!(t.get(3).is_some());
        // Parallel tools' hooks overlap, so a slightly older prompt still counts
        t.apply(at("UserPromptSubmit", 9.0), 0);
        t.apply(
            HookEvent {
                at: 9.2,
                ..tool_ev("PostToolUse", "Read", Some("r1"))
            },
            0,
        );
        assert!(t
            .apply(
                HookEvent {
                    at: 9.1,
                    ..tool_ev("PermissionRequest", "Bash", None)
                },
                0
            )
            .is_some());
    }

    #[test]
    fn late_boundaries_keep_newer_status() {
        let mut t = Tracker::default();
        t.apply(
            HookEvent {
                at: 1.0,
                ..ev("Stop")
            },
            0,
        );
        // The prompt's pipe lands after the question it led to
        t.apply(
            HookEvent {
                at: 3.0,
                ..tool_ev("PreToolUse", "AskUserQuestion", Some("q1"))
            },
            0,
        );
        assert_eq!(
            t.apply(
                HookEvent {
                    at: 2.0,
                    ..ev("UserPromptSubmit")
                },
                0
            ),
            None
        );
        assert_eq!(
            t.get(3).unwrap().status,
            Status::NeedsInput("Has a question".into())
        );
        // It still marks the boundary, so older events stay dropped
        assert_eq!(
            t.apply(
                HookEvent {
                    at: 1.5,
                    ..ev("PostToolUse")
                },
                0
            ),
            None
        );
        t.apply(
            HookEvent {
                at: 4.0,
                ..tool_ev("PostToolUse", "AskUserQuestion", Some("q1"))
            },
            0,
        );
        assert_eq!(t.get(3).unwrap().status, Status::Working(None));
    }

    #[test]
    fn save_restores() {
        let mut t = Tracker::default();
        let mut req = ev("PermissionRequest");
        req.tool = Some("Bash".into());
        t.apply(req, 5);
        t.apply(
            HookEvent {
                pane: 4,
                ..ev("Stop")
            },
            6,
        );
        t.mark_seen(4);
        let mut tabs = TabNames::default();
        tabs.wrote(2, "api", &["! api".into()]);
        let palette = Palette {
            recent: vec!["close-pane".into()],
            base_mode: Some(InputMode::Locked),
        };
        let (r, tabs, restored) = restore(&save(&t, &tabs, &palette));
        let s = r.get(3).unwrap();
        assert_eq!(s.status, Status::NeedsInput("Wants to use Bash".into()));
        assert_eq!((s.since, s.project()), (5, "my-app"));
        assert!(r.get(4).unwrap().seen);
        assert_eq!(tabs.base(2, "! api"), "api");
        assert_eq!(restored, palette);
        // The bare session list earlier versions saved
        let old = serde_json::to_string(&r.sorted(|_| None)).unwrap();
        assert!(restore(&old).0.get(3).is_some());
        assert!(restore("not json").0.is_empty());
        // A session that ended stays ended through a reload
        let mut t = Tracker::default();
        t.apply(
            HookEvent {
                at: 2.0,
                ..ev("SessionEnd")
            },
            0,
        );
        let (mut r, ..) = restore(&save(&t, &TabNames::default(), &Palette::default()));
        r.apply(
            HookEvent {
                at: 1.0,
                ..ev("PostToolUse")
            },
            0,
        );
        assert!(r.get(3).is_none());
    }

    #[test]
    fn fuzzy_matches_subsequences_per_field() {
        let fields = ["dashboard", "2 api", "needs input"];
        assert_eq!(
            fuzzy_match("", &fields),
            Some((0, vec![vec![], vec![], vec![]]))
        );
        assert_eq!(
            fuzzy_match("dsb", &fields).unwrap().1,
            vec![vec![0, 2, 4], vec![], vec![]]
        );
        assert_eq!(fuzzy_match("DASH", &fields).unwrap().1[0], vec![0, 1, 2, 3]);
        // Every term must match, each in whichever field fits best
        assert_eq!(
            fuzzy_match("dash need", &fields).unwrap().1[2],
            vec![0, 1, 2, 3]
        );
        assert_eq!(fuzzy_match("dash zzz", &fields), None);
        // Terms don't match across fields
        assert_eq!(fuzzy_match("boardapi", &fields), None);
    }

    #[test]
    fn fuzzy_prefers_runs_and_word_starts() {
        let score = |q, t| fuzzy_match(q, &[t]).map(|(s, _)| s);
        assert!(score("api", "api-gateway") > score("api", "a-p-i"));
        assert!(score("gw", "api-gateway") < score("gw", "api-gw"));
        // A later, tighter match beats the first greedy one
        assert_eq!(
            fuzzy_match("rad", &["r-a-radar"]).unwrap().1[0],
            vec![4, 5, 6]
        );
    }

    #[test]
    fn project_is_last_path_component() {
        let mut t = Tracker::default();
        let mut e = ev("SessionStart");
        e.cwd = "/Users/me/.config/".into();
        t.apply(e, 0);
        assert_eq!(t.get(3).unwrap().project(), ".config");
        assert_eq!(format_age(59), "59s");
        assert_eq!(format_age(61), "1m");
        assert_eq!(format_age(7300), "2h");
    }
}
