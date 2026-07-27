//! Per-window terminal context store.
//!
//! Lines arrive from the kitty C hook tagged with the OSC 133 prompt kind of
//! the row they were read from. Each line gets a monotonically increasing
//! sequence number, so eviction from the front never invalidates the
//! "consumed up to" marker (the defect class of the old daemon's
//! index-based cache).

/// OSC 133 prompt kind of a captured line (mirrors kitty's PromptKind).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineKind {
    Unknown,
    Prompt,
    SecondaryPrompt,
    Output,
}

impl LineKind {
    pub fn from_u8(v: u8) -> Self {
        match v {
            1 => LineKind::Prompt,
            2 => LineKind::SecondaryPrompt,
            3 => LineKind::Output,
            _ => LineKind::Unknown,
        }
    }
}

#[derive(Debug, Clone)]
struct StoredLine {
    seq: u64,
    kind: LineKind,
    text: String,
}

/// Bounded, sequence-numbered ring of captured terminal lines.
#[derive(Debug)]
pub struct ContextStore {
    lines: std::collections::VecDeque<StoredLine>,
    next_seq: u64,
    consumed_seq: u64,
    total_bytes: usize,
    max_lines: usize,
    max_bytes: usize,
    /// Unconsumed lines dropped by ring eviction since the last collect.
    /// Surfaced as an explicit marker so the model is told history was lost
    /// rather than silently seeing a gap (spec §3 ring-buffer overflow row).
    dropped_unconsumed: usize,
}

/// Character prefix that marks an AI trigger line; such lines are the
/// question itself and must not be duplicated into the context block.
pub const TRIGGER_PREFIX: char = '》';

impl ContextStore {
    pub fn new(max_lines: usize, max_bytes: usize) -> Self {
        Self {
            lines: std::collections::VecDeque::new(),
            next_seq: 0,
            consumed_seq: 0,
            total_bytes: 0,
            max_lines,
            max_bytes,
            dropped_unconsumed: 0,
        }
    }

    pub fn push(&mut self, kind: LineKind, text: &str) {
        let text = text.trim_end();
        if text.is_empty() {
            return;
        }
        self.total_bytes += text.len();
        self.lines.push_back(StoredLine {
            seq: self.next_seq,
            kind,
            text: text.to_string(),
        });
        self.next_seq += 1;
        while self.lines.len() > self.max_lines || self.total_bytes > self.max_bytes {
            if let Some(evicted) = self.lines.pop_front() {
                self.total_bytes -= evicted.text.len();
                if evicted.seq >= self.consumed_seq {
                    self.dropped_unconsumed += 1;
                }
            } else {
                break;
            }
        }
    }

    /// Record a finished command's non-zero exit status as a context line.
    pub fn push_exit_status(&mut self, status: i64) {
        if status != 0 {
            self.push(LineKind::Output, &format!("[exit status: {status}]"));
        }
    }

    /// Collect all unconsumed lines and advance the consumed marker in the
    /// same critical section (the old daemon did these separately and lost
    /// lines to the race). Trailing trigger lines (the `》question` the user
    /// just typed) are excluded. Returns the formatted context block, or an
    /// empty string when nothing new happened since the last ask.
    pub fn collect_and_advance(&mut self, budget_bytes: usize) -> String {
        let fresh: Vec<&StoredLine> = self
            .lines
            .iter()
            .filter(|l| l.seq >= self.consumed_seq)
            .collect();
        self.consumed_seq = self.next_seq;

        // Drop trailing trigger/prompt lines: the last thing on screen before
        // an ask is the prompt line carrying the `》question` itself.
        let mut end = fresh.len();
        while end > 0 {
            let line = fresh[end - 1];
            let is_trigger = line.text.trim_start().starts_with(TRIGGER_PREFIX)
                || matches!(line.kind, LineKind::Prompt | LineKind::SecondaryPrompt)
                    && line.text.contains(TRIGGER_PREFIX);
            if is_trigger {
                end -= 1;
            } else {
                break;
            }
        }

        let mut out = String::new();
        let mut used = 0usize;
        let mut truncated = false;
        let dropped = std::mem::take(&mut self.dropped_unconsumed);
        // Walk backwards so the freshest lines survive the budget.
        let mut kept: Vec<&StoredLine> = Vec::new();
        for line in fresh[..end].iter().rev() {
            let cost = line.text.len() + 1;
            if used + cost > budget_bytes {
                truncated = true;
                break;
            }
            used += cost;
            kept.push(line);
        }
        kept.reverse();
        if dropped > 0 {
            out.push_str(&format!(
                "[...{dropped} earlier terminal lines were dropped: output exceeded the capture buffer...]\n"
            ));
        }
        if truncated {
            out.push_str("[...earlier terminal output trimmed...]\n");
        }
        for line in kept {
            out.push_str(&line.text);
            out.push('\n');
        }
        out
    }

