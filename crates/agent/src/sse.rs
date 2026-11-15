//! Server-sent events, one line at a time.
//!
//! Both protocols stream replies as SSE. The caller reads the response body
//! line by line (so multi-byte characters are never split) and feeds each line
//! here; a blank line completes a frame.

/// One complete event: its `event:` name (empty if none) and its `data:` lines joined.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    pub event: String,
    pub data: String,
}

#[derive(Debug, Default)]
pub struct Decoder {
    event: String,
    data: Vec<String>,
}

impl Decoder {
    /// Feed one line (with or without its line ending). Returns a frame when the line completes one.
    pub fn line(&mut self, line: &str) -> Option<Frame> {
        let line = line.trim_end_matches(['\n', '\r']);
        if line.is_empty() {
            return self.flush();
        }
        if let Some(v) = line.strip_prefix("data:") {
            self.data.push(v.strip_prefix(' ').unwrap_or(v).to_string());
        } else if let Some(v) = line.strip_prefix("event:") {
            self.event = v.trim().to_string();
        }
        // `:` comments, `id:` and `retry:` don't matter here.
        None
    }

    /// The last frame, if the stream ended without a blank line after it.
    pub fn finish(&mut self) -> Option<Frame> {
        self.flush()
    }

    fn flush(&mut self) -> Option<Frame> {
        // Per the SSE spec, a frame without data lines is not dispatched; its
        // `event:` name doesn't carry over to the next frame either.
        if self.data.is_empty() {
            self.event.clear();
            return None;
        }
        let frame = Frame { event: std::mem::take(&mut self.event), data: self.data.join("\n") };
        self.data.clear();
        Some(frame)
    }
}
