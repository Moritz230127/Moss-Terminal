//! AgentEvent → styled terminal bytes, buffered per window.
//!
//! The kitty side drains this queue on its main loop and feeds the bytes to
//! the window's VT parser, so everything here must be plain ANSI. Newlines
//! are emitted as CRLF because the bytes bypass the pty line discipline.

use crate::agent::AgentEvent;
use crate::llm::ChatStreamKind;
use std::sync::Mutex;

const GREEN: &str = "\x1b[38;5;10m";
const BLUE: &str = "\x1b[38;5;39m";
const DIM: &str = "\x1b[2m";
const RESET: &str = "\x1b[0m";

/// Stream lifecycle as observed by the poller.
pub const STATE_IDLE: i32 = 0;
pub const STATE_ACTIVE: i32 = 1;

#[derive(Debug, Default)]
struct Inner {
    buf: Vec<u8>,
    active: bool,
    /// Rendering state: are we currently inside a reasoning (green) span?
    in_reasoning: bool,
    /// Has any content been emitted this turn (controls leading separators)?
    emitted: bool,
    /// Instant the current reasoning span started (drives the collapse
    /// marker's duration field).
    reasoning_started: Option<std::time::Instant>,
    /// Plain-text safety net state: bytes withheld pending classification.
    line_buf: String,
    /// Current line's start was classified; stream its remainder directly.
    line_open: bool,
    /// Current line is being discarded (fence delimiter or table separator).
    line_dropped: bool,
    /// The discarded line was a fence delimiter: toggle fence at the newline.
    fence_toggle_pending: bool,
    /// Current line is a Markdown pipe-table row: buffer it whole, then
    /// rewrite (pipes cannot be degraded incrementally).
    line_table: bool,
    /// Inside a ``` fence (fence delimiters are dropped, body is indented).
    in_fence: bool,
}

/// Line-start classification for the plain-text degrader.
enum LineStart {
    /// Not enough characters yet to decide — keep buffering.
    Pending,
    /// A ``` fence delimiter: drop the line, toggle fence state.
    FenceDelimiter,
    /// Regular text; `strip` bytes of leading Markdown marker to remove.
    Text { strip: usize },
    /// A pipe-table row: buffer the whole line, rewrite it at the newline.
    TableRow,
}

/// Decide what the start of a line is, given the characters seen so far.
/// `complete` is true when the line is already terminated (no more chars
/// can arrive), which resolves otherwise-ambiguous prefixes.
fn classify_line_start(buf: &str, in_fence: bool, complete: bool) -> LineStart {
    let indent = buf.len() - buf.trim_start().len();
    let t = &buf[indent..];
    if t.is_empty() {
        return if complete {
            LineStart::Text { strip: 0 }
        } else {
            LineStart::Pending
        };
    }
    if t.starts_with('`') {
        if t.starts_with("```") {
            return LineStart::FenceDelimiter;
        }
        // Could still become a fence once more backticks arrive.
        if !complete && t.len() < 3 && t.bytes().all(|b| b == b'`') {
            return LineStart::Pending;
        }
        return LineStart::Text { strip: 0 };
    }
    if in_fence {
        // Inside a fence only the closing delimiter is special; everything
        // else is verbatim code.
        return LineStart::Text { strip: 0 };
    }
    if t.starts_with('|') {
        return LineStart::TableRow;
    }
    if t.starts_with('#') {
        let hashes = t.bytes().take_while(|b| *b == b'#').count();
        match t.as_bytes().get(hashes) {
            Some(b' ') if hashes <= 6 => {
                return LineStart::Text {
                    strip: indent + hashes + 1,
                }
            }
            Some(_) => return LineStart::Text { strip: 0 },
            None if !complete => return LineStart::Pending,
            None => return LineStart::Text { strip: 0 },
        }
    }
    LineStart::Text { strip: 0 }
}

/// Strip inline Markdown emphasis/code markers from an already-classified
/// text span (never applied inside a fence, where content is verbatim code).
fn strip_inline_markers(s: &str) -> String {
    s.replace("**", "").replace('`', "")
}

