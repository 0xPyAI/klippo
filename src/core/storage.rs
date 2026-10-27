use std::fs::{File, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// Direct file storage allocator for zero-concatenation parallel offset writes.
#[derive(Clone)]
pub struct DirectStorage {
    file_path: PathBuf,
    file: Arc<Mutex<Option<File>>>,
    total_size: Option<u64>,
}

impl DirectStorage {
    pub fn new<P: AsRef<Path>>(file_path: P, total_size: Option<u64>) -> Self {
        Self {
            file_path: file_path.as_ref().to_path_buf(),
            file: Arc::new(Mutex::new(None)),
            total_size,
        }
    }

    /// Opens the file without discarding existing data (resume-friendly).
    pub fn open(&self) -> io::Result<()> {
        self.open_with(false)
    }

    /// Opens the file; `truncate` discards stale data from an earlier attempt.
    pub fn open_with(&self, truncate: bool) -> io::Result<()> {
        let mut lock = self.file.lock().unwrap();
        if lock.is_some() {
            return Ok(());
        }

        if let Some(parent) = self.file_path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)?;
            }
        }

        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(truncate)
            .open(&self.file_path)?;

        // Pre-allocate space if total size is known, so a full disk fails early
        if let Some(size) = self.total_size {
            if size > 0 {
                file.set_len(size)?;
            }
        }

        *lock = Some(file);
        Ok(())
    }

    pub fn write_at(&self, offset: u64, data: &[u8]) -> io::Result<usize> {
        if data.is_empty() {
            return Ok(0);
        }

        let lock = self.file.lock().unwrap();
        match *lock {
            Some(ref file) => {
                write_all_at(file, offset, data)?;
                Ok(data.len())
            }
            None => Err(io::Error::new(io::ErrorKind::NotConnected, "storage file is not open")),
        }
    }

    pub fn flush(&self) -> io::Result<()> {
        let lock = self.file.lock().unwrap();
        if let Some(ref file) = *lock {
            file.sync_data()?;
        }
        Ok(())
    }

    pub fn close(&self) {
        let mut lock = self.file.lock().unwrap();
        *lock = None;
    }

    pub fn truncate_to(&self, size: u64) -> io::Result<()> {
        let lock = self.file.lock().unwrap();
        if let Some(ref file) = *lock {
            file.set_len(size)?;
        }
        Ok(())
    }

    pub fn file_path(&self) -> &Path {
        &self.file_path
    }
}

#[cfg(windows)]
fn write_all_at(file: &File, mut offset: u64, mut data: &[u8]) -> io::Result<()> {
    use std::os::windows::fs::FileExt;
    while !data.is_empty() {
        let n = file.seek_write(data, offset)?;
        if n == 0 {
            return Err(io::Error::new(io::ErrorKind::WriteZero, "failed to write to file"));
        }
        data = &data[n..];
        offset += n as u64;
    }
    Ok(())
}

#[cfg(unix)]
fn write_all_at(file: &File, offset: u64, data: &[u8]) -> io::Result<()> {
    use std::os::unix::fs::FileExt;
    file.write_all_at(data, offset)
}
