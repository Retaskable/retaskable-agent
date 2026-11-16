//! The computer's processes on Linux and macOS, and signalling one, with no tool the machine may not have: the agent's
//! Docker image (`node:22-bookworm-slim`) and the CI runner's (`debian:bookworm-slim`) carry no `ps`, and their `kill`
//! is the shell's, not a program. On Linux the table is read from `/proc`; on macOS (no `/proc`) it is `ps`'s, which
//! every Mac has. A signal goes through `kill(2)`. Windows has its own listing (`lanes.rs`).
use std::io;

/// One process, as `ps -axo pid=,ppid=,pgid=,state=,args=` would give it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Proc {
    pub pid: u32,
    pub ppid: u32,
    pub pgid: u32,
    /// The state letter (`R`, `S`, `Z` for a zombie that has exited and isn't reaped yet, ...).
    pub state: char,
    /// The command line, its arguments joined by spaces; `[name]` for a process with none (a kernel thread, a zombie).
    pub args: String,
}

/// Every process now. A process that exits while the table is read is left out.
pub fn all() -> io::Result<Vec<Proc>> {
    #[cfg(target_os = "linux")]
    {
        read_proc(std::path::Path::new("/proc"))
    }
    #[cfg(not(target_os = "linux"))]
    {
        from_ps()
    }
}

/// Sends `signal` (`libc::SIGTERM`, ...) to `pid`; a negative `pid` is the process group `-pid`, as with `kill -- -pgid`.
/// An error is the process (or group) not being there, or not being ours to signal.
pub fn signal(pid: i32, signal: i32) -> io::Result<()> {
    // SAFETY: `kill` takes two integers and touches no memory of ours.
    if unsafe { libc::kill(pid, signal) } == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

/// The table out of a `/proc`-shaped tree at `root`: each numbered folder's `stat` and `cmdline`.
#[cfg(any(target_os = "linux", test))]
fn read_proc(root: &std::path::Path) -> io::Result<Vec<Proc>> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir(root)? {
        let Ok(entry) = entry else { continue };
        let Some(pid) = entry.file_name().to_str().and_then(|n| n.parse::<u32>().ok()) else {
            continue;
        };
        // Gone between the listing and the read: not there any more.
        let Ok(stat) = std::fs::read_to_string(entry.path().join("stat")) else {
            continue;
        };
        let Some((name, state, ppid, pgid)) = parse_stat(&stat) else {
            continue;
        };
        let cmdline = std::fs::read(entry.path().join("cmdline")).unwrap_or_default();
        let args = cmdline
            .split(|b| *b == 0)
            .filter(|part| !part.is_empty())
            .map(|part| String::from_utf8_lossy(part).into_owned())
            .collect::<Vec<_>>()
            .join(" ");
        out.push(Proc {
            pid,
            ppid,
            pgid,
            state,
            args: if args.is_empty() { format!("[{name}]") } else { args },
        });
    }
    Ok(out)
}

/// `(name, state, ppid, pgid)` from `/proc/<pid>/stat`: `pid (name) state ppid pgrp ...`. The name is in parentheses and
/// may hold spaces and parentheses itself, so it ends at the last `)`.
#[cfg(any(target_os = "linux", test))]
fn parse_stat(stat: &str) -> Option<(&str, char, u32, u32)> {
    let open = stat.find('(')?;
    let close = stat.rfind(')')?;
    let name = stat.get(open + 1..close)?;
    let mut rest = stat.get(close + 1..)?.split_whitespace();
    let state = rest.next()?.chars().next()?;
    let ppid = rest.next()?.parse().ok()?;
    let pgid = rest.next()?.parse().ok()?;
    Some((name, state, ppid, pgid))
}

#[cfg(not(target_os = "linux"))]
fn from_ps() -> io::Result<Vec<Proc>> {
    let output = std::process::Command::new("ps")
        .args(["-axo", "pid=,ppid=,pgid=,state=,args="])
        .output()?;
    Ok(String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(parse_ps_line)
        .collect())
}

