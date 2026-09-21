//! Limits on what an MCP server can send before rmcp reads it.
//!
//! rmcp's stdio transport reads a line at a time into a buffer with no
//! limit, so one endless line from a hostile server would grow it without
//! end (ADR 0034). The server's stdout is therefore read through
//! [`BoundedLines`], which fails the read once a single line passes its
//! bound. rmcp treats a failed read as the connection closing, which it is.
//!
//! The server's stderr is drained continuously into [`StderrTail`], which
//! keeps only the last few kilobytes: a server that writes to stderr without
//! end must neither block on a full pipe nor fill memory.

use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};

use tokio::io::{AsyncRead, AsyncReadExt, ReadBuf};

/// A reader that fails once any one line is longer than its bound.
pub(crate) struct BoundedLines<R> {
    inner: R,
    since_newline: usize,
    max_line: usize,
    exceeded: Arc<AtomicBool>,
}

impl<R> BoundedLines<R> {
    /// Read `inner`, allowing lines of at most `max_line` bytes. `exceeded`
    /// is set when the bound closes the stream, so the reason can be told.
    pub(crate) fn new(inner: R, max_line: usize, exceeded: Arc<AtomicBool>) -> Self {
        Self {
            inner,
            since_newline: 0,
            max_line,
            exceeded,
        }
    }
}

impl<R: AsyncRead + Unpin> AsyncRead for BoundedLines<R> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        let before = buf.filled().len();
        let this = &mut *self;
        match Pin::new(&mut this.inner).poll_read(cx, buf) {
            Poll::Ready(Ok(())) => {
                for &byte in &buf.filled()[before..] {
                    if byte == b'\n' {
                        this.since_newline = 0;
                    } else {
                        this.since_newline += 1;
                        if this.since_newline > this.max_line {
                            this.exceeded.store(true, Ordering::SeqCst);
                            // A read that fails hands over no bytes: readers
                            // above this one (tokio's, rmcp's buffer) may
                            // assume an error left the buffer as it was.
                            buf.set_filled(before);
                            return Poll::Ready(Err(std::io::Error::new(
                                std::io::ErrorKind::InvalidData,
                                "an MCP message was longer than the limit",
                            )));
                        }
                    }
                }
                Poll::Ready(Ok(()))
            }
            other => other,
        }
    }
}

/// The last bytes a server wrote to stderr, for diagnostics.
#[derive(Debug, Clone, Default)]
pub(crate) struct StderrTail {
    kept: Arc<Mutex<Vec<u8>>>,
}

impl StderrTail {
    /// Drain `stream` in the background, keeping at most `keep` bytes.
    pub(crate) fn start<R>(stream: Option<R>, keep: usize) -> Self
    where
        R: AsyncRead + Unpin + Send + 'static,
    {
        let tail = Self::default();
        if let Some(mut stream) = stream {
            let kept = Arc::clone(&tail.kept);
            tokio::spawn(async move {
                let mut chunk = [0_u8; 4096];
                loop {
                    match stream.read(&mut chunk).await {
                        Ok(0) | Err(_) => break,
                        Ok(read) => {
                            let Ok(mut kept) = kept.lock() else { break };
                            kept.extend_from_slice(&chunk[..read]);
                            if kept.len() > keep {
                                let excess = kept.len() - keep;
                                kept.drain(..excess);
                            }
                        }
                    }
                }
            });
        }
        tail
    }

    /// What was kept, as text. Not yet redacted; the caller redacts it.
    pub(crate) fn text(&self) -> String {
        self.kept
            .lock()
            .map(|kept| String::from_utf8_lossy(&kept).into_owned())
            .unwrap_or_default()
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    reason = "tests assert on known-good values; a panic is the failure report"
)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_line_within_the_bound_reads_whole() {
        let exceeded = Arc::new(AtomicBool::new(false));
        let mut reader = BoundedLines::new(&b"short\nlines\n"[..], 8, Arc::clone(&exceeded));
        let mut text = String::new();
        reader.read_to_string(&mut text).await.unwrap();
        assert_eq!(text, "short\nlines\n");
        assert!(!exceeded.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn a_line_past_the_bound_fails_the_read_and_says_so() {
        let exceeded = Arc::new(AtomicBool::new(false));
        let long = format!("ok\n{}\n", "x".repeat(64));
        let mut reader = BoundedLines::new(long.as_bytes(), 16, Arc::clone(&exceeded));
        let mut text = String::new();
        assert!(reader.read_to_string(&mut text).await.is_err());
        assert!(exceeded.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn the_tail_keeps_only_the_last_bytes() {
        let noise = format!("{}END", "n".repeat(10_000));
        let tail = StderrTail::start(Some(std::io::Cursor::new(noise.into_bytes())), 64);
        for _ in 0..100 {
            if tail.text().ends_with("END") {
                break;
            }
            tokio::task::yield_now().await;
        }
        let text = tail.text();
        assert!(text.ends_with("END"), "{text}");
        assert!(text.len() <= 64);
    }
}
