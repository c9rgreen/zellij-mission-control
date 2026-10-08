/// Posts a notification as an OSC escape code written to the Claude pane's pty.
/// Zellij forwards OSC 777 from a pane to the outer terminal (as OSC 9), so it
/// reaches the local terminal even over SSH; the terminal decides whether to
/// show it. Args: the pid Zellij reports for the pane, title, message.
///
/// Events come from a pipe any local process can write to. A forged event can
/// still pick which pane is notified, but not where the write goes: `ps` names
/// the pane process's own terminal, so it can only reach a pane's pty. That's
/// still checked for a character device, and opened for append, so a surprise
/// can never overwrite a file.
///
/// Busybox `ps` has no `-p`, so when `ps` names nothing, Linux reads the
/// controlling terminal from `/proc` instead: field 7 of `stat`, a device
/// number whose majors 136-143 are `/dev/pts`.
///
/// Claude draws to the same pty, and a write landing inside one of its escape
/// sequences garbles the screen. There's no way to write between its frames,
/// so wait out the redraw that comes with the status change; it goes quiet
/// once Claude stops or prompts.
pub const NOTIFY_SH: &str = r#"
pid=$1 title=$2 message=$3
name=$(ps -o tty= -p "$pid" 2>/dev/null | tr -d ' ')
if [ -z "$name" ] && stat=$(cat "/proc/$pid/stat" 2>/dev/null); then
    # The command name in field 2 can hold spaces and parens, so skip past it
    set -- ${stat##*) }
    dev=${5:-0}
    major=$(( (dev >> 8) & 0xfff )) minor=$(( (dev & 0xff) | ((dev >> 12) & 0xfff00) ))
    [ "$major" -ge 136 ] && [ "$major" -le 143 ] && name=pts/$(( (major - 136) * 256 + minor ))
fi
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
    use std::ffi::{c_char, c_int, c_short, CStr};
    use std::fs::{File, OpenOptions};
    use std::io::Read;
    use std::os::fd::AsRawFd;
    use std::os::unix::fs::PermissionsExt;
    use std::path::PathBuf;
    use std::process::{Command, Output};
    use std::sync::atomic::{AtomicUsize, Ordering};

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
                    .open(path)
                    .unwrap()
            };
            let master = rw("/dev/ptmx");
            let fd = master.as_raw_fd();
            // SAFETY: fd is an open pty master, and ptsname's buffer is copied
            // out before anything else can call it
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

        /// What's been written to the slave. Waits at most a few seconds, so
        /// a script that never writes fails the test instead of hanging it.
        fn written(&self) -> Vec<u8> {
            let mut fd = PollFd {
                fd: self.master.as_raw_fd(),
                events: POLLIN,
                revents: 0,
            };
            // SAFETY: one valid pollfd, and the master outlives the call
            let ready = unsafe { poll(&mut fd, 1, 5000) };
            assert_eq!(ready, 1, "nothing was written to the pty");
            let mut buf = [0; 256];
            let n = (&self.master).read(&mut buf).unwrap();
            buf[..n].to_vec()
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

    /// Runs the script for `pid`. With `stubs`, those commands come first on
    /// the PATH, so no test writes to a terminal it didn't open.
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
        // Lives until the command has finished
        let _dir = (!stubs.is_empty()).then(|| {
            let dir = TempDir::new();
            for (name, body) in stubs {
                dir.stub(name, body);
            }
            let path = std::env::var("PATH").unwrap_or_default();
            cmd.env("PATH", format!("{}:{path}", dir.0.display()));
            dir
        });
        cmd.output().unwrap()
    }

    /// A stub `ps` that names `tty` as the process's terminal.
    fn ps_says(tty: &str) -> String {
        format!("echo ' {tty}'")
    }

    #[test]
    fn writes_to_the_pane_process_terminal() {
        let pty = Pty::open();
        let out = notify(
            "1234",
            "Claude; api",
            "Done",
            &[("ps", &ps_says(&pty.name))],
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
        for name in ["", "?", "??", "nosuchtty"] {
            let out = notify("1234", "Claude", "Done", &[("ps", &ps_says(name))]);
            assert!(!out.status.success(), "{name:?}");
        }
        // ps and /proc both know nothing of it
        let out = notify(
            "1234",
            "Claude",
            "Done",
            &[("ps", "exit 1"), ("cat", "exit 1")],
        );
        assert!(!out.status.success());
    }

    #[test]
    fn falls_back_to_proc_without_ps_p() {
        // Busybox ps rejects -p. The stub stat has a command name with spaces
        // and parens, and a tty_nr of major 137, minor 44: /dev/pts/300.
        let stat = "echo '1234 (my (odd) sh) S 1 1234 1234 35116 1234 0'";
        let out = notify("1234", "Claude", "Done", &[("ps", "exit 1"), ("cat", stat)]);
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(err.contains("'/dev/pts/300'"), "{err}");

        // A tty_nr of 0 means no controlling terminal
        let stat = "echo '1234 (sh) S 1 1234 1234 0 -1 0'";
        let out = notify("1234", "Claude", "Done", &[("ps", "exit 1"), ("cat", stat)]);
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(err.contains("has no terminal"), "{err}");
    }
}
