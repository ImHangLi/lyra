//! Canonical run logs (§14.4): `000001.jsonl` segments, ≤8 KiB records split on UTF-8
//! boundaries, buffered writes flushed every 250 ms or 64 KiB, and a per-run byte cap that
//! rotates out the oldest segments even while the run is active.

use std::collections::VecDeque;
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use lyra_protocol::ids::LogSeq;
use lyra_protocol::limits::MAX_LOG_TEXT_BYTES;
use lyra_protocol::run::{LogRecord, LogStream};
use lyra_protocol::time::Timestamp;
use lyra_protocol::view::LogLevel;

const SEGMENT_MAX_BYTES: u64 = 4 * 1024 * 1024;
const FLUSH_BYTES: usize = 64 * 1024;
pub const FLUSH_EVERY: Duration = Duration::from_millis(250);
/// Recent records kept in memory for cheap tail reads and streaming.
const RING_RECORDS: usize = 2000;

pub type SharedLog = Arc<Mutex<RunLog>>;

struct Segment {
    number: u32,
    first_seq: u64,
    bytes: u64,
}

pub struct RunLog {
    dir: PathBuf,
    next_seq: u64,
    segments: VecDeque<Segment>,
    writer: Option<BufWriter<File>>,
    unflushed: usize,
    last_flush: Instant,
    cap_bytes: u64,
    total_bytes: u64,
    ring: VecDeque<LogRecord>,
    /// Records that could not be written (IO failure); never silently zero.
    pub dropped: u64,
    /// Records split because they exceeded 8 KiB.
    pub truncated: u64,
    pub write_error: Option<String>,
    partial: [Vec<u8>; 2],
}

fn segment_name(n: u32) -> String {
    format!("{n:06}.jsonl")
}

/// Splits text into ≤8 KiB chunks on character boundaries.
fn chunks(text: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut rest = text;
    while rest.len() > MAX_LOG_TEXT_BYTES {
        let mut end = MAX_LOG_TEXT_BYTES;
        while !rest.is_char_boundary(end) {
            end -= 1;
        }
        out.push(&rest[..end]);
        rest = &rest[end..];
    }
    out.push(rest);
    out
}