/// Degrade a complete Markdown pipe-table row to plain text. Separator rows
/// (|---|---|) vanish; data rows lose their pipes, cells joined by two
/// spaces. Returns None to drop the line.
fn degrade_table_row(line: &str) -> Option<String> {
    let t = line.trim();
    let inner = t.trim_start_matches('|').trim_end_matches('|');
    if inner.chars().all(|c| matches!(c, '-' | ':' | '|' | ' ')) && inner.contains('-') {
        return None; // alignment separator row
    }
    let cells: Vec<String> = inner
        .split('|')
        .map(|c| strip_inline_markers(c.trim()))
        .collect();
    Some(cells.join("  "))
}

/// How many trailing bytes must be withheld because they might be the first
/// half of an inline marker (`**`) split across stream chunks.
fn inline_hold_len(s: &str) -> usize {
    let mut hold = 0usize;
    for ch in s.chars().rev() {
        if (ch == '*' || ch == '`') && hold < 2 {
            hold += ch.len_utf8();
        } else {
            break;
        }
    }
    hold
}

#[derive(Debug, Default)]
pub struct OutputQueue {
    inner: Mutex<Inner>,
}

fn crlf(text: &str) -> String {
    text.replace('\n', "\r\n")
}

impl OutputQueue {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn begin(&self) {
        let mut g = self.inner.lock().unwrap();
        g.active = true;
        g.in_reasoning = false;
        g.emitted = false;
        g.reasoning_started = None;
        g.line_buf.clear();
        g.line_open = false;
        g.line_dropped = false;
        g.fence_toggle_pending = false;
        g.line_table = false;
        g.in_fence = false;
    }

    pub fn state(&self) -> i32 {
        let g = self.inner.lock().unwrap();
        if g.active || !g.buf.is_empty() {
            STATE_ACTIVE
        } else {
            STATE_IDLE
        }
    }

    fn push_str(g: &mut Inner, s: &str) {
        g.buf.extend_from_slice(s.as_bytes());
    }

    fn close_reasoning(g: &mut Inner) {
        if g.in_reasoning {
            Self::push_str(g, RESET);
            Self::push_str(g, "\r\n");
            // APC end marker: lets the kitty side collapse the finished
            // reasoning block (invisible if it reaches the VT parser).
            let secs = g
                .reasoning_started
                .map(|t| t.elapsed().as_secs_f32())
                .unwrap_or(0.0);
            let marker = format!("\x1b_moss;re;{secs:.1}\x1b\\");
            Self::push_str(g, &marker);
            g.in_reasoning = false;
            g.reasoning_started = None;
        }
    }

    /// Route content through the plain-text degrader. Emission is
    /// incremental — only the few bytes that could still be part of a
    /// Markdown construct are withheld, so streaming latency is unaffected
    /// even for models that emit long unbroken paragraphs.
    fn push_content(g: &mut Inner, text: &str) {
        for ch in text.chars() {
            if ch == '\n' {
                Self::emit_pending(g, true);
                if g.fence_toggle_pending {
                    g.in_fence = !g.in_fence;
                }
                if !g.line_dropped {
                    Self::push_str(g, "\r\n");
                }
                g.line_buf.clear();
                g.line_open = false;
                g.line_dropped = false;
                g.fence_toggle_pending = false;
                g.line_table = false;
            } else if ch != '\r' {
                g.line_buf.push(ch);
                Self::emit_pending(g, false);
            }
        }
    }