/// One `pid ppid pgid state args` line of `ps`.
#[cfg(any(not(target_os = "linux"), test))]
fn parse_ps_line(line: &str) -> Option<Proc> {
    let mut rest = line.trim_start();
    let mut field = || {
        let end = rest.find(char::is_whitespace).unwrap_or(rest.len());
        let (head, tail) = rest.split_at(end);
        rest = tail.trim_start();
        (!head.is_empty()).then_some(head)
    };
    let pid = field()?.parse().ok()?;
    let ppid = field()?.parse().ok()?;
    let pgid = field()?.parse().ok()?;
    let state = field()?.chars().next()?;
    Some(Proc {
        pid,
        ppid,
        pgid,
        state,
        args: rest.trim_end().to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::Path;

    fn add(root: &Path, pid: &str, stat: &str, cmdline: &[u8]) {
        let dir = root.join(pid);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("stat"), stat).unwrap();
        fs::write(dir.join("cmdline"), cmdline).unwrap();
    }

    fn table(root: &Path) -> Vec<Proc> {
        let mut all = read_proc(root).unwrap();
        all.sort_by_key(|p| p.pid);
        all
    }

    #[test]
    fn a_proc_tree_is_read_into_pid_parent_group_state_and_command_line() {
        let root = tempfile::tempdir().unwrap();
        add(
            root.path(),
            "10",
            "10 (node) S 1 10 10 0 -1 4194560 0",
            b"node\0/work/lane-1/server.js\0--port\x003000\0",
        );
        add(root.path(), "11", "11 (sh) R 10 10 10 0 -1 0", b"sh\0-c\0sleep 5\0");
        assert_eq!(
            table(root.path()),
            vec![
                Proc {
                    pid: 10,
                    ppid: 1,
                    pgid: 10,
                    state: 'S',
                    args: "node /work/lane-1/server.js --port 3000".into()
                },
                Proc {
                    pid: 11,
                    ppid: 10,
                    pgid: 10,
                    state: 'R',
                    args: "sh -c sleep 5".into()
                },
            ]
        );
    }

    #[test]
    fn a_name_with_spaces_and_parentheses_does_not_shift_the_fields() {
        let root = tempfile::tempdir().unwrap();
        add(root.path(), "20", "20 (a) b (c)) S 7 9 9 0 -1", b"a\0");
        let p = &table(root.path())[0];
        assert_eq!((p.pid, p.ppid, p.pgid, p.state), (20, 7, 9, 'S'));
    }

    #[test]
    fn a_zombie_and_a_kernel_thread_have_no_command_line_and_show_their_name() {
        let root = tempfile::tempdir().unwrap();
        add(root.path(), "30", "30 (worker) Z 10 10 10 0 -1", b"");
        add(root.path(), "2", "2 (kthreadd) S 0 0 0 0 -1", b"");
        let all = table(root.path());
        assert_eq!(all[0].args, "[kthreadd]");
        assert_eq!((all[1].state, all[1].args.as_str()), ('Z', "[worker]"));
    }

    #[test]
    fn what_is_not_a_process_or_has_gone_or_is_unreadable_is_left_out() {
        let root = tempfile::tempdir().unwrap();
        add(root.path(), "40", "40 (ok) S 1 40 40 0 -1", b"ok\0");
        fs::create_dir_all(root.path().join("self")).unwrap();
        fs::create_dir_all(root.path().join("sys")).unwrap();
        fs::write(root.path().join("uptime"), "1 2").unwrap();
        fs::create_dir_all(root.path().join("41")).unwrap(); // exited before its stat was read
        add(root.path(), "42", "garbage", b"x\0"); // a stat that isn't one
        add(root.path(), "43", "43 (short) S", b"x\0");
        assert_eq!(table(root.path()).iter().map(|p| p.pid).collect::<Vec<_>>(), [40]);
    }

    #[test]
    fn a_missing_proc_folder_is_an_error_not_an_empty_table() {
        let root = tempfile::tempdir().unwrap();
        assert!(read_proc(&root.path().join("nope")).is_err());
    }

    #[test]
    fn a_ps_line_keeps_the_arguments_whole() {
        let p = parse_ps_line("  812   1  812 Ss   /usr/bin/node  /work/lane-1/a b.js ").unwrap();
        assert_eq!((p.pid, p.ppid, p.pgid, p.state), (812, 1, 812, 'S'));
        assert_eq!(p.args, "/usr/bin/node  /work/lane-1/a b.js");
        assert_eq!(
            parse_ps_line("12 1 12 Z"),
            Some(Proc {
                pid: 12,
                ppid: 1,
                pgid: 12,
                state: 'Z',
                args: String::new()
            })
        );
        assert!(parse_ps_line("PID PPID").is_none());
        assert!(parse_ps_line("").is_none());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn the_real_proc_lists_this_process_in_its_own_group() {
        let me = std::process::id();
        let all = all().unwrap();
        let mine = all.iter().find(|p| p.pid == me).expect("this process");
        assert!(mine.state != 'Z' && !mine.args.is_empty());
        assert_eq!(mine.ppid, std::os::unix::process::parent_id());
    }

    #[test]
    fn signalling_a_process_that_is_gone_is_an_error() {
        let mut child = std::process::Command::new("sleep").arg("30").spawn().unwrap();
        let pid = child.id() as i32;
        signal(pid, libc::SIGTERM).unwrap();
        child.wait().unwrap();
        assert!(signal(pid, 0).is_err());
    }
}
