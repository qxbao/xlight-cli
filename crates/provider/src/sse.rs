// SPDX-License-Identifier: GPL-3.0-only

//! Incremental Server-Sent Events parser (PATTERNS.md §6, port map docs/PLAN.md §4.5
//! `bridge/sse.ts` → `provider::sse`).
//!
//! Wraps any `Stream<Item = Result<Bytes, ProviderError>>` (e.g. `reqwest::Response::bytes_stream`)
//! and yields fully parsed [`SseEvent`]s, following the WHATWG "processing model" for
//! `text/event-stream` closely enough for the providers we target: `\n`, `\r\n` and lone `\r` line
//! terminators, `:`-prefixed comments, multi-line `data:` fields joined by `\n`, and `event`/`id`/
//! `retry` fields. Chunk boundaries may split anywhere — mid-line, mid-field, even mid-CRLF — the
//! parser buffers until it has a full line.

use std::collections::VecDeque;
use std::pin::Pin;
use std::task::{Context, Poll};

use bytes::{Bytes, BytesMut};
use futures::Stream;

use xlightcli_protocol::ProviderError;

/// One parsed Server-Sent Event.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SseEvent {
    pub event: Option<String>,
    /// All `data:` lines for this event, joined by `\n` (trailing newline already removed).
    pub data: String,
    pub id: Option<String>,
    pub retry: Option<u64>,
}

#[derive(Default)]
struct EventAccumulator {
    event: Option<String>,
    data_lines: Vec<String>,
    id: Option<String>,
    retry: Option<u64>,
}

impl EventAccumulator {
    fn has_data(&self) -> bool {
        !self.data_lines.is_empty()
    }

    /// Per spec: an event with an empty data buffer is not dispatched.
    fn dispatch(&mut self) -> Option<SseEvent> {
        if !self.has_data() {
            *self = Self::default();
            return None;
        }
        let event = SseEvent {
            event: self.event.take(),
            data: self.data_lines.join("\n"),
            id: self.id.take(),
            retry: self.retry.take(),
        };
        *self = Self::default();
        Some(event)
    }

    fn apply_field(&mut self, field: &str, value: &str) {
        match field {
            "event" => self.event = Some(value.to_string()),
            "data" => self.data_lines.push(value.to_string()),
            "id" if !value.contains('\0') => self.id = Some(value.to_string()),
            "retry" => {
                if let Ok(ms) = value.parse::<u64>() {
                    self.retry = Some(ms);
                }
            }
            _ => {} // unknown field: ignore (PATTERNS.md §5 "harmless unknown field")
        }
    }
}

/// Incremental parser. See module docs for the exact behavior.
pub struct SseParser<S> {
    inner: S,
    buffer: BytesMut,
    inner_finished: bool,
    pending: VecDeque<SseEvent>,
    acc: EventAccumulator,
}

/// Wraps `inner` in an incremental SSE parser.
pub fn parse<S>(inner: S) -> SseParser<S>
where
    S: Stream<Item = Result<Bytes, ProviderError>>,
{
    SseParser {
        inner,
        buffer: BytesMut::new(),
        inner_finished: false,
        pending: VecDeque::new(),
        acc: EventAccumulator::default(),
    }
}

impl<S> std::fmt::Debug for SseParser<S> {
    /// Manual impl so `S` need not be `Debug` — only internal buffering state is shown.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SseParser")
            .field("buffered_bytes", &self.buffer.len())
            .field("inner_finished", &self.inner_finished)
            .field("pending_events", &self.pending.len())
            .finish()
    }
}

impl<S> SseParser<S> {
    /// Extracts and processes as many complete lines as are currently buffered. Returns `true`
    /// if at least one line was consumed.
    fn drain_lines(&mut self, flush_incomplete_tail: bool) -> bool {
        let mut consumed_any = false;
        while let Some((line_len, consumed_len)) =
            find_line_end(&self.buffer, flush_incomplete_tail)
        {
            let chunk = self.buffer.split_to(consumed_len);
            self.process_line(&chunk[..line_len]);
            consumed_any = true;
        }
        consumed_any
    }