    /// Emit everything currently emittable from `line_buf`.
    fn emit_pending(g: &mut Inner, complete: bool) {
        if g.line_dropped {
            g.line_buf.clear();
            return;
        }
        if g.line_table {
            // Table rows are only rewritable once the whole line is known.
            if !complete {
                return;
            }
            let line = std::mem::take(&mut g.line_buf);
            match degrade_table_row(&line) {
                Some(rendered) => Self::push_str(g, &rendered),
                None => g.line_dropped = true, // alignment separator row
            }
            return;
        }
        if !g.line_open {
            match classify_line_start(&g.line_buf, g.in_fence, complete) {
                LineStart::Pending => return,
                LineStart::FenceDelimiter => {
                    g.line_dropped = true;
                    g.fence_toggle_pending = true;
                    g.line_buf.clear();
                    return;
                }
                LineStart::TableRow => {
                    g.line_table = true;
                    if !complete {
                        return;
                    }
                    let line = std::mem::take(&mut g.line_buf);
                    match degrade_table_row(&line) {
                        Some(rendered) => Self::push_str(g, &rendered),
                        None => g.line_dropped = true,
                    }
                    return;
                }
                LineStart::Text { strip } => {
                    if strip > 0 {
                        g.line_buf.drain(..strip);
                    }
                    if g.in_fence {
                        Self::push_str(g, "    ");
                    }
                    g.line_open = true;
                }
            }
        }
        let hold = if complete {
            0
        } else {
            inline_hold_len(&g.line_buf)
        };
        if hold >= g.line_buf.len() {
            return;
        }
        let emit: String = g.line_buf.drain(..g.line_buf.len() - hold).collect();
        if emit.is_empty() {
            return;
        }
        let rendered = if g.in_fence {
            emit
        } else {
            strip_inline_markers(&emit)
        };
        Self::push_str(g, &rendered);
    }

    fn flush_content(g: &mut Inner) {
        if !g.line_buf.is_empty() || g.line_open || g.line_table {
            Self::emit_pending(g, true);
            g.line_buf.clear();
            g.line_open = false;
            g.line_dropped = false;
            g.fence_toggle_pending = false;
            g.line_table = false;
        }
    }

    pub fn push_event(&self, event: &AgentEvent) {
        let mut g = self.inner.lock().unwrap();
        match event {
            AgentEvent::Chunk(chunk) => match chunk.kind {
                ChatStreamKind::Reasoning => {
                    if !g.in_reasoning {
                        // APC start marker precedes the styled reasoning
                        // block (see close_reasoning for the end marker).
                        Self::push_str(&mut g, "\x1b_moss;rs\x1b\\");
                        Self::push_str(&mut g, GREEN);
                        g.in_reasoning = true;
                        g.emitted = true;
                        g.reasoning_started = Some(std::time::Instant::now());
                    }
                    let text = crlf(&chunk.text);
                    Self::push_str(&mut g, &text);
                }
                ChatStreamKind::Content => {
                    Self::close_reasoning(&mut g);
                    g.emitted = true;
                    Self::push_content(&mut g, &chunk.text);
                }
                ChatStreamKind::ReasoningReset => {
                    if g.in_reasoning {
                        Self::push_str(&mut g, RESET);
                        Self::push_str(&mut g, "\r\n");
                        Self::push_str(&mut g, DIM);
                        Self::push_str(&mut g, "[endpoint failover: reasoning restarted]");
                        Self::push_str(&mut g, RESET);
                        Self::push_str(&mut g, "\r\n");
                        g.in_reasoning = false;
                    }
                }
                ChatStreamKind::ReasoningPartStart | ChatStreamKind::ReasoningPartEnd => {}
                ChatStreamKind::ToolCall => {}
            },
            AgentEvent::ToolCall { name, arguments } => {
                Self::close_reasoning(&mut g);
                let args_short: String = arguments.chars().take(120).collect();
                let line = format!("{BLUE}▶ {name} {args_short}{RESET}\r\n");
                Self::push_str(&mut g, &line);
                g.emitted = true;
            }
            AgentEvent::ToolResult { name, ok, .. } => {
                let mark = if *ok { "✓" } else { "✗" };
                let line = format!("{BLUE}{mark} {name}{RESET}\r\n");
                Self::push_str(&mut g, &line);
            }
            AgentEvent::ToolProgress { message, .. } => {
                let line = format!("{DIM}{}{RESET}\r\n", crlf(message));
                Self::push_str(&mut g, &line);
            }
            AgentEvent::CommandOutput { chunk, .. } => {
                Self::push_str(&mut g, DIM);
                let text = crlf(&String::from_utf8_lossy(chunk));
                Self::push_str(&mut g, &text);
                Self::push_str(&mut g, RESET);
            }
            AgentEvent::CompactStart => {
                Self::close_reasoning(&mut g);
                Self::push_str(&mut g, DIM);
                Self::push_str(&mut g, "[compacting conversation...]");
                Self::push_str(&mut g, RESET);
                Self::push_str(&mut g, "\r\n");
            }
            AgentEvent::CompactEnd => {
                Self::push_str(&mut g, DIM);
                Self::push_str(&mut g, "[compacted]");
                Self::push_str(&mut g, RESET);
                Self::push_str(&mut g, "\r\n");
            }
            AgentEvent::PrepareForExternalOutput { .. } | AgentEvent::AskQuestion { .. } => {
                // Handled by the caller (they carry oneshot responders).
            }
            _ => {}
        }
    }

