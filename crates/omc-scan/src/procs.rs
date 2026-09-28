//! Running processes, used to skip caches of running apps and to quit apps before they are
//! uninstalled. Read from `/proc` on Linux and from `ps`/`tasklist` elsewhere (no `unsafe`
//! process APIs).

use std::path::PathBuf;
use std::process::Command;

/// A running process.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Process {
    /// Process id.
    pub pid: u32,
    /// Executable file name (`Google Chrome`, `chrome.exe`, `firefox`).
    pub name: String,
    /// Full executable path, when the OS reveals it (not on Windows).
    pub exe: Option<PathBuf>,
}

/// Every process visible to this user. Empty when the listing fails (logged).
pub fn running() -> Vec<Process> {
    #[cfg(target_os = "linux")]
    {
        linux()
    }
    #[cfg(windows)]
    {
        windows()
    }
    #[cfg(not(any(target_os = "linux", windows)))]
    {
        unix_ps()
    }
}

#[cfg(target_os = "linux")]
fn linux() -> Vec<Process> {
    let Ok(dir) = std::fs::read_dir("/proc") else {
        return unix_ps();
    };
    let mut out = Vec::new();
    for entry in dir.flatten() {
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|s| s.parse::<u32>().ok())
        else {
            continue;
        };
        let exe = std::fs::read_link(entry.path().join("exe")).ok();
        let name = std::fs::read_to_string(entry.path().join("comm"))
            .map(|s| s.trim_end().to_owned())
            .ok()
            .or_else(|| {
                exe.as_ref()
                    .and_then(|e| e.file_name())
                    .map(|n| n.to_string_lossy().into_owned())
            });
        if let Some(name) = name {
            out.push(Process { pid, name, exe });
        }
    }
    out
}

#[cfg(not(windows))]
fn unix_ps() -> Vec<Process> {
    let output = match Command::new("ps")
        .args(["-axww", "-o", "pid=,comm="])
        .output()
    {
        Ok(output) if output.status.success() => output,
        Ok(output) => {
            tracing::warn!(status = %output.status, "ps failed");
            return Vec::new();
        }
        Err(err) => {
            tracing::warn!(%err, "cannot run ps");
            return Vec::new();
        }
    };
    parse_ps(&String::from_utf8_lossy(&output.stdout))
}

/// Parses `ps -o pid=,comm=` output (`comm` is the full path on macOS).
#[cfg(any(not(windows), test))]
fn parse_ps(text: &str) -> Vec<Process> {
    text.lines()
        .filter_map(|line| {
            let line = line.trim_start();
            let (pid, comm) = line.split_once(char::is_whitespace)?;
            let pid = pid.parse().ok()?;
            let comm = comm.trim();
            if comm.is_empty() {
                return None;
            }
            let path = PathBuf::from(comm);
            let (name, exe) = if path.is_absolute() {
                let name = path.file_name()?.to_string_lossy().into_owned();
                (name, Some(path))
            } else {
                (comm.to_owned(), None)
            };
            Some(Process { pid, name, exe })
        })
        .collect()
}

#[cfg(windows)]
fn windows() -> Vec<Process> {
    use std::os::windows::process::CommandExt as _;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let output = match Command::new("tasklist")
        .args(["/fo", "csv", "/nh"])
        .creation_flags(CREATE_NO_WINDOW)
        .output()
    {
        Ok(output) if output.status.success() => output,
        Ok(output) => {
            tracing::warn!(status = %output.status, "tasklist failed");
            return Vec::new();
        }
        Err(err) => {
            tracing::warn!(%err, "cannot run tasklist");
            return Vec::new();
        }
    };
    parse_tasklist(&String::from_utf8_lossy(&output.stdout))
}

/// Parses `tasklist /fo csv /nh`: `"name","pid","session","#","mem"`.
#[cfg(any(windows, test))]
fn parse_tasklist(text: &str) -> Vec<Process> {
    text.lines()
        .filter_map(|line| {
            let mut fields = line.split("\",\"");
            let name = fields.next()?.trim().trim_start_matches('"').to_owned();
            let pid = fields.next()?.trim_matches('"').parse().ok()?;
            (!name.is_empty()).then_some(Process {
                pid,
                name,
                exe: None,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ps_output_parses_paths_and_bare_names() {
        let got = parse_ps(
            "  1 /sbin/launchd\n 812 /Applications/Google Chrome.app/Contents/MacOS/Google Chrome\n 90 kernel_task\nbogus\n",
        );
        assert_eq!(got.len(), 3, "three valid lines: {got:?}");
        assert!(
            got.iter()
                .any(|p| p.pid == 812 && p.name == "Google Chrome" && p.exe.is_some()),
            "names with spaces keep their path: {got:?}"
        );
        assert!(
            got.iter()
                .any(|p| p.pid == 90 && p.name == "kernel_task" && p.exe.is_none()),
            "bare names have no path"
        );
    }

    #[test]
    fn tasklist_csv_parses() {
        let got = parse_tasklist(
            "\"System Idle Process\",\"0\",\"Services\",\"0\",\"8 K\"\r\n\"chrome.exe\",\"4242\",\"Console\",\"1\",\"120,000 K\"\r\n",
        );
        assert_eq!(
            got.get(1).map(|p| (p.name.as_str(), p.pid)),
            Some(("chrome.exe", 4242)),
            "name and pid: {got:?}"
        );
    }
}
