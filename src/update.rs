//! Telling the user when a newer release is out, and when the Claude Code
//! hook and this plugin come from different releases. Kept free of
//! zellij-tile so it can be unit tested natively; the plugin makes the request
//! and hands the response here.

use serde::{Deserialize, Serialize};
use std::cmp::Ordering;
use std::collections::BTreeMap;

/// This build's version, as `just bump` writes it into Cargo.toml.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
/// Where releases come from, so a fork checks its own.
const REPOSITORY: &str = env!("CARGO_PKG_REPOSITORY");

/// How long a successful check stands before the next one.
const DAY: u64 = 24 * 60 * 60;
/// How long to wait after a failed check before trying again.
const RETRY: u64 = 60 * 60;
/// Zellij's HTTP client has no timeout, so a request that hangs never
/// reports back. After this long the plugin stops waiting and sends a new one.
const GIVE_UP: u64 = 10 * 60;

/// Hooks from before the hook sent its version all came from this release.
const UNVERSIONED_HOOK: &str = "0.1.0";

/// The GitHub API URL for the repository's latest release, if the repository
/// is on GitHub.
pub fn latest_release_url() -> Option<String> {
    api_url(REPOSITORY)
}

fn api_url(repository: &str) -> Option<String> {
    let path = repository
        .strip_prefix("https://github.com/")?
        .trim_end_matches('/')
        .trim_end_matches(".git");
    let mut parts = path.split('/');
    let (owner, repo) = (parts.next()?, parts.next()?);
    if owner.is_empty() || repo.is_empty() || parts.next().is_some() {
        return None;
    }
    Some(format!(
        "https://api.github.com/repos/{owner}/{repo}/releases/latest"
    ))
}

/// A release version as its three numbers, with or without a leading `v`.
/// Anything else, such as a pre-release suffix, isn't a version to offer.
fn parse(version: &str) -> Option<(u64, u64, u64)> {
    let mut parts = version.strip_prefix('v').unwrap_or(version).split('.');
    let mut next = || parts.next()?.parse::<u64>().ok();
    let v = (next()?, next()?, next()?);
    parts.next().is_none().then_some(v)
}

fn compare(a: &str, b: &str) -> Option<Ordering> {
    Some(parse(a)?.cmp(&parse(b)?))
}

/// The update check's part of what survives a plugin reload.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct UpdateCheck {
    /// Unix time after which to check again.
    #[serde(default)]
    pub next_at: u64,
    /// The latest release's tag, as of the last successful check.
    #[serde(default)]
    pub latest: Option<String>,
    /// GitHub's tag for that response. Sending it back gets a 304 when nothing
    /// changed, which GitHub doesn't count against the rate limit.
    #[serde(default)]
    pub etag: Option<String>,
    /// Unix time the request in flight was sent; 0 when none is.
    #[serde(skip)]
    pub sent_at: u64,
}

impl UpdateCheck {
    /// Whether to send a request now.
    pub fn due(&self, now: u64) -> bool {
        let waiting = self.sent_at != 0 && now < self.sent_at + GIVE_UP;
        !waiting && now >= self.next_at
    }

    /// The headers for the request; call when sending it.
    pub fn send(&mut self, now: u64) -> BTreeMap<String, String> {
        self.sent_at = now;
        let mut headers = BTreeMap::from([
            (
                "User-Agent".to_owned(),
                format!("zellij-mission-control/{VERSION}"),
            ),
            (
                "Accept".to_owned(),
                "application/vnd.github+json".to_owned(),
            ),
        ]);
        // A 304 only helps when there's a tag to keep
        if let (Some(etag), Some(_)) = (&self.etag, &self.latest) {
            headers.insert("If-None-Match".to_owned(), etag.clone());
        }
        headers
    }