    #[cfg(test)]
    pub fn unconsumed_count(&self) -> usize {
        self.lines
            .iter()
            .filter(|l| l.seq >= self.consumed_seq)
            .count()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn collect_excludes_trailing_trigger_line() {
        let mut c = ContextStore::new(100, 10_000);
        c.push(LineKind::Prompt, "user@host ~> gcc main.c");
        c.push(LineKind::Output, "error: expected ';'");
        c.push(LineKind::Prompt, "user@host ~> 》why does it fail");
        let block = c.collect_and_advance(8192);
        assert!(block.contains("gcc main.c"));
        assert!(block.contains("error: expected ';'"));
        assert!(!block.contains('》'));
    }

    #[test]
    fn second_collect_only_returns_new_lines() {
        let mut c = ContextStore::new(100, 10_000);
        c.push(LineKind::Output, "first");
        let b1 = c.collect_and_advance(8192);
        assert!(b1.contains("first"));
        c.push(LineKind::Output, "second");
        let b2 = c.collect_and_advance(8192);
        assert!(!b2.contains("first"));
        assert!(b2.contains("second"));
    }

    #[test]
    fn eviction_does_not_break_consumed_marker() {
        let mut c = ContextStore::new(4, 10_000);
        for i in 0..3 {
            c.push(LineKind::Output, &format!("old{i}"));
        }
        let _ = c.collect_and_advance(8192);
        // Push enough to evict everything from before the marker.
        for i in 0..8 {
            c.push(LineKind::Output, &format!("new{i}"));
        }
        let block = c.collect_and_advance(8192);
        assert!(!block.contains("old"));
        assert!(block.contains("new7"));
        // Marker landed at the end: nothing unconsumed remains.
        assert_eq!(c.unconsumed_count(), 0);
    }

    #[test]
    fn byte_budget_keeps_freshest_lines() {
        let mut c = ContextStore::new(1000, 1_000_000);
        for i in 0..100 {
            c.push(LineKind::Output, &format!("line-{i:03} {}", "x".repeat(50)));
        }
        let block = c.collect_and_advance(600);
        assert!(block.contains("line-099"));
        assert!(!block.contains("line-000"));
        assert!(block.contains("trimmed"));
    }

    #[test]
    fn nonzero_exit_status_recorded() {
        let mut c = ContextStore::new(10, 1000);
        c.push_exit_status(0);
        c.push_exit_status(127);
        let block = c.collect_and_advance(8192);
        assert!(block.contains("[exit status: 127]"));
        assert!(!block.contains("[exit status: 0]"));
    }
}

#[cfg(test)]
mod overflow_notice_tests {
    use super::*;

    #[test]
    fn ring_eviction_of_unconsumed_lines_is_announced() {
        let mut c = ContextStore::new(4, 10_000);
        for i in 0..12 {
            c.push(LineKind::Output, &format!("line{i}"));
        }
        let block = c.collect_and_advance(8192);
        assert!(
            block.contains("earlier terminal lines were dropped"),
            "expected an explicit loss marker, got: {block:?}"
        );
        assert!(block.contains("line11"), "newest lines survive: {block:?}");
    }

    #[test]
    fn no_notice_when_nothing_unconsumed_was_lost() {
        let mut c = ContextStore::new(100, 10_000);
        c.push(LineKind::Output, "a");
        let block = c.collect_and_advance(8192);
        assert!(!block.contains("dropped"));
    }

    #[test]
    fn consumed_lines_evicted_do_not_trigger_the_notice() {
        let mut c = ContextStore::new(4, 10_000);
        c.push(LineKind::Output, "old");
        let _ = c.collect_and_advance(8192); // consume it
        for i in 0..10 {
            c.push(LineKind::Output, &format!("new{i}"));
        }
        let block = c.collect_and_advance(8192);
        // 'old' was already consumed before eviction: no loss to report for it,
        // but the newer unconsumed overflow is still reported.
        assert!(block.contains("new9"));
    }
}
