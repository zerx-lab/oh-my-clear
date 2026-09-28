//! Maps OS errors to the [`FailReason`] the UI explains (and offers a fix for).
//!
//! macOS distinguishes TCC privacy denials from Unix permissions: TCC returns `EPERM`
//! ("Operation not permitted") where plain permissions return `EACCES` ("Permission
//! denied"). `EPERM` inside another app's bundle is the App Management permission;
//! elsewhere under the home folder or `/Library` it is Full Disk Access.

use std::io;
use std::path::Path;

use omc_proto::jobs::FailReason;

/// Classifies an error hit while reading or removing `path`.
pub fn classify(err: &io::Error, path: &Path) -> FailReason {
    let code = err.raw_os_error();
    #[cfg(target_os = "macos")]
    {
        const EPERM: i32 = 1;
        const EACCES: i32 = 13;
        const EBUSY: i32 = 16;
        const ETXTBSY: i32 = 26;
        match code {
            Some(EPERM) => {
                if path
                    .components()
                    .any(|c| c.as_os_str().as_encoded_bytes().ends_with(b".app"))
                {
                    FailReason::AppManagement
                } else {
                    FailReason::FullDiskAccess
                }
            }
            Some(EACCES) => FailReason::PermissionDenied,
            Some(EBUSY | ETXTBSY) => FailReason::InUse,
            _ => generic(err),
        }
    }
    #[cfg(windows)]
    {
        const ERROR_ACCESS_DENIED: i32 = 5;
        const ERROR_SHARING_VIOLATION: i32 = 32;
        const ERROR_LOCK_VIOLATION: i32 = 33;
        const ERROR_PRIVILEGE_NOT_HELD: i32 = 1314;
        let _ = path;
        match code {
            Some(ERROR_ACCESS_DENIED | ERROR_PRIVILEGE_NOT_HELD) => FailReason::PermissionDenied,
            Some(ERROR_SHARING_VIOLATION | ERROR_LOCK_VIOLATION) => FailReason::InUse,
            _ => generic(err),
        }
    }
    #[cfg(not(any(target_os = "macos", windows)))]
    {
        const EBUSY: i32 = 16;
        const ETXTBSY: i32 = 26;
        let _ = path;
        match code {
            Some(EBUSY | ETXTBSY) => FailReason::InUse,
            _ => generic(err),
        }
    }
}

fn generic(err: &io::Error) -> FailReason {
    match err.kind() {
        io::ErrorKind::PermissionDenied => FailReason::PermissionDenied,
        io::ErrorKind::ResourceBusy | io::ErrorKind::ExecutableFileBusy => FailReason::InUse,
        _ => FailReason::Other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn permission_errors_are_classified() {
        let denied = io::Error::from(io::ErrorKind::PermissionDenied);
        assert_eq!(
            classify(&denied, Path::new("/x")),
            FailReason::PermissionDenied,
            "kind-based fallback"
        );
        let other = io::Error::other("boom");
        assert_eq!(
            classify(&other, Path::new("/x")),
            FailReason::Other,
            "other"
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_eperm_is_a_privacy_denial() {
        let eperm = io::Error::from_raw_os_error(1);
        assert_eq!(
            classify(&eperm, Path::new("/Users/me/Library/Safari/History.db")),
            FailReason::FullDiskAccess,
            "TCC-protected data"
        );
        assert_eq!(
            classify(&eperm, Path::new("/Applications/Foo.app/Contents")),
            FailReason::AppManagement,
            "inside another app"
        );
        let eacces = io::Error::from_raw_os_error(13);
        assert_eq!(
            classify(&eacces, Path::new("/Library/x")),
            FailReason::PermissionDenied,
            "plain permissions need admin"
        );
    }
}
