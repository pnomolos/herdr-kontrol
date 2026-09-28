//! Raise the GUI app hosting a herdr TUI client (Ghostty, Kitty, iTerm, Zed, VS Code, …).

use std::process::{Command, Stdio};

use tracing::{debug, info, warn};

#[derive(Clone, Debug, Eq, PartialEq)]
struct Proc {
    pid: u32,
    ppid: u32,
    command: String,
}

pub fn raise(window_hint: Option<&str>) {
    match raise_inner(window_hint) {
        Ok(pid) => info!(pid, hint = window_hint.unwrap_or(""), "raised herdr host"),
        Err(e) => debug!(%e, "host raise skipped"),
    }
}

fn raise_inner(window_hint: Option<&str>) -> Result<u32, String> {
    let procs = list_procs()?;
    let gui = gui_pid_for_herdr(&procs).ok_or_else(|| "no herdr TUI host".to_string())?;
    activate_pid(gui, window_hint)?;
    Ok(gui)
}

fn list_procs() -> Result<Vec<Proc>, String> {
    let out = Command::new("ps")
        .args(["-axo", "pid=,ppid=,command="])
        .output()
        .map_err(|e| e.to_string())?;
    if !out.status.success() {
        return Err("ps failed".into());
    }
    Ok(parse_ps(&String::from_utf8_lossy(&out.stdout)))
}

fn parse_ps(text: &str) -> Vec<Proc> {
    let mut out = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let mut parts = line.split_whitespace();
        let Some(pid) = parts.next().and_then(|s| s.parse().ok()) else {
            continue;
        };
        let Some(ppid) = parts.next().and_then(|s| s.parse().ok()) else {
            continue;
        };
        let command = parts.collect::<Vec<_>>().join(" ");
        if command.is_empty() {
            continue;
        }
        out.push(Proc { pid, ppid, command });
    }
    out
}

fn is_herdr_tui(command: &str) -> bool {
    let cmd = command.trim();
    if cmd.contains("herdr-kontrol") || cmd.contains("herdr server") {
        return false;
    }
    let base = cmd.split_whitespace().next().unwrap_or("");
    base.ends_with("/herdr") || base == "herdr"
}

fn is_gui_app(command: &str) -> bool {
    command.contains(".app/Contents/MacOS/") && !command.contains("Helper")
}

fn gui_pid_for_herdr(procs: &[Proc]) -> Option<u32> {
    let by_pid: std::collections::HashMap<u32, &Proc> = procs.iter().map(|p| (p.pid, p)).collect();
    for p in procs {
        if !is_herdr_tui(&p.command) {
            continue;
        }
        let mut cur = p.ppid;
        let mut hops = 0;
        while hops < 16 {
            let Some(parent) = by_pid.get(&cur) else {
                break;
            };
            if is_gui_app(&parent.command) {
                return Some(parent.pid);
            }
            if parent.ppid == cur || parent.ppid == 0 {
                break;
            }
            cur = parent.ppid;
            hops += 1;
        }
    }
    None
}

fn activate_pid(pid: u32, window_hint: Option<&str>) -> Result<(), String> {
    let hint = window_hint.unwrap_or("");
    let mut child = Command::new("osascript")
        .args(["-", &pid.to_string(), hint])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| e.to_string())?;
    if let Some(mut stdin) = child.stdin.take() {
        use std::io::Write;
        stdin
            .write_all(RAISE_SCRIPT.as_bytes())
            .map_err(|e| e.to_string())?;
    }
    let out = child.wait_with_output().map_err(|e| e.to_string())?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        warn!(pid, %err, "osascript raise failed");
        return Err(err.trim().to_string());
    }
    Ok(())
}

const RAISE_SCRIPT: &str = r#"
on run argv
  set pidNum to item 1 of argv as integer
  set hint to ""
  if (count of argv) is greater than 1 then set hint to item 2 of argv
  tell application "System Events"
    set proc to first application process whose unix id is pidNum
    set frontmost of proc to true
    if hint is not "" then
      try
        repeat with w in windows of proc
          if name of w contains hint then
            perform action "AXRaise" of w
            exit repeat
          end if
        end repeat
      end try
    end if
  end tell
end run
"#;

#[cfg(test)]
mod tests {
    use super::*;

    fn p(pid: u32, ppid: u32, command: &str) -> Proc {
        Proc {
            pid,
            ppid,
            command: command.into(),
        }
    }

    #[test]
    fn herdr_tui_not_server_or_kontrol() {
        assert!(is_herdr_tui("/opt/homebrew/bin/herdr"));
        assert!(is_herdr_tui("herdr"));
        assert!(!is_herdr_tui("herdr server"));
        assert!(!is_herdr_tui(
            "/Users/me/Projects/herdr-kontrol/target/release/herdr-kontrol"
        ));
    }

    #[test]
    fn walks_shell_to_ghostty() {
        let procs = vec![
            p(1, 0, "/sbin/launchd"),
            p(917, 1, "/Applications/Ghostty.app/Contents/MacOS/ghostty"),
            p(3660, 917, "/usr/bin/login"),
            p(3700, 3660, "-zsh"),
            p(3800, 3700, "/opt/homebrew/bin/herdr"),
            p(13006, 1, "herdr server"),
        ];
        assert_eq!(gui_pid_for_herdr(&procs), Some(917));
    }

    #[test]
    fn walks_to_zed() {
        let procs = vec![
            p(1, 0, "/sbin/launchd"),
            p(500, 1, "/Applications/Zed.app/Contents/MacOS/zed"),
            p(510, 500, "/bin/zsh"),
            p(520, 510, "herdr"),
        ];
        assert_eq!(gui_pid_for_herdr(&procs), Some(500));
    }

    #[test]
    fn walks_to_vscode_not_helper() {
        let procs = vec![
            p(1, 0, "/sbin/launchd"),
            p(200, 1, "/Applications/Visual Studio Code.app/Contents/MacOS/Electron"),
            p(210, 200, "/Applications/Visual Studio Code.app/Contents/Frameworks/Code Helper.app/Contents/MacOS/Code Helper"),
            p(220, 210, "/bin/zsh"),
            p(230, 220, "/opt/homebrew/bin/herdr"),
        ];
        assert_eq!(gui_pid_for_herdr(&procs), Some(200));
    }

    #[test]
    fn parse_ps_lines() {
        let procs = parse_ps("  917   1 /Applications/Ghostty.app/Contents/MacOS/ghostty\n 3800 3700 /opt/homebrew/bin/herdr\n");
        assert_eq!(procs.len(), 2);
        assert_eq!(procs[0].pid, 917);
        assert_eq!(procs[1].ppid, 3700);
    }
}