    /// Take in GitHub's response. Returns whether the newest release known
    /// changed, or why the check failed.
    pub fn receive(
        &mut self,
        status: u16,
        headers: &BTreeMap<String, String>,
        body: &[u8],
        now: u64,
    ) -> Result<bool, String> {
        self.sent_at = 0;
        let before = self.latest.clone();
        let result = match status {
            200 => {
                #[derive(Deserialize)]
                struct Release {
                    tag_name: String,
                }
                match serde_json::from_slice::<Release>(body) {
                    Ok(release) => {
                        self.latest = Some(release.tag_name);
                        self.etag = headers
                            .iter()
                            .find(|(k, _)| k.eq_ignore_ascii_case("etag"))
                            .map(|(_, v)| v.clone());
                        Ok(())
                    }
                    Err(e) => Err(format!("unreadable release: {e}")),
                }
            }
            304 if self.latest.is_some() => Ok(()),
            // No release published yet
            404 => {
                self.latest = None;
                self.etag = None;
                Ok(())
            }
            _ => Err(format!(
                "HTTP {status}: {}",
                String::from_utf8_lossy(&body[..body.len().min(200)])
            )),
        };
        self.next_at = now + if result.is_ok() { DAY } else { RETRY };
        result.map(|()| self.latest != before)
    }

    /// The newer release to offer, if there is one.
    pub fn available(&self) -> Option<&str> {
        let latest = self.latest.as_deref()?;
        (compare(latest, VERSION)? == Ordering::Greater).then_some(latest)
    }
}

/// The hook and this plugin come from different releases.
#[derive(Debug, PartialEq, Eq)]
pub enum Drift {
    /// The hook is older: update the Claude Code plugin.
    HookBehind(String),
    /// The hook is newer: update the plugin URL in config.kdl.
    PluginBehind(String),
}

