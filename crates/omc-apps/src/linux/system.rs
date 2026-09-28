//! System facts (`/etc/os-release`, `id -u`, `df -kP`) and emptying the XDG Trash.

use std::path::Path;

use omc_proto::settings::{Access, Os, SystemInfo, Volume};

use super::common::{self, QUICK_TIMEOUT};
use crate::{Error, Result};

/// `PRETTY_NAME` (else `NAME VERSION_ID`) of an os-release file.
pub(super) fn parse_os_release(text: &str) -> Option<String> {
    let mut pretty = None;
    let mut name = None;
    let mut version = None;
    for line in text.lines() {
        let Some((key, value)) = line.trim().split_once('=') else {
            continue;
        };
        let value = value.trim().trim_matches(['"', '\'']).trim().to_owned();
        if value.is_empty() {
            continue;
        }
        match key.trim() {
            "PRETTY_NAME" => pretty = Some(value),
            "NAME" => name = Some(value),
            "VERSION_ID" => version = Some(value),
            _ => {}
        }
    }
    pretty.or_else(|| match (name, version) {
        (Some(n), Some(v)) => Some(format!("{n} {v}")),
        (n, _) => n,
    })
}

/// Mounts that are not user storage.
fn skip_mount(mount: &str) -> bool {
    mount.starts_with("/boot")
        || mount.starts_with("/snap/")
        || mount.starts_with("/run/")
        || mount.starts_with("/dev")
        || mount.starts_with("/sys")
        || mount.starts_with("/proc")
        || mount.starts_with("/var/lib/snapd")
        || mount.starts_with("/var/lib/docker")
}

/// Parses `df -kP` output (1024-byte blocks; the mount point may contain spaces). One
/// volume per device (btrfs subvolumes share one).
pub(super) fn parse_df(text: &str) -> Vec<Volume> {
    let mut out: Vec<Volume> = Vec::new();
    let mut devices: Vec<String> = Vec::new();
    for line in text.lines().skip(1) {
        let tokens: Vec<&str> = line.split_whitespace().collect();
        // Capacity is the first `NN%` token after three numbers.
        let Some(cap) = tokens.iter().enumerate().position(|(i, t)| {
            i >= 4
                && t.strip_suffix('%')
                    .is_some_and(|n| n == "-" || n.parse::<u32>().is_ok())
        }) else {
            continue;
        };
        let (Some(total), Some(avail)) = (
            cap.checked_sub(3).and_then(|i| tokens.get(i)),
            cap.checked_sub(1).and_then(|i| tokens.get(i)),
        ) else {
            continue;
        };
        let (Ok(total), Ok(avail)) = (total.parse::<u64>(), avail.parse::<u64>()) else {
            continue;
        };
        let device = tokens
            .get(..cap.saturating_sub(3))
            .map(|d| d.join(" "))
            .unwrap_or_default();
        let Some(cap_token) = tokens.get(cap) else {
            continue;
        };
        // Mount point: the rest of the line after the capacity token, spaces preserved.
        let mount = line
            .find(&format!(" {cap_token} "))
            .and_then(|at| line.get(at.saturating_add(cap_token.len()).saturating_add(2)..))
            .map(str::trim)
            .unwrap_or_default();
        if mount.is_empty() || total == 0 || skip_mount(mount) {
            continue;
        }
        if device.starts_with('/') && devices.contains(&device) {
            continue;
        }
        devices.push(device.clone());
        let name = Path::new(&device)
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .filter(|_| device.starts_with('/'))
            .unwrap_or(device);
        out.push(Volume {
            mount: mount.to_owned(),
            name,
            total: total.saturating_mul(1024),
            free: avail.saturating_mul(1024),
        });
    }
    out
}

/// The daemon runs as root.
pub(super) fn is_root() -> bool {
    common::run_stdout("id", &["-u"], QUICK_TIMEOUT, true).is_some_and(|out| out.trim() == "0")
}