impl RunLog {
    pub fn create(dir: PathBuf, cap_bytes: u64) -> Self {
        let mut log = Self {
            dir,
            next_seq: 1,
            segments: VecDeque::new(),
            writer: None,
            unflushed: 0,
            last_flush: Instant::now(),
            cap_bytes,
            total_bytes: 0,
            ring: VecDeque::new(),
            dropped: 0,
            truncated: 0,
            write_error: None,
            partial: [Vec::new(), Vec::new()],
        };
        if let Err(e) = std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&log.dir)
        {
            log.write_error = Some(format!("cannot create log dir: {e}"));
        }
        log
    }

    /// Opens an existing run log for reading (finished runs from earlier host epochs).
    pub fn open_existing(dir: PathBuf) -> Self {
        let mut log = Self::create_readonly(dir);
        let mut numbers: Vec<u32> = std::fs::read_dir(&log.dir)
            .map(|rd| {
                rd.flatten()
                    .filter_map(|e| e.file_name().to_str()?.strip_suffix(".jsonl")?.parse().ok())
                    .collect()
            })
            .unwrap_or_default();
        numbers.sort_unstable();
        for n in numbers {
            let path = log.dir.join(segment_name(n));
            let bytes = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
            let first = read_segment(&path)
                .first()
                .map_or(log.next_seq, |r| r.log_seq.get());
            if let Some(last) = read_segment(&path).last() {
                log.next_seq = last.log_seq.get() + 1;
            }
            log.total_bytes += bytes;
            log.segments.push_back(Segment {
                number: n,
                first_seq: first,
                bytes,
            });
        }
        log
    }

    fn create_readonly(dir: PathBuf) -> Self {
        Self {
            dir,
            next_seq: 1,
            segments: VecDeque::new(),
            writer: None,
            unflushed: 0,
            last_flush: Instant::now(),
            cap_bytes: u64::MAX,
            total_bytes: 0,
            ring: VecDeque::new(),
            dropped: 0,
            truncated: 0,
            write_error: None,
            partial: [Vec::new(), Vec::new()],
        }
    }

    pub fn first_available(&self) -> Option<LogSeq> {
        self.segments
            .front()
            .map(|s| s.first_seq)
            .or_else(|| self.ring.front().map(|r| r.log_seq.get()))
            .and_then(|v| LogSeq::new(v).ok())
    }

    pub fn last_available(&self) -> Option<LogSeq> {
        (self.next_seq > 1)
            .then(|| LogSeq::new(self.next_seq - 1).ok())
            .flatten()
    }

    fn open_segment(&mut self) -> Result<(), String> {
        let number = self.segments.back().map_or(1, |s| s.number + 1);
        let path = self.dir.join(segment_name(number));
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .mode(0o600)
            .open(&path)
            .map_err(|e| e.to_string())?;
        self.writer = Some(BufWriter::with_capacity(FLUSH_BYTES, file));
        self.segments.push_back(Segment {
            number,
            first_seq: self.next_seq,
            bytes: 0,
        });
        Ok(())
    }

    fn rotate_if_needed(&mut self) {
        let full = self
            .segments
            .back()
            .is_none_or(|s| s.bytes >= SEGMENT_MAX_BYTES);
        if full {
            if let Some(w) = self.writer.as_mut() {
                let _ = w.flush();
            }
            self.writer = None;
            if let Err(e) = self.open_segment() {
                self.write_error = Some(e);
            }
        }
        // Enforce the per-run cap by removing the oldest closed segments.
        while self.total_bytes > self.cap_bytes && self.segments.len() > 1 {
            if let Some(old) = self.segments.pop_front() {
                let _ = std::fs::remove_file(self.dir.join(segment_name(old.number)));
                self.total_bytes = self.total_bytes.saturating_sub(old.bytes);
            }
        }
    }

    fn append(&mut self, record: LogRecord) {
        match serde_json::to_vec(&record) {
            Ok(mut line) if self.write_error.is_none() => {
                line.push(b'\n');
                self.rotate_if_needed();
                let len = line.len() as u64;
                match self.writer.as_mut().map(|w| w.write_all(&line)) {
                    Some(Ok(())) => {
                        if let Some(s) = self.segments.back_mut() {
                            s.bytes += len;
                        }
                        self.total_bytes += len;
                        self.unflushed += line.len();
                    }
                    Some(Err(e)) => {
                        self.write_error = Some(e.to_string());
                        self.dropped += 1;
                    }
                    None => self.dropped += 1,
                }
            }
            _ => self.dropped += 1,
        }
        self.next_seq += 1;
        self.ring.push_back(record);
        if self.ring.len() > RING_RECORDS {
            self.ring.pop_front();
        }
        if self.unflushed >= FLUSH_BYTES {
            self.flush();
        }
    }

    /// Records one line of text (without its newline) and returns the created records.
    pub fn push_line(
        &mut self,
        stream: LogStream,
        level: LogLevel,
        text: &str,
        continued_from_previous: bool,
    ) -> Vec<LogRecord> {
        self.push_line_fields(stream, level, text, continued_from_previous, None)
    }

    /// Like [`Self::push_line`], with optional structured fields on the first record.
    pub fn push_line_fields(
        &mut self,
        stream: LogStream,
        level: LogLevel,
        text: &str,
        continued_from_previous: bool,
        mut fields: Option<serde_json::Map<String, serde_json::Value>>,
    ) -> Vec<LogRecord> {
        let parts = chunks(text);
        let n = parts.len();
        if n > 1 {
            self.truncated += 1;
        }
        let mut out = Vec::with_capacity(n);
        for (i, part) in parts.into_iter().enumerate() {
            let Ok(seq) = LogSeq::new(self.next_seq) else {
                break;
            };
            let record = LogRecord {
                log_seq: seq,
                recorded_at: Timestamp::now(),
                stream,
                level,
                text: part.to_owned(),
                continued: continued_from_previous || i > 0,
                truncated: n > 1 && i + 1 < n,
                fields: fields.take().filter(|f| !f.is_empty()),
            };
            out.push(record.clone());
            self.append(record);
        }
        out
    }

    /// Feeds raw pipe bytes. Complete lines become records; an unterminated tail longer than
    /// 8 KiB is emitted early as a continued record so memory stays bounded.
    pub fn push_bytes(&mut self, stream: LogStream, bytes: &[u8]) -> Vec<LogRecord> {
        let idx = usize::from(stream == LogStream::Stderr);
        let level = if stream == LogStream::Stderr {
            LogLevel::Warn
        } else {
            LogLevel::Info
        };
        let mut out = Vec::new();
        let mut buf = std::mem::take(&mut self.partial[idx]);
        buf.extend_from_slice(bytes);
        let mut start = 0;
        while let Some(pos) = buf[start..].iter().position(|b| *b == b'\n') {
            let mut line = &buf[start..start + pos];
            if line.last() == Some(&b'\r') {
                line = &line[..line.len() - 1];
            }
            out.extend(self.push_line(stream, level, &String::from_utf8_lossy(line), false));
            start += pos + 1;
        }
        let mut rest = buf[start..].to_vec();
        if rest.len() > MAX_LOG_TEXT_BYTES {
            let mut cut = MAX_LOG_TEXT_BYTES;
            while cut > 0 && std::str::from_utf8(&rest[..cut]).is_err() {
                cut -= 1;
            }
            let head: Vec<u8> = rest.drain(..cut.max(1)).collect();
            let mut records = self.push_line(stream, level, &String::from_utf8_lossy(&head), false);
            if let Some(r) = records.last_mut() {
                r.truncated = true;
            }
            out.extend(records);
        }
        self.partial[idx] = rest;
        out
    }

    /// Emits any unterminated tails (at EOF).
    pub fn finish_streams(&mut self) -> Vec<LogRecord> {
        let mut out = Vec::new();
        for (idx, stream) in [(0, LogStream::Stdout), (1, LogStream::Stderr)] {
            let rest = std::mem::take(&mut self.partial[idx]);
            if !rest.is_empty() {
                let level = if idx == 1 {
                    LogLevel::Warn
                } else {
                    LogLevel::Info
                };
                out.extend(self.push_line(stream, level, &String::from_utf8_lossy(&rest), false));
            }
        }
        out
    }

    pub fn flush_due(&self) -> bool {
        self.unflushed > 0 && self.last_flush.elapsed() >= FLUSH_EVERY
    }

    pub fn flush(&mut self) {
        if let Some(w) = self.writer.as_mut()
            && let Err(e) = w.flush()
        {
            self.write_error = Some(e.to_string());
        }
        self.unflushed = 0;
        self.last_flush = Instant::now();
    }

    /// Up to `limit` records with `log_seq < before` (or the tail), oldest first.
    pub fn read_before(&mut self, before: Option<u64>, limit: usize) -> Vec<LogRecord> {
        self.flush();
        let upper = before.unwrap_or(u64::MAX);
        let from_ring: Vec<LogRecord> = self
            .ring
            .iter()
            .filter(|r| r.log_seq.get() < upper)
            .cloned()
            .collect();
        // The ring suffices when it holds enough records or starts at the oldest kept record.
        let ring_complete = self.ring.front().map(|r| r.log_seq.get())
            == self.segments.front().map(|s| s.first_seq);
        let mut records = if from_ring.len() >= limit || ring_complete || self.segments.is_empty() {
            from_ring
        } else {
            let mut all = Vec::new();
            for s in &self.segments {
                if s.first_seq >= upper {
                    break;
                }
                all.extend(
                    read_segment(&self.dir.join(segment_name(s.number)))
                        .into_iter()
                        .filter(|r| r.log_seq.get() < upper),
                );
            }
            all
        };
        if records.len() > limit {
            records.drain(..records.len() - limit);
        }
        records
    }
}

