/// Posts a notification as an OSC escape code written to the Claude pane's pty.
/// Zellij forwards OSC 777 from a pane to the outer terminal (as OSC 9), so it
/// reaches the local terminal even over SSH; the terminal decides whether to
/// show it. Args: the pid Zellij reports for the pane, title, message.
///
/// Events come from a pipe anything running as this user can write to, so a
/// stray or malformed event picks which pane is notified, but never where the
/// write goes: that's the pane process's own terminal, which can only be a
/// pane's pty while the process runs (the plugin skips panes whose command
/// has exited, as their pid may since have been reused). It's still checked
/// for a character device, and opened for append, so a surprise can never
/// overwrite a file.
///
/// Linux reads the controlling terminal from `/proc`, which doesn't vary like
/// `ps` does: field 7 of `stat`, a device number whose major 136 is
/// `/dev/pts`, with the minor as the index. Where there's no `/proc`, on macOS
/// or in a container without one, `ps` is asked, with its output expected as
/// macOS prints it; BSD `ps` abbreviates terminal names, which isn't handled,
/// so only Linux and macOS are supported. Either way only a pty, `pts/N` or
/// `ttysN`, is written to, as a pane is always on one: a process on any other
/// terminal has moved there, and the notification mustn't follow it.
///
/// Claude draws to the same pty, and a write landing inside one of its escape
/// sequences garbles the screen. There's no way to write between its frames,
/// so wait out the redraw that comes with the status change; it goes quiet
/// once Claude stops or prompts.
pub const NOTIFY_SH: &str = r#"
pid=$1 title=$2 message=$3
if stat=$(cat "/proc/$pid/stat" 2>/dev/null); then
    # The command name in field 2 can hold spaces and parens, so skip past it
    set -- ${stat##*) }
    dev=$5
    case ${dev#-} in
    '' | *[!0-9]*) echo "pane process $pid has an unreadable stat: $stat" >&2; exit 2 ;;
    esac
    [ "$dev" -ne 0 ] || { echo "pane process $pid has no terminal" >&2; exit 2; }
    # The major sits between the low and high bits of a 20-bit minor
    major=$(( (dev >> 8) & 0xfff )) minor=$(( (dev & 0xff) | ((dev >> 12) & 0xfff00) ))
    [ "$major" -eq 136 ] || { echo "pane process $pid's terminal (device $dev) isn't a pty" >&2; exit 2; }
    tty=/dev/pts/$minor
else
    name=$(ps -o tty= -p "$pid" 2>/dev/null | tr -d ' ')
    case $name in
    '' | '?' | '??' | '-') echo "pane process $pid has no terminal" >&2; exit 2 ;;
    ttys*) n=${name#ttys} ;;
    pts/*) n=${name#pts/} ;;
    esac
    # ttysNNN on macOS, or pts/N on a Linux without /proc, and nothing more
    case ${n:-x} in
    *[!0-9]*) echo "pane process $pid's terminal '$name' isn't a pty" >&2; exit 2 ;;
    esac
    tty=/dev/$name
fi
[ -c "$tty" ] && [ -w "$tty" ] || { echo "pane tty '$tty' isn't a writable terminal" >&2; exit 2; }
sleep 0.3
printf '\033]777;notify;%s;%s\007' "$(printf %s "$title" | tr ';' ,)" "$(printf %s "$message" | tr ';' ,)" >>"$tty"
"#;

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::ffi::{c_char, c_int, c_short, CStr};
    use std::fs::{File, OpenOptions};
    use std::io::Read;
    use std::os::fd::AsRawFd;
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
    use std::path::PathBuf;
    use std::process::{Command, Output};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Mutex;
    use std::time::{Duration, Instant};

    extern "C" {
        fn grantpt(fd: c_int) -> c_int;
        fn unlockpt(fd: c_int) -> c_int;
        fn ptsname(fd: c_int) -> *const c_char;
        fn poll(fds: *mut PollFd, nfds: Nfds, timeout: c_int) -> c_int;
    }

    #[cfg(target_os = "linux")]
    type Nfds = std::ffi::c_ulong;
    #[cfg(not(target_os = "linux"))]
    type Nfds = std::ffi::c_uint;

    /// `struct pollfd`, laid out the same on macOS and Linux.
    #[repr(C)]
    struct PollFd {
        fd: c_int,
        events: c_short,
        revents: c_short,
    }

    const POLLIN: c_short = 1;

    /// Opening the slave must not make it the controlling terminal, or a
    /// test run that is a session leader without one (under setsid, say)
    /// adopts it and is hung up when the Pty is dropped.
    #[cfg(target_os = "linux")]
    const O_NOCTTY: c_int = 0o400;
    #[cfg(not(target_os = "linux"))]
    const O_NOCTTY: c_int = 0x20000;

    /// A pseudo-terminal, so a test can read back what the script wrote. The
    /// slave stays open so the pty isn't hung up when the script closes it.
    struct Pty {
        master: File,
        _slave: File,
        /// The slave's name under /dev, as `ps` prints it.
        name: String,
    }

    impl Pty {
        fn open() -> Self {
            let rw = |path: &str| {
                OpenOptions::new()
                    .read(true)
                    .write(true)
                    .custom_flags(O_NOCTTY)
                    .open(path)
                    .unwrap()
            };
            // ptsname returns a static buffer, and tests run in parallel
            static PTSNAME: Mutex<()> = Mutex::new(());
            let master = rw("/dev/ptmx");
            let fd = master.as_raw_fd();
            let _lock = PTSNAME.lock().unwrap_or_else(|e| e.into_inner());
            // SAFETY: fd is an open pty master, and the lock keeps any other
            // ptsname call from reusing the buffer before it's copied out
            let path = unsafe {
                assert_eq!(grantpt(fd), 0);
                assert_eq!(unlockpt(fd), 0);
                CStr::from_ptr(ptsname(fd)).to_str().unwrap().to_owned()
            };
            Pty {
                _slave: rw(&path),
                name: path.strip_prefix("/dev/").unwrap().to_owned(),
                master,
            }
        }

        /// What's been written to the slave, up to the BEL that ends a
        /// notification, which can arrive in pieces. Waits at most a few
        /// seconds, so a script that never writes fails the test instead of
        /// hanging it.
        fn written(&self) -> Vec<u8> {
            let deadline = Instant::now() + Duration::from_secs(5);
            let mut out = Vec::new();
            while out.last() != Some(&0x07) {
                let left = deadline.saturating_duration_since(Instant::now());
                let mut fd = PollFd {
                    fd: self.master.as_raw_fd(),
                    events: POLLIN,
                    revents: 0,
                };
                // SAFETY: one valid pollfd, and the master outlives the call
                let ready = unsafe { poll(&mut fd, 1, left.as_millis() as c_int) };
                if ready < 0
                    && std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted
                {
                    continue;
                }
                if ready != 1 {
                    break;
                }
                let mut buf = [0; 256];
                let n = (&self.master).read(&mut buf).unwrap();
                if n == 0 {
                    // Hung up, so nothing more is coming
                    break;
                }
                out.extend_from_slice(&buf[..n]);
            }
            assert!(!out.is_empty(), "nothing was written to the pty");
            out
        }
    }

    /// A scratch directory removed on drop, so a failed assert cleans up too.
    struct TempDir(PathBuf);

    impl TempDir {
        fn new() -> Self {
            // Tests run in parallel, so each needs its own
            static NEXT: AtomicUsize = AtomicUsize::new(0);
            let n = NEXT.fetch_add(1, Ordering::Relaxed);
            let dir = std::env::temp_dir()
                .join(format!("mission-control-notify-{}-{n}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            TempDir(dir)
        }

        /// Puts an executable `name` that runs `body` in the directory.
        fn stub(&self, name: &str, body: &str) {
            let path = self.0.join(name);
            std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// Runs the script for `pid`, with `stubs` first on the PATH. Every test
    /// stubs both `ps` and `cat`, so none reads a real process's terminal and
    /// writes to a terminal it didn't open.
    fn notify(pid: &str, title: &str, message: &str, stubs: &[(&str, &str)]) -> Output {
        let mut cmd = Command::new("sh");
        cmd.args([
            "-c",
            NOTIFY_SH,
            "mission-control-notify",
            pid,
            title,
            message,
        ]);
        for name in ["ps", "cat"] {
            assert!(stubs.iter().any(|(n, _)| *n == name), "stub {name}");
        }
        // Lives until the command has finished
        let dir = TempDir::new();
        for (name, body) in stubs {
            dir.stub(name, body);
        }
        let path = std::env::var("PATH").unwrap_or_default();
        cmd.env("PATH", format!("{}:{path}", dir.0.display()));
        cmd.output().unwrap()
    }

    /// A stub `ps` that names `tty` as the process's terminal.
    fn ps_says(tty: &str) -> String {
        format!("echo ' {tty}'")
    }

    /// A stub `cat` for a system without `/proc`, so `ps` is asked.
    const NO_PROC: (&str, &str) = ("cat", "exit 1");

    #[test]
    fn writes_to_the_pane_process_terminal() {
        let pty = Pty::open();
        let out = notify(
            "1234",
            "Claude; api",
            "Done",
            &[("ps", &ps_says(&pty.name)), NO_PROC],
        );
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert_eq!(pty.written(), b"\x1b]777;notify;Claude, api;Done\x07");
    }

    #[test]
    fn skips_processes_without_a_terminal() {
        for name in ["", "?", "??", "-", "nosuchtty"] {
            let out = notify("1234", "Claude", "Done", &[("ps", &ps_says(name)), NO_PROC]);
            assert!(!out.status.success(), "{name:?}");
        }
        // ps and /proc both know nothing of it
        let out = notify("1234", "Claude", "Done", &[("ps", "exit 1"), NO_PROC]);
        assert!(!out.status.success());
    }

    #[test]
    fn writes_only_to_a_pty() {
        // A writable regular file, named relative to /dev as ps would
        let dir = TempDir::new();
        let file = dir.0.join("not-a-tty");
        std::fs::write(&file, "").unwrap();
        let name = format!("..{}", file.display());
        let out = notify(
            "1234",
            "Claude",
            "Done",
            &[("ps", &ps_says(&name)), NO_PROC],
        );
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(err.contains("isn't a pty"), "{err}");
        assert_eq!(std::fs::read(&file).unwrap(), b"");

        // Terminals that aren't a pty, and pty names with something appended
        for name in ["console", "tty1", "ttys", "pts/", "ttys1/../console"] {
            let out = notify("1234", "Claude", "Done", &[("ps", &ps_says(name)), NO_PROC]);
            let err = String::from_utf8_lossy(&out.stderr);
            assert!(err.contains("isn't a pty"), "{name}: {err}");
        }

        // A pty name that no device answers to
        let out = notify(
            "1234",
            "Claude",
            "Done",
            &[("ps", &ps_says("pts/1050367")), NO_PROC],
        );
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(
            err.contains("'/dev/pts/1050367' isn't a writable terminal"),
            "{err}"
        );
    }

    #[test]
    fn reads_the_terminal_from_proc_before_ps() {
        // A ps that names a terminal, which /proc overrules
        let ps_body = ps_says("ttys001");
        let ps = ("ps", ps_body.as_str());

        // A command name with spaces and parens, and a tty_nr of major 136,
        // minor 0xfffff, printed signed as the kernel does. That's pts index
        // 1048575, the last a kernel can hand out, which no real terminal has.
        let stat = "echo '1234 (my (odd) sh) S 1 1234 1234 -1013505 1234 0'";
        let out = notify("1234", "Claude", "Done", &[ps, ("cat", stat)]);
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(err.contains("'/dev/pts/1048575'"), "{err}");

        // A tty_nr of 0 means no controlling terminal, whatever ps says
        let stat = "echo '1234 (sh) S 1 1234 1234 0 -1 0'";
        let out = notify("1234", "Claude", "Done", &[ps, ("cat", stat)]);
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(err.contains("has no terminal"), "{err}");

        // Major 4, minor 1 is the console tty1: a terminal, but not a pane's
        // pts, so it's refused without asking ps
        let stat = "echo '1234 (sh) S 1 1234 1234 1025 -1 0'";
        let out = notify("1234", "Claude", "Done", &[ps, ("cat", stat)]);
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(err.contains("(device 1025) isn't a pty"), "{err}");

        // A stat that isn't laid out as expected is reported as such, rather
        // than as the shell's complaint about a non-number
        for stat in ["echo garbage", "echo '1234 (sh) S 1 1234 1234 x7 -1 0'"] {
            let out = notify("1234", "Claude", "Done", &[ps, ("cat", stat)]);
            let err = String::from_utf8_lossy(&out.stderr);
            assert!(err.contains("has an unreadable stat"), "{stat}: {err}");
            assert!(!err.contains("expected"), "{stat}: {err}");
        }
    }
}