    fn process_line(&mut self, line: &[u8]) {
        let line = String::from_utf8_lossy(line);
        if line.is_empty() {
            if let Some(event) = self.acc.dispatch() {
                self.pending.push_back(event);
            }
            return;
        }
        if line.starts_with(':') {
            return; // comment line
        }
        match line.find(':') {
            Some(idx) => {
                let field = &line[..idx];
                let mut value = &line[idx + 1..];
                if let Some(stripped) = value.strip_prefix(' ') {
                    value = stripped;
                }
                self.acc.apply_field(field, value);
            }
            None => self.acc.apply_field(&line, ""),
        }
    }
}

/// Finds the next line terminator in `buf`. Returns `(line_len, consumed_len)`: `line_len`
/// excludes the terminator, `consumed_len` includes it. A lone `\r` at the very end of the
/// currently available buffer is ambiguous (could be the start of a `\r\n` split across a chunk
/// boundary) and is only treated as a terminator once `flush_incomplete_tail` is set (inner
/// stream ended).
fn find_line_end(buf: &[u8], flush_incomplete_tail: bool) -> Option<(usize, usize)> {
    let mut i = 0;
    while i < buf.len() {
        match buf[i] {
            b'\n' => return Some((i, i + 1)),
            b'\r' => {
                return match buf.get(i + 1) {
                    Some(b'\n') => Some((i, i + 2)),
                    Some(_) => Some((i, i + 1)),
                    None if flush_incomplete_tail => Some((i, i + 1)),
                    None => None,
                };
            }
            _ => i += 1,
        }
    }
    if flush_incomplete_tail && !buf.is_empty() {
        Some((buf.len(), buf.len()))
    } else {
        None
    }
}