/// Whether a hook that reported `hook` matches this build.
pub fn drift(hook: Option<&str>) -> Option<Drift> {
    let hook = hook.unwrap_or(UNVERSIONED_HOOK);
    match compare(hook, VERSION)? {
        Ordering::Less => Some(Drift::HookBehind(hook.to_owned())),
        Ordering::Greater => Some(Drift::PluginBehind(hook.to_owned())),
        Ordering::Equal => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bump(v: &str, by: (u64, u64, u64)) -> String {
        let (a, b, c) = parse(v).unwrap();
        format!("{}.{}.{}", a + by.0, b + by.1, c + by.2)
    }

    #[test]
    fn api_url_comes_from_the_repository() {
        assert_eq!(
            api_url("https://github.com/c9rgreen/zellij-mission-control").as_deref(),
            Some("https://api.github.com/repos/c9rgreen/zellij-mission-control/releases/latest")
        );
        assert_eq!(
            api_url("https://github.com/a/b.git/").as_deref(),
            Some("https://api.github.com/repos/a/b/releases/latest")
        );
        assert_eq!(api_url("https://gitlab.com/a/b"), None);
        assert_eq!(api_url("https://github.com/a"), None);
        assert_eq!(api_url("https://github.com/a/b/c"), None);
        assert!(latest_release_url().is_some());
    }

    #[test]
    fn versions_compare_by_number() {
        assert_eq!(parse("v1.2.3"), Some((1, 2, 3)));
        assert_eq!(parse("1.2.3"), Some((1, 2, 3)));
        assert_eq!(parse("v1.2"), None);
        assert_eq!(parse("v1.2.3.4"), None);
        assert_eq!(parse("v1.2.3-rc1"), None);
        assert_eq!(compare("v0.10.0", "0.9.9"), Some(Ordering::Greater));
        assert_eq!(compare("v0.2.0", "0.2.0"), Some(Ordering::Equal));
        assert_eq!(compare("junk", "0.2.0"), None);
    }

    #[test]
    fn offers_only_newer_releases() {
        let mut check = UpdateCheck::default();
        assert_eq!(check.available(), None);
        check.latest = Some(format!("v{VERSION}"));
        assert_eq!(check.available(), None);
        let newer = format!("v{}", bump(VERSION, (0, 0, 1)));
        check.latest = Some(newer.clone());
        assert_eq!(check.available(), Some(newer.as_str()));
        check.latest = Some("v0.0.0".into());
        assert_eq!(check.available(), None);
        check.latest = Some("nightly".into());
        assert_eq!(check.available(), None);
    }

    #[test]
    fn checks_daily_and_retries_failures_sooner() {
        let mut check = UpdateCheck::default();
        assert!(check.due(1000));
        check.send(1000);
        // One request at a time
        assert!(!check.due(1001));
        let headers = BTreeMap::from([("etag".to_owned(), "W/\"abc\"".to_owned())]);
        let changed = check
            .receive(200, &headers, br#"{"tag_name":"v9.0.0","name":"x"}"#, 1002)
            .unwrap();
        assert!(changed);
        assert_eq!(check.latest.as_deref(), Some("v9.0.0"));
        assert_eq!(check.etag.as_deref(), Some("W/\"abc\""));
        assert!(!check.due(1002 + DAY - 1));
        assert!(check.due(1002 + DAY));

        // Unchanged since: GitHub answers 304 to the saved tag
        let sent = check.send(1002 + DAY);
        assert_eq!(
            sent.get("If-None-Match").map(String::as_str),
            Some("W/\"abc\"")
        );
        assert!(sent["User-Agent"].starts_with("zellij-mission-control/"));
        let changed = check
            .receive(304, &BTreeMap::new(), b"", 1003 + DAY)
            .unwrap();
        assert!(!changed);
        assert_eq!(check.latest.as_deref(), Some("v9.0.0"));

        // Offline or rate limited: Zellij reports a failed request as a 400
        let at = 1003 + 2 * DAY;
        check.send(at);
        assert!(check
            .receive(400, &BTreeMap::new(), b"timed out", at)
            .is_err());
        assert_eq!(check.latest.as_deref(), Some("v9.0.0"));
        assert!(!check.due(at + RETRY - 1));
        assert!(check.due(at + RETRY));
    }

    #[test]
    fn a_hung_request_is_given_up() {
        let mut check = UpdateCheck::default();
        check.send(1000);
        assert!(!check.due(1000 + GIVE_UP - 1));
        assert!(check.due(1000 + GIVE_UP));
    }

    #[test]
    fn a_304_without_a_saved_release_is_a_failure() {
        let mut check = UpdateCheck::default();
        // No tag to keep, so no If-None-Match either
        assert!(!check.send(1).contains_key("If-None-Match"));
        assert!(check.receive(304, &BTreeMap::new(), b"", 1).is_err());
        assert!(check
            .receive(200, &BTreeMap::new(), b"not json", 1)
            .is_err());
    }

    #[test]
    fn no_release_yet_clears_the_last_one() {
        let mut check = UpdateCheck {
            latest: Some("v9.0.0".into()),
            etag: Some("x".into()),
            ..Default::default()
        };
        assert!(check.receive(404, &BTreeMap::new(), b"", 1).unwrap());
        assert_eq!((check.latest, check.etag), (None, None));
    }

    #[test]
    fn survives_a_reload_without_the_request_in_flight() {
        let mut check = UpdateCheck {
            next_at: 5,
            latest: Some("v1.0.0".into()),
            etag: Some("e".into()),
            sent_at: 0,
        };
        check.send(3);
        let restored: UpdateCheck =
            serde_json::from_str(&serde_json::to_string(&check).unwrap()).unwrap();
        assert_eq!(restored.sent_at, 0);
        assert_eq!(restored.latest, check.latest);
        assert_eq!(restored.next_at, 5);
        // Older saves have no update check at all
        let old: UpdateCheck = serde_json::from_str("{}").unwrap();
        assert_eq!(old, UpdateCheck::default());
    }

    #[test]
    fn hook_drift_names_the_half_to_update() {
        assert_eq!(drift(Some(VERSION)), None);
        let older = "0.0.1";
        assert_eq!(drift(Some(older)), Some(Drift::HookBehind(older.into())));
        let newer = bump(VERSION, (1, 0, 0));
        assert_eq!(
            drift(Some(&newer)),
            Some(Drift::PluginBehind(newer.clone()))
        );
        // A hook from before versions were sent is 0.1.0's
        assert_eq!(drift(None), drift(Some(UNVERSIONED_HOOK)),);
        // A version we can't read says nothing
        assert_eq!(drift(Some("dev")), None);
    }
}
