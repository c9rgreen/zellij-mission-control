/// Posts a notification as an OSC escape code written to the Claude pane's pty.
/// Zellij forwards OSC 777 from a pane to the outer terminal (as OSC 9), so it
/// reaches the local terminal even over SSH; the terminal decides whether to
/// show it. Args: the pid Zellij reports for the pane, title, message.
///
/// Events come from a pipe any local process can write to, so the pty isn't
/// taken from them: `ps` names the pane process's own terminal, which can only
/// be one of the pane's. It's still checked for a character device, and opened
/// for append, so a surprise can never overwrite a file.
///
/// Claude draws to the same pty, and a write landing inside one of its escape
/// sequences garbles the screen. There's no way to write between its frames,
/// so wait out the redraw that comes with the status change; it goes quiet
/// once Claude stops or prompts.
pub const NOTIFY_SH: &str = r#"
pid=$1 title=$2 message=$3
name=$(ps -o tty= -p "$pid" 2>/dev/null | tr -d ' ')
case $name in
'' | '?' | '??') echo "pane process $pid has no terminal" >&2; exit 2 ;;
esac
tty=/dev/$name
[ -c "$tty" ] && [ -w "$tty" ] || { echo "pane tty '$tty' isn't a writable terminal" >&2; exit 2; }
sleep 0.3
printf '\033]777;notify;%s;%s\007' "$(printf %s "$title" | tr ';' ,)" "$(printf %s "$message" | tr ';' ,)" >>"$tty"
"#;

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use std::path::PathBuf;
    use std::process::Command;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// A scratch directory removed on drop, so a failed assert cleans up too.
    struct TempDir(PathBuf);

    impl TempDir {
        fn new(name: &str) -> Self {
            // Tests run in parallel, so each needs its own
            static NEXT: AtomicUsize = AtomicUsize::new(0);
            let n = NEXT.fetch_add(1, Ordering::Relaxed);
            let dir = std::env::temp_dir()
                .join(format!("mission-control-{name}-{}-{n}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            TempDir(dir)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// Runs the script for `pid`. With `ps_says`, a stub `ps` reports that as
    /// the terminal, so no test writes to a real one.
    fn notify(pid: &str, ps_says: Option<&str>) -> bool {
        let dir = TempDir::new("notify");
        let mut cmd = Command::new("sh");
        cmd.args([
            "-c",
            NOTIFY_SH,
            "mission-control-notify",
            pid,
            "Claude",
            "Done",
        ]);
        if let Some(name) = ps_says {
            let ps = dir.0.join("ps");
            std::fs::write(&ps, format!("#!/bin/sh\necho ' {name}'\n")).unwrap();
            std::fs::set_permissions(&ps, std::fs::Permissions::from_mode(0o755)).unwrap();
            let path = std::env::var("PATH").unwrap_or_default();
            cmd.env("PATH", format!("{}:{path}", dir.0.display()));
        }
        cmd.output().unwrap().status.success()
    }

    #[test]
    fn writes_to_the_pane_process_terminal() {
        assert!(notify("1234", Some("null")));
    }

    #[test]
    fn skips_processes_without_a_terminal() {
        // No such process, so the real ps prints nothing
        assert!(!notify("999999999", None));
        for name in ["", "?", "??", "nosuchtty"] {
            assert!(!notify("1234", Some(name)), "{name:?}");
        }
    }
}