impl RunLog {
    /// Up to `limit` records with `from <= log_seq <= upto`, oldest first.
    pub fn read_range(&mut self, from: u64, upto: u64, limit: usize) -> Vec<LogRecord> {
        self.flush();
        let in_range = |r: &LogRecord| (from..=upto).contains(&r.log_seq.get());
        let ring_covers = self.ring.front().is_some_and(|r| r.log_seq.get() <= from);
        if ring_covers || self.segments.is_empty() {
            return self
                .ring
                .iter()
                .filter(|r| in_range(r))
                .take(limit)
                .cloned()
                .collect();
        }
        let mut out = Vec::new();
        for (i, s) in self.segments.iter().enumerate() {
            if s.first_seq > upto {
                break;
            }
            // Skip segments that end before `from`.
            if self
                .segments
                .get(i + 1)
                .is_some_and(|next| next.first_seq <= from)
            {
                continue;
            }
            for r in read_segment(&self.dir.join(segment_name(s.number))) {
                if in_range(&r) {
                    out.push(r);
                    if out.len() >= limit {
                        return out;
                    }
                }
            }
        }
        out
    }
}

/// Parses complete LF-terminated records; a torn final line is ignored.
fn read_segment(path: &Path) -> Vec<LogRecord> {
    let Ok(f) = File::open(path) else {
        return vec![];
    };
    let mut out = Vec::new();
    let mut reader = BufReader::new(f);
    let mut line = Vec::new();
    while let Ok(n) = reader.read_until(b'\n', &mut line) {
        if n == 0 || line.last() != Some(&b'\n') {
            break;
        }
        if let Ok(r) = serde_json::from_slice::<LogRecord>(&line[..line.len() - 1]) {
            out.push(r);
        }
        line.clear();
    }
    out
}
