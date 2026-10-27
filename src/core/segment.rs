use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SegmentStatus {
    Pending,
    Downloading,
    Completed,
    Paused,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Segment {
    pub segment_id: usize,
    pub start: u64,
    /// Inclusive end offset. `Segment::UNBOUNDED_END` when the file size is unknown.
    pub end: u64,
    pub current_offset: u64,
    pub status: SegmentStatus,
    pub connection_id: Option<usize>,
    pub speed_bps: f64,
    /// Consecutive failed attempts for this segment.
    #[serde(default)]
    pub retries: u32,
}

impl Segment {
    /// End marker for a stream whose length the server did not report.
    pub const UNBOUNDED_END: u64 = u64::MAX;

    pub fn new(segment_id: usize, start: u64, end: u64) -> Self {
        Self {
            segment_id,
            start,
            end,
            current_offset: start,
            status: SegmentStatus::Pending,
            connection_id: None,
            speed_bps: 0.0,
            retries: 0,
        }
    }

    pub fn new_unbounded(segment_id: usize) -> Self {
        Self::new(segment_id, 0, Self::UNBOUNDED_END)
    }

    pub fn is_unbounded(&self) -> bool {
        self.end == Self::UNBOUNDED_END
    }

    pub fn total_bytes(&self) -> u64 {
        if self.end >= self.start {
            (self.end - self.start).saturating_add(1)
        } else {
            0
        }
    }

    /// Bytes written for this segment, never more than the segment length
    /// (a split can move `end` below an in-flight `current_offset`).
    pub fn downloaded_bytes(&self) -> u64 {
        self.current_offset
            .min(self.end.saturating_add(1))
            .saturating_sub(self.start)
    }

    pub fn remaining_bytes(&self) -> u64 {
        if self.current_offset > self.end {
            0
        } else {
            (self.end - self.current_offset).saturating_add(1)
        }
    }

    pub fn progress_fraction(&self) -> f64 {
        if self.status == SegmentStatus::Completed {
            return 1.0;
        }
        if self.is_unbounded() {
            return 0.0;
        }
        let total = self.total_bytes();
        if total == 0 {
            0.0
        } else {
            (self.downloaded_bytes() as f64 / total as f64).min(1.0)
        }
    }

    pub fn is_finished(&self) -> bool {
        self.current_offset > self.end
    }

    pub fn advance(&mut self, num_bytes: u64) {
        self.current_offset = self.current_offset.saturating_add(num_bytes);
        if self.current_offset > self.end {
            self.status = SegmentStatus::Completed;
        }
    }

    /// The signature algorithm: dynamic work stealing.
    /// Splits the uncompleted remaining bytes in half and produces a new Segment.
    pub fn split_remaining(&mut self, new_segment_id: usize, min_split_bytes: u64) -> Option<Segment> {
        if self.status != SegmentStatus::Downloading || self.is_unbounded() {
            return None;
        }
        let rem = self.remaining_bytes();
        if rem < min_split_bytes.saturating_mul(2).max(2) {
            return None;
        }

        let half = rem / 2;
        let new_start = self.current_offset + half;
        let new_end = self.end;

        self.end = new_start - 1;

        Some(Segment::new(new_segment_id, new_start, new_end))
    }
}
