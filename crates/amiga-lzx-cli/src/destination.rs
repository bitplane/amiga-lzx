//! The directory an archive is extracted into.
//!
//! Where cap-std is available, every lookup is confined to the opened
//! directory, so a component swapped for a symlink after the check still
//! cannot redirect a write. AROS has no `openat()` family for cap-std to
//! build on; there paths are joined onto the destination and the symlink
//! check in `extract` is the only guard. AROS's C library also lacks
//! `futimens`, so times are set by path once the file is closed.

use std::fs::File;
use std::io;
use std::path::Path;
use std::time::SystemTime;

#[cfg(not(target_os = "aros"))]
pub struct Destination(cap_std::fs::Dir);

#[cfg(not(target_os = "aros"))]
impl Destination {
    pub fn open(path: &Path) -> io::Result<Self> {
        cap_std::fs::Dir::open_ambient_dir(path, cap_std::ambient_authority()).map(Self)
    }

    pub fn is_symlink(&self, name: &Path) -> io::Result<bool> {
        Ok(self.0.symlink_metadata(name)?.file_type().is_symlink())
    }

    pub fn create_dir_all(&self, name: &Path) -> io::Result<()> {
        self.0.create_dir_all(name)
    }

    pub fn create_file(&self, name: &Path) -> io::Result<File> {
        let mut options = cap_std::fs::OpenOptions::new();
        options.write(true).create(true).truncate(true);
        Ok(self.0.open_with(name, &options)?.into_std())
    }

    pub fn set_modified(&self, file: File, _name: &Path, time: SystemTime) -> io::Result<()> {
        file.set_modified(time)
    }
}

#[cfg(target_os = "aros")]
pub struct Destination(std::path::PathBuf);

#[cfg(target_os = "aros")]
impl Destination {
    pub fn open(path: &Path) -> io::Result<Self> {
        if std::fs::metadata(path)?.is_dir() {
            Ok(Self(path.to_path_buf()))
        } else {
            Err(io::Error::new(
                io::ErrorKind::NotADirectory,
                "extraction target is not a directory",
            ))
        }
    }

    pub fn is_symlink(&self, name: &Path) -> io::Result<bool> {
        Ok(std::fs::symlink_metadata(self.0.join(name))?
            .file_type()
            .is_symlink())
    }

    pub fn create_dir_all(&self, name: &Path) -> io::Result<()> {
        std::fs::create_dir_all(self.0.join(name))
    }

    pub fn create_file(&self, name: &Path) -> io::Result<File> {
        File::create(self.0.join(name))
    }

    pub fn set_modified(&self, file: File, name: &Path, time: SystemTime) -> io::Result<()> {
        use std::ffi::CString;
        use std::os::unix::ffi::OsStrExt;

        drop(file);
        let path = CString::new(self.0.join(name).as_os_str().as_bytes())?;
        let since_epoch = time
            .duration_since(SystemTime::UNIX_EPOCH)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "time before 1970"))?;
        let stamp = libc::timeval {
            tv_sec: since_epoch.as_secs() as _,
            tv_usec: since_epoch.subsec_micros() as _,
        };
        // Access and modification times.
        let times = [stamp, stamp];
        if unsafe { libc::utimes(path.as_ptr(), times.as_ptr()) } == 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    }
}