impl<S> Stream for SseParser<S>
where
    S: Stream<Item = Result<Bytes, ProviderError>> + Unpin,
{
    type Item = Result<SseEvent, ProviderError>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        loop {
            if let Some(event) = this.pending.pop_front() {
                return Poll::Ready(Some(Ok(event)));
            }
            if this.inner_finished {
                if this.drain_lines(true) {
                    continue;
                }
                if let Some(event) = this.acc.dispatch() {
                    return Poll::Ready(Some(Ok(event)));
                }
                return Poll::Ready(None);
            }
            match Pin::new(&mut this.inner).poll_next(cx) {
                Poll::Ready(Some(Ok(bytes))) => {
                    this.buffer.extend_from_slice(&bytes);
                    this.drain_lines(false);
                    continue;
                }
                Poll::Ready(Some(Err(err))) => return Poll::Ready(Some(Err(err))),
                Poll::Ready(None) => {
                    this.inner_finished = true;
                    continue;
                }
                Poll::Pending => return Poll::Pending,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use futures::StreamExt;
    use pretty_assertions::assert_eq;

    use super::*;

    /// Splits `raw` into single-byte chunks to exercise "chunk boundary splits anywhere",
    /// including mid-CRLF and mid-field-name.
    async fn parse_as_single_byte_chunks(raw: &[u8]) -> Vec<SseEvent> {
        let chunks: Vec<Result<Bytes, ProviderError>> = raw
            .iter()
            .map(|b| Ok(Bytes::copy_from_slice(&[*b])))
            .collect();
        let stream = futures::stream::iter(chunks);
        parse(stream).map(|r| r.unwrap()).collect().await
    }

    async fn parse_whole(raw: &[u8]) -> Vec<SseEvent> {
        let stream =
            futures::stream::iter(vec![Ok::<_, ProviderError>(Bytes::copy_from_slice(raw))]);
        parse(stream).map(|r| r.unwrap()).collect().await
    }

    #[tokio::test]
    async fn basic_single_line_event() {
        let events = parse_whole(b"data: hello\n\n").await;
        assert_eq!(
            events,
            vec![SseEvent {
                data: "hello".into(),
                ..Default::default()
            }]
        );
    }

    #[tokio::test]
    async fn crlf_line_endings() {
        let events = parse_whole(b"data: hello\r\n\r\n").await;
        assert_eq!(
            events,
            vec![SseEvent {
                data: "hello".into(),
                ..Default::default()
            }]
        );
    }

    #[tokio::test]
    async fn lone_cr_line_endings() {
        let events = parse_whole(b"data: hello\r\r").await;
        assert_eq!(
            events,
            vec![SseEvent {
                data: "hello".into(),
                ..Default::default()
            }]
        );
    }

    #[tokio::test]
    async fn multi_line_data_is_joined_with_newline() {
        let events = parse_whole(b"data: line one\ndata: line two\n\n").await;
        assert_eq!(
            events,
            vec![SseEvent {
                data: "line one\nline two".into(),
                ..Default::default()
            }]
        );
    }

    #[tokio::test]
    async fn comments_are_ignored() {
        let events = parse_whole(b": this is a comment\ndata: hello\n\n").await;
        assert_eq!(
            events,
            vec![SseEvent {
                data: "hello".into(),
                ..Default::default()
            }]
        );
    }

    #[tokio::test]
    async fn event_and_id_fields_are_captured() {
        let events = parse_whole(b"event: turn_started\nid: 42\ndata: hi\n\n").await;
        assert_eq!(
            events,
            vec![SseEvent {
                event: Some("turn_started".into()),
                data: "hi".into(),
                id: Some("42".into()),
                retry: None,
            }]
        );
    }

    #[tokio::test]
    async fn retry_field_is_parsed_as_u64() {
        let events = parse_whole(b"retry: 3000\ndata: hi\n\n").await;
        assert_eq!(events[0].retry, Some(3000));
    }

    #[tokio::test]
    async fn non_numeric_retry_is_ignored() {
        let events = parse_whole(b"retry: not-a-number\ndata: hi\n\n").await;
        assert_eq!(events[0].retry, None);
    }

    #[tokio::test]
    async fn field_without_colon_has_empty_value() {
        // A bare "data" line (no colon) is valid per spec and appends an empty data line.
        let events = parse_whole(b"data\n\n").await;
        assert_eq!(
            events,
            vec![SseEvent {
                data: String::new(),
                ..Default::default()
            }]
        );
    }

    #[tokio::test]
    async fn event_with_empty_data_buffer_is_not_dispatched() {
        let events = parse_whole(b"event: ping\n\ndata: real\n\n").await;
        assert_eq!(
            events,
            vec![SseEvent {
                data: "real".into(),
                ..Default::default()
            }]
        );
    }

    #[tokio::test]
    async fn multiple_events_in_one_chunk() {
        let events = parse_whole(b"data: one\n\ndata: two\n\n").await;
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].data, "one");
        assert_eq!(events[1].data, "two");
    }

    #[tokio::test]
    async fn trailing_event_without_final_blank_line_is_flushed_at_stream_end() {
        let events = parse_whole(b"data: no trailing blank line").await;
        assert_eq!(
            events,
            vec![SseEvent {
                data: "no trailing blank line".into(),
                ..Default::default()
            }]
        );
    }

    #[tokio::test]
    async fn unknown_field_is_ignored_harmlessly() {
        let events = parse_whole(b"totally_unknown_field: whatever\ndata: hi\n\n").await;
        assert_eq!(
            events,
            vec![SseEvent {
                data: "hi".into(),
                ..Default::default()
            }]
        );
    }

    #[tokio::test]
    async fn chunk_boundaries_split_anywhere_single_byte_at_a_time() {
        let raw = b"event: turn_started\r\nid: 1\r\ndata: line one\r\ndata: line two\r\n\r\ndata: second event\r\n\r\n";
        let events = parse_as_single_byte_chunks(raw).await;
        assert_eq!(
            events,
            vec![
                SseEvent {
                    event: Some("turn_started".into()),
                    data: "line one\nline two".into(),
                    id: Some("1".into()),
                    retry: None,
                },
                SseEvent {
                    data: "second event".into(),
                    ..Default::default()
                },
            ]
        );
    }

    #[tokio::test]
    async fn value_leading_space_is_stripped_once() {
        let events = parse_whole(b"data:  two leading spaces\n\n").await;
        assert_eq!(events[0].data, " two leading spaces");
    }

    #[tokio::test]
    async fn upstream_error_propagates_through_the_parser() {
        let stream =
            futures::stream::iter(vec![Err::<Bytes, _>(ProviderError::Network("boom".into()))]);
        let mut parser = parse(stream);
        let result = parser.next().await;
        assert!(matches!(result, Some(Err(ProviderError::Network(_)))));
    }
}
