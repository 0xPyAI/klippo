use std::fs::{self, File};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use serde::{Deserialize, Serialize};
use super::segment::{Segment, SegmentStatus};

pub const STATE_EXTENSION: &str = ".klippo.state";

#[derive(Debug, Serialize, Deserialize)]
pub struct DownloadState {
    pub version: u32,
    pub url: String,
    pub output_path: String,
    pub total_size: Option<u64>,
    pub etag: Option<String>,
    pub last_modified: Option<String>,
    pub segments: Vec<Segment>,
}

impl DownloadState {
    /// True when the remote file still looks like the one the saved
    /// segments were downloaded from.
    pub fn matches_remote(
        &self,
        total_size: Option<u64>,
        etag: Option<&str>,
        last_modified: Option<&str>,
        ignore_time_changes: bool,
    ) -> bool {
        if self.total_size != total_size {
            return false;
        }
        if let (Some(saved), Some(now)) = (self.etag.as_deref(), etag) {
            if saved != now {
                return false;
            }
        }
        if !ignore_time_changes {
            if let (Some(saved), Some(now)) = (self.last_modified.as_deref(), last_modified) {
                if saved != now {
                    return false;
                }
            }
        }
        true
    }
}

#[derive(Clone)]
pub struct ResumeManager {
    file_path: PathBuf,
    state_file_path: PathBuf,
}

impl ResumeManager {
    pub fn new<P: AsRef<Path>>(file_path: P) -> Self {
        let p = file_path.as_ref().to_path_buf();
        Self {
            state_file_path: Self::state_path_for(&p),
            file_path: p,
        }
    }

    /// Path of the sidecar state file that belongs to a download target.
    pub fn state_path_for(file_path: &Path) -> PathBuf {
        let mut state_name = file_path.as_os_str().to_os_string();
        state_name.push(STATE_EXTENSION);
        PathBuf::from(state_name)
    }

    pub fn exists(&self) -> bool {
        self.state_file_path.exists()
    }

    pub fn save_state(
        &self,
        url: &str,
        total_size: Option<u64>,
        etag: Option<&str>,
        last_modified: Option<&str>,
        segments: &[Segment],
    ) -> io::Result<()> {
        let state = DownloadState {
            version: 1,
            url: url.to_string(),
            output_path: self.file_path.to_string_lossy().to_string(),
            total_size,
            etag: etag.map(String::from),
            last_modified: last_modified.map(String::from),
            segments: segments.to_vec(),
        };

        let json_data = serde_json::to_string_pretty(&state).map_err(io::Error::other)?;

        if let Some(parent) = self.state_file_path.parent() {
            if !parent.as_os_str().is_empty() {
                fs::create_dir_all(parent)?;
            }
        }

        // Atomic write via temp file
        let tmp_path = self.state_file_path.with_extension("tmp");
        {
            let mut f = File::create(&tmp_path)?;
            f.write_all(json_data.as_bytes())?;
            f.flush()?;
        }

        fs::rename(&tmp_path, &self.state_file_path)?;
        Ok(())
    }

    pub fn load_state(&self) -> Option<DownloadState> {
        if !self.exists() {
            return None;
        }
        let data = fs::read_to_string(&self.state_file_path).ok()?;
        serde_json::from_str(&data).ok()
    }

    pub fn restore_segments(&self) -> Vec<Segment> {
        match self.load_state() {
            Some(state) => Self::reset_segments(state.segments),
            None => Vec::new(),
        }
    }

    /// Prepares saved segments for a new run: unfinished work goes back to Pending.
    pub fn reset_segments(segments: Vec<Segment>) -> Vec<Segment> {
        segments
            .into_iter()
            .map(|mut s| {
                if s.is_finished() {
                    s.status = SegmentStatus::Completed;
                } else if s.status != SegmentStatus::Completed {
                    s.status = SegmentStatus::Pending;
                }
                s.speed_bps = 0.0;
                s.retries = 0;
                s.connection_id = None;
                s
            })
            .collect()
    }

    pub fn cleanup(&self) {
        if self.exists() {
            let _ = fs::remove_file(&self.state_file_path);
        }
    }

    pub fn state_file_path(&self) -> &Path {
        &self.state_file_path
    }
}