    pub fn push_error(&self, message: &str) {
        let mut g = self.inner.lock().unwrap();
        Self::close_reasoning(&mut g);
        let line = format!("\x1b[31m[moss error] {}{RESET}\r\n", crlf(message));
        Self::push_str(&mut g, &line);
    }

    pub fn push_notice(&self, message: &str) {
        let mut g = self.inner.lock().unwrap();
        let line = format!("{DIM}{}{RESET}\r\n", crlf(message));
        Self::push_str(&mut g, &line);
    }

    /// Mark the stream finished. Trailing reset + newline are appended so the
    /// shell prompt that follows starts on a clean line.
    pub fn finish(&self) {
        let mut g = self.inner.lock().unwrap();
        Self::close_reasoning(&mut g);
        Self::flush_content(&mut g);
        if g.emitted {
            Self::push_str(&mut g, RESET);
            Self::push_str(&mut g, "\r\n");
        }
        g.active = false;
    }

    /// Drain up to `cap` bytes into `dest`. Returns the number of bytes
    /// copied. Partial UTF-8/ANSI sequences at the boundary are fine — the
    /// consumer feeds a streaming VT parser.
    pub fn drain_into(&self, dest: &mut [u8]) -> usize {
        let mut g = self.inner.lock().unwrap();
        let n = dest.len().min(g.buf.len());
        if n > 0 {
            dest[..n].copy_from_slice(&g.buf[..n]);
            g.buf.drain(..n);
        }
        n
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::ChatStreamChunk;

    fn chunk(kind: ChatStreamKind, text: &str) -> AgentEvent {
        AgentEvent::Chunk(ChatStreamChunk {
            kind,
            text: text.to_string(),
        })
    }

    #[test]
    fn reasoning_then_content_colors_and_state() {
        let q = OutputQueue::new();
        q.begin();
        assert_eq!(q.state(), STATE_ACTIVE);
        q.push_event(&chunk(ChatStreamKind::Reasoning, "thinking"));
        q.push_event(&chunk(ChatStreamKind::Content, "answer\nline2"));
        q.finish();
        let mut buf = vec![0u8; 4096];
        let n = q.drain_into(&mut buf);
        let s = String::from_utf8_lossy(&buf[..n]).to_string();
        assert!(s.contains(GREEN));
        assert!(s.contains("thinking"));
        assert!(s.contains("answer\r\nline2"));
        assert_eq!(q.state(), STATE_IDLE);
    }

    #[test]
    fn drain_respects_cap_and_preserves_rest() {
        let q = OutputQueue::new();
        q.begin();
        q.push_event(&chunk(ChatStreamKind::Content, "0123456789"));
        // Content is line-buffered; finish() flushes the partial line.
        q.finish();
        let mut small = vec![0u8; 4];
        let n1 = q.drain_into(&mut small);
        assert_eq!(n1, 4);
        let mut rest = vec![0u8; 4096];
        let n2 = q.drain_into(&mut rest);
        assert!(n2 > 0);
        let combined = [&small[..n1], &rest[..n2]].concat();
        assert!(String::from_utf8_lossy(&combined).contains("0123456789"));
    }

    #[test]
    fn markdown_is_degraded_to_plain_text_across_chunks() {
        let q = OutputQueue::new();
        q.begin();
        // Markdown constructs split across stream chunk boundaries.
        q.push_event(&chunk(ChatStreamKind::Content, "## Ti"));
        q.push_event(&chunk(ChatStreamKind::Content, "tle\n**bo"));
        q.push_event(&chunk(
            ChatStreamKind::Content,
            "ld** and `code`\n```bash\nls -l\n``",
        ));
        q.push_event(&chunk(ChatStreamKind::Content, "`\ntail"));
        q.finish();
        let mut buf = vec![0u8; 4096];
        let n = q.drain_into(&mut buf);
        let s = String::from_utf8_lossy(&buf[..n]).to_string();
        assert!(s.contains("Title\r\n"));
        assert!(s.contains("bold and code\r\n"));
        assert!(
            s.contains("    ls -l\r\n"),
            "fence body kept, indented: {s:?}"
        );
        assert!(!s.contains("```"));
        assert!(!s.contains("**"));
        assert!(!s.contains('#'));
        assert!(s.contains("tail"));
    }

    #[test]
    fn markdown_tables_are_degraded() {
        let q = OutputQueue::new();
        q.begin();
        q.push_event(&chunk(
            ChatStreamKind::Content,
            "| 名称 | 大小 |\n|---|---:|\n| **WarThunder** | 146G |\nend\n",
        ));
        q.finish();
        let mut buf = vec![0u8; 4096];
        let n = q.drain_into(&mut buf);
        let s = String::from_utf8_lossy(&buf[..n]).to_string();
        assert!(!s.contains('|'), "pipes removed: {s:?}");
        assert!(!s.contains("---"), "separator row dropped: {s:?}");
        assert!(s.contains("名称  大小"), "cells joined: {s:?}");
        assert!(
            s.contains("WarThunder  146G"),
            "bold stripped in cell: {s:?}"
        );
        assert!(s.contains("end"));
    }

    #[test]
    fn long_paragraph_streams_without_waiting_for_newline() {
        // Regression: content must not be withheld until a newline arrives,
        // otherwise models that stream long paragraphs appear frozen.
        let q = OutputQueue::new();
        q.begin();
        q.push_event(&chunk(ChatStreamKind::Content, "hello world, this streams"));
        let mut buf = vec![0u8; 4096];
        let n = q.drain_into(&mut buf);
        let s = String::from_utf8_lossy(&buf[..n]).to_string();
        assert!(s.contains("hello world, this streams"), "got {s:?}");
    }

    #[test]
    fn reasoning_block_is_wrapped_in_apc_markers() {
        let q = OutputQueue::new();
        q.begin();
        q.push_event(&chunk(ChatStreamKind::Reasoning, "hmm"));
        q.push_event(&chunk(ChatStreamKind::Content, "done"));
        q.finish();
        let mut buf = vec![0u8; 4096];
        let n = q.drain_into(&mut buf);
        let s = String::from_utf8_lossy(&buf[..n]).to_string();
        assert!(s.contains("\x1b_moss;rs\x1b\\"));
        assert!(s.contains("\x1b_moss;re;"));
        let start = s.find("\x1b_moss;rs").unwrap();
        let end = s.find("\x1b_moss;re;").unwrap();
        assert!(start < end);
        assert!(s[end..].contains("done"));
    }

    #[test]
    fn state_active_until_finished_and_drained() {
        let q = OutputQueue::new();
        q.begin();
        q.push_event(&chunk(ChatStreamKind::Content, "x"));
        q.finish();
        assert_eq!(q.state(), STATE_ACTIVE); // bytes still pending
        let mut buf = vec![0u8; 64];
        while q.drain_into(&mut buf) > 0 {}
        assert_eq!(q.state(), STATE_IDLE);
    }
}
