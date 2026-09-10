//! Reading Server-Sent Events from a byte stream.
//!
//! Deliberately small and local rather than a dependency. What these providers
//! actually use is a narrow subset — `data:` lines, blank-line separated, with
//! a `[DONE]` sentinel — and a general SSE implementation would bring field
//! parsing, retry hints, and event types that no adapter here reads.
//!
//! The part worth being careful about is that a chunk boundary can fall
//! anywhere, including inside a JSON object or between the `\n\n` that ends an
//! event. Buffering until a terminator is seen is the whole job.

/// Accumulates bytes and yields complete `data:` payloads.
#[derive(Debug, Default)]
pub(crate) struct EventStreamDecoder {
    buffer: String,
}

impl EventStreamDecoder {
    /// A decoder holding nothing.
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Add a chunk and return every complete payload it finished.
    ///
    /// Lossy UTF-8 rather than a decode error: a provider that emits a
    /// malformed byte should cost one replacement character, not the whole
    /// answer the user was waiting for.
    pub(crate) fn push(&mut self, chunk: &[u8]) -> Vec<String> {
        self.buffer.push_str(&String::from_utf8_lossy(chunk));

        let mut payloads = Vec::new();
        // Events are separated by a blank line. `\r\n\r\n` is normalized first
        // so a provider using CRLF is not treated as one long unterminated
        // event.
        self.buffer = self.buffer.replace("\r\n", "\n");
        while let Some(end) = self.buffer.find("\n\n") {
            let event: String = self.buffer.drain(..end + 2).collect();
            if let Some(payload) = data_of(&event) {
                payloads.push(payload);
            }
        }
        payloads
    }

    /// Whatever is left when the connection closes without a final blank line.
    ///
    /// Providers do this. Discarding it would drop the last token of an
    /// otherwise complete answer.
    pub(crate) fn finish(&mut self) -> Option<String> {
        let remaining = std::mem::take(&mut self.buffer);
        data_of(&remaining)
    }
}

/// The concatenated `data:` payload of one event, if it has any.
fn data_of(event: &str) -> Option<String> {
    let mut data = String::new();
    for line in event.lines() {
        // A comment keeps a connection alive and carries nothing.
        if line.starts_with(':') {
            continue;
        }
        let Some(value) = line.strip_prefix("data:") else {
            continue;
        };
        if !data.is_empty() {
            data.push('\n');
        }
        data.push_str(value.strip_prefix(' ').unwrap_or(value));
    }
    (!data.trim().is_empty()).then_some(data)
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "tests assert on known-good literals; a panic is the failure report"
)]
mod tests {
    use super::*;

    #[test]
    fn a_complete_event_yields_its_payload() {
        let mut decoder = EventStreamDecoder::new();
        assert_eq!(decoder.push(b"data: {\"a\":1}\n\n"), vec!["{\"a\":1}"]);
    }

    #[test]
    fn a_payload_split_across_chunks_is_reassembled() {
        // The property that matters: a chunk boundary can fall anywhere,
        // including inside a JSON object.
        let mut decoder = EventStreamDecoder::new();
        assert!(decoder.push(b"data: {\"content\":\"hel").is_empty());
        assert!(decoder.push(b"lo\"}").is_empty());
        assert_eq!(decoder.push(b"\n\n"), vec!["{\"content\":\"hello\"}"]);
    }

    #[test]
    fn several_events_in_one_chunk_all_arrive_in_order() {
        let mut decoder = EventStreamDecoder::new();
        assert_eq!(
            decoder.push(b"data: one\n\ndata: two\n\ndata: three\n\n"),
            vec!["one", "two", "three"]
        );
    }

    #[test]
    fn crlf_line_endings_are_understood() {
        let mut decoder = EventStreamDecoder::new();
        assert_eq!(decoder.push(b"data: {\"a\":1}\r\n\r\n"), vec!["{\"a\":1}"]);
    }

    #[test]
    fn comments_and_other_fields_carry_nothing() {
        let mut decoder = EventStreamDecoder::new();
        assert!(decoder.push(b": keep-alive\n\n").is_empty());
        assert!(decoder.push(b"event: ping\nid: 7\n\n").is_empty());
    }

    #[test]
    fn a_final_event_without_a_blank_line_is_not_lost() {
        // Providers close the connection after the last event. Discarding the
        // remainder would drop the last token of a complete answer.
        let mut decoder = EventStreamDecoder::new();
        assert!(decoder.push(b"data: last").is_empty());
        assert_eq!(decoder.finish(), Some("last".to_owned()));
        assert_eq!(decoder.finish(), None, "the buffer is consumed once");
    }

    #[test]
    fn a_multi_line_payload_is_joined_with_newlines() {
        let mut decoder = EventStreamDecoder::new();
        assert_eq!(decoder.push(b"data: one\ndata: two\n\n"), vec!["one\ntwo"]);
    }

    #[test]
    fn an_invalid_byte_costs_one_character_rather_than_the_answer() {
        let mut decoder = EventStreamDecoder::new();
        let payloads = decoder.push(b"data: ok\xff\n\n");
        assert_eq!(payloads.len(), 1);
        assert!(payloads[0].starts_with("ok"));
    }
}
