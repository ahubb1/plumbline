//! Atomic replacement of the destination file by a finished temporary file.
//!
//! On Windows the first choice is `ReplaceFileW`, which swaps the file while keeping the
//! destination's attributes, ACLs and creation time. Some shares and file systems refuse it,
//! so the fallback is `MoveFileExW` with `MOVEFILE_REPLACE_EXISTING`. If both fail, the
//! destination still holds its old content and the caller removes the temporary file.

use std::path::{Path, PathBuf};

use crate::error::{PdfError, Result};

/// Which call put the new file in place.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReplaceMethod {
    /// `ReplaceFileW`.
    ReplaceFile,
    /// `MoveFileExW` (the destination did not exist, or `ReplaceFileW` was refused).
    MoveFile,
    /// `std::fs::rename` on platforms other than Windows.
    Rename,
}

/// Name of the short-lived backup `ReplaceFileW` moves the old file to: `name.pdf.plbak`.
fn backup_path_for(dest: &Path) -> PathBuf {
    let mut name = dest.as_os_str().to_os_string();
    name.push(".plbak");
    PathBuf::from(name)
}

/// Replace `dest` with `tmp`. Afterwards `tmp` no longer exists and `dest` has its content.
/// On error `dest` is as it was before the call, and `tmp` still exists.
pub fn replace_file(tmp: &Path, dest: &Path) -> Result<ReplaceMethod> {
    imp::replace_file(tmp, dest)
}

#[cfg(windows)]
mod imp {
    use super::*;
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Foundation::{GetLastError, ERROR_UNABLE_TO_MOVE_REPLACEMENT_2};
    use windows_sys::Win32::Storage::FileSystem::{
        MoveFileExW, ReplaceFileW, MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH,
        REPLACEFILE_IGNORE_MERGE_ERRORS,
    };

    fn wide(path: &Path) -> Vec<u16> {
        path.as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect()
    }

    fn move_replace(from: &[u16], to: &[u16]) -> std::result::Result<(), u32> {
        // SAFETY: both pointers are NUL-terminated UTF-16 buffers that outlive the call.
        let ok = unsafe {
            MoveFileExW(
                from.as_ptr(),
                to.as_ptr(),
                MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
            )
        };
        if ok != 0 {
            Ok(())
        } else {
            // SAFETY: plain thread-local read.
            Err(unsafe { GetLastError() })
        }
    }

    pub(super) fn replace_file(tmp: &Path, dest: &Path) -> Result<ReplaceMethod> {
        let tmp_w = wide(tmp);
        let dest_w = wide(dest);
        let fail = |replace_code: u32, move_code: u32| PdfError::Replace {
            from: tmp.to_path_buf(),
            to: dest.to_path_buf(),
            replace_code,
            move_code,
        };

        // ReplaceFileW needs an existing destination; a new file is a plain move.
        if !dest.exists() {
            return move_replace(&tmp_w, &dest_w)
                .map(|()| ReplaceMethod::MoveFile)
                .map_err(|code| fail(0, code));
        }

        // A named backup means that in the one failure mode where ReplaceFileW has already
        // renamed the old file, we know where it went and can put it back.
        let backup = backup_path_for(dest);
        let backup_w = wide(&backup);
        let _ = std::fs::remove_file(&backup);

        // SAFETY: the three path pointers are NUL-terminated UTF-16 buffers that outlive the
        // call; the two reserved pointers must be null.
        let ok = unsafe {
            ReplaceFileW(
                dest_w.as_ptr(),
                tmp_w.as_ptr(),
                backup_w.as_ptr(),
                REPLACEFILE_IGNORE_MERGE_ERRORS,
                std::ptr::null(),
                std::ptr::null(),
            )
        };
        if ok != 0 {
            let _ = std::fs::remove_file(&backup);
            return Ok(ReplaceMethod::ReplaceFile);
        }
        // SAFETY: plain thread-local read.
        let replace_code = unsafe { GetLastError() };

        if replace_code == ERROR_UNABLE_TO_MOVE_REPLACEMENT_2 {
            // The old file now sits under the backup name and the destination name is free.
            // Finish the job with a move; if that fails too, restore the old file.
            return match move_replace(&tmp_w, &dest_w) {
                Ok(()) => {
                    let _ = std::fs::remove_file(&backup);
                    Ok(ReplaceMethod::MoveFile)
                }
                Err(move_code) => {
                    let _ = move_replace(&backup_w, &dest_w);
                    Err(fail(replace_code, move_code))
                }
            };
        }

        // In every other failure mode both files still have their own names and content.
        match move_replace(&tmp_w, &dest_w) {
            Ok(()) => Ok(ReplaceMethod::MoveFile),
            Err(move_code) => Err(fail(replace_code, move_code)),
        }
    }
}