pub(super) fn system_info() -> SystemInfo {
    let os_version = ["/etc/os-release", "/usr/lib/os-release"]
        .iter()
        .find_map(|p| std::fs::read_to_string(p).ok())
        .and_then(|t| parse_os_release(&t))
        .or_else(|| {
            common::run_stdout("uname", &["-sr"], QUICK_TIMEOUT, true).map(|s| s.trim().to_owned())
        })
        .unwrap_or_default();
    let volumes = common::run_stdout(
        "df",
        &[
            "-kP", "-x", "tmpfs", "-x", "devtmpfs", "-x", "squashfs", "-x", "overlay", "-x",
            "efivarfs",
        ],
        QUICK_TIMEOUT,
        false,
    )
    .map(|t| parse_df(&t))
    .unwrap_or_default();
    SystemInfo {
        os: Os::Linux,
        os_version,
        home: common::home()
            .map(|h| h.display().to_string())
            .unwrap_or_default(),
        elevated: is_root(),
        full_disk_access: Access::NotApplicable,
        volumes,
    }
}

/// Deletes the contents of `~/.local/share/Trash/{files,info,expunged}` and the
/// `directorysizes` cache; fails with the first error after trying everything.
pub(super) fn empty_trash() -> Result<()> {
    let trash = common::data_home()
        .map(|d| d.join("Trash"))
        .ok_or_else(|| Error::Unsupported("no home folder".to_owned()))?;
    let mut first_err: Option<std::io::Error> = None;
    for sub in ["files", "info", "expunged"] {
        for child in common::children(&trash.join(sub)) {
            let result = match std::fs::symlink_metadata(&child) {
                Ok(meta) if meta.is_dir() => std::fs::remove_dir_all(&child),
                Ok(_) => std::fs::remove_file(&child),
                Err(err) => Err(err),
            };
            if let Err(err) = result
                && err.kind() != std::io::ErrorKind::NotFound
            {
                tracing::warn!(%err, path = %child.display(), "cannot empty trash entry");
                first_err.get_or_insert(err);
            }
        }
    }
    let sizes = trash.join("directorysizes");
    if let Err(err) = std::fs::remove_file(&sizes)
        && err.kind() != std::io::ErrorKind::NotFound
    {
        tracing::debug!(%err, "trash directorysizes");
    }
    first_err.map_or(Ok(()), |e| Err(Error::Io(e)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn os_release_prefers_pretty_name() {
        assert_eq!(
            parse_os_release(
                "NAME=\"Ubuntu\"\nVERSION_ID=\"24.04\"\nPRETTY_NAME=\"Ubuntu 24.04.1 LTS\"\n"
            ),
            Some("Ubuntu 24.04.1 LTS".to_owned()),
            "pretty name"
        );
        assert_eq!(
            parse_os_release("NAME=Arch\nVERSION_ID=rolling\n"),
            Some("Arch rolling".to_owned()),
            "fallback"
        );
    }

    #[test]
    fn df_parses_mounts_with_spaces_and_skips_system_mounts() {
        let text = "Filesystem     1024-blocks      Used Available Capacity Mounted on\n\
                    /dev/nvme0n1p2   100000000  40000000  60000000      40% /\n\
                    /dev/nvme0n1p2   100000000  40000000  60000000      40% /home\n\
                    /dev/nvme0n1p1     1000000    100000    900000      10% /boot/efi\n\
                    /dev/sdb1          2000000   1000000   1000000      50% /media/me/My Disk\n";
        let got = parse_df(text);
        assert_eq!(got.len(), 2, "boot skipped, subvolume deduped: {got:?}");
        assert_eq!(
            got.get(1)
                .map(|v| (v.mount.as_str(), v.name.as_str(), v.free)),
            Some(("/media/me/My Disk", "sdb1", 1_024_000_000)),
            "spaces kept, KiB converted"
        );
    }
}