#[cfg(not(windows))]
mod imp {
    use super::*;

    pub(super) fn replace_file(tmp: &Path, dest: &Path) -> Result<ReplaceMethod> {
        let _ = backup_path_for;
        std::fs::rename(tmp, dest)
            .map(|()| ReplaceMethod::Rename)
            .map_err(|e| PdfError::Replace {
                from: tmp.to_path_buf(),
                to: dest.to_path_buf(),
                replace_code: 0,
                move_code: e.raw_os_error().unwrap_or(0) as u32,
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replaces_an_existing_file_and_removes_the_tmp() {
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("a.pdf");
        let tmp = dir.path().join("a.pdf.tmp");
        std::fs::write(&dest, b"old").unwrap();
        std::fs::write(&tmp, b"new content").unwrap();
        let method = replace_file(&tmp, &dest).unwrap();
        if cfg!(windows) {
            assert_eq!(method, ReplaceMethod::ReplaceFile);
        }
        assert_eq!(std::fs::read(&dest).unwrap(), b"new content");
        assert!(!tmp.exists());
        assert!(
            !backup_path_for(&dest).exists(),
            "backup must not be left behind"
        );
    }

    #[test]
    fn moves_into_place_when_the_destination_is_new() {
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("new.pdf");
        let tmp = dir.path().join("new.pdf.tmp");
        std::fs::write(&tmp, b"fresh").unwrap();
        let method = replace_file(&tmp, &dest).unwrap();
        if cfg!(windows) {
            assert_eq!(method, ReplaceMethod::MoveFile);
        }
        assert_eq!(std::fs::read(&dest).unwrap(), b"fresh");
        assert!(!tmp.exists());
    }

    #[cfg(windows)]
    #[test]
    fn a_locked_destination_fails_and_keeps_both_files() {
        use std::os::windows::fs::OpenOptionsExt;
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("locked.pdf");
        let tmp = dir.path().join("locked.pdf.tmp");
        std::fs::write(&dest, b"old").unwrap();
        std::fs::write(&tmp, b"new").unwrap();
        // share_mode(0): nobody else may read, write or delete while this handle is open.
        let lock = std::fs::OpenOptions::new()
            .read(true)
            .share_mode(0)
            .open(&dest)
            .unwrap();
        let err = replace_file(&tmp, &dest).unwrap_err();
        match err {
            PdfError::Replace {
                replace_code,
                move_code,
                ..
            } => {
                assert_ne!(replace_code, 0);
                assert_ne!(move_code, 0);
            }
            other => panic!("unexpected error {other:?}"),
        }
        drop(lock);
        assert_eq!(std::fs::read(&dest).unwrap(), b"old");
        assert_eq!(std::fs::read(&tmp).unwrap(), b"new");
        assert!(!backup_path_for(&dest).exists());
    }

    #[test]
    fn a_missing_tmp_fails_and_keeps_the_destination() {
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("keep.pdf");
        std::fs::write(&dest, b"old").unwrap();
        assert!(replace_file(&dir.path().join("keep.pdf.tmp"), &dest).is_err());
        assert_eq!(std::fs::read(&dest).unwrap(), b"old");
    }
}
