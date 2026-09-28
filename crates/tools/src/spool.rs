// SPDX-License-Identifier: GPL-3.0-only

//! `OutputSpool` (INV-7, PATTERNS.md §9): every byte a tool produces is streamed straight to an
//! artifact file; only a bounded head + tail window is ever kept in RAM. A 200 MB tool output
//! grows RAM by at most `head_bytes + tail_bytes`, never by the full output size.

use std::collections::VecDeque;
use std::path::PathBuf;

use tokio::io::AsyncWriteExt;
use xlightcli_protocol::{SessionId, ToolCallId};

/// Points at the on-disk artifact a [`SpooledOutput`] was written to
/// (`$DATA/artifacts/<session>/<call-id>.log`, CODEBASE.md §6). The model can ask `read_file` for
/// a byte range of this path to see more than the head/tail window.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtifactRef {
    pub session_id: SessionId,
    pub call_id: ToolCallId,
    pub path: PathBuf,
}

/// Head/tail window sizes. Defaults match `xlightcli_config::ToolsConfig` (8 KiB / 32 KiB,
/// docs/PLAN.md §7.5) — construct via [`Self::from_config`] rather than duplicating the numbers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SpoolLimits {
    pub head_bytes: usize,
    pub tail_bytes: usize,
}

impl Default for SpoolLimits {
    fn default() -> Self {
        Self {
            head_bytes: 8 * 1024,
            tail_bytes: 32 * 1024,
        }
    }
}

impl SpoolLimits {
    pub fn from_config(cfg: &xlightcli_config::ToolsConfig) -> Self {
        Self {
            head_bytes: cfg.spool_head_bytes as usize,
            tail_bytes: cfg.spool_tail_bytes as usize,
        }
    }
}

/// What the model (and the TUI) actually receives instead of the full output (PATTERNS.md §9):
/// the first `head_bytes`, the last `tail_bytes`, total size/line count, whether anything was
/// dropped from the middle, and a pointer to the full artifact file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpooledOutput {
    pub head: Vec<u8>,
    pub tail: Vec<u8>,
    pub total_bytes: u64,
    pub total_lines: u64,
    pub truncated: bool,
    pub artifact: ArtifactRef,
}

impl SpooledOutput {
    /// Lossy UTF-8 view of the head window, for a quick text summary.
    pub fn head_text(&self) -> String {
        String::from_utf8_lossy(&self.head).into_owned()
    }

    /// Lossy UTF-8 view of the tail window.
    pub fn tail_text(&self) -> String {
        String::from_utf8_lossy(&self.tail).into_owned()
    }
}

/// A tool output sink: `write_chunk` streams bytes to the artifact file while updating the
/// in-memory head/tail windows; `finish` closes the file and returns the [`SpooledOutput`]
/// summary.
pub struct OutputSpool {
    limits: SpoolLimits,
    artifact: ArtifactRef,
    file: tokio::fs::File,
    head: Vec<u8>,
    tail: VecDeque<u8>,
    total_bytes: u64,
    total_lines: u64,
}

impl std::fmt::Debug for OutputSpool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OutputSpool")
            .field("artifact", &self.artifact)
            .field("total_bytes", &self.total_bytes)
            .finish_non_exhaustive()
    }
}

impl OutputSpool {
    /// Creates the artifact file at `<artifacts_dir>/<session-id>/<call-id>.log` (CODEBASE.md
    /// §6), truncating if it somehow already exists (a tool call id is only ever run once).
    pub async fn create(
        artifacts_dir: &std::path::Path,
        session_id: SessionId,
        call_id: ToolCallId,
        limits: SpoolLimits,
    ) -> Result<Self, std::io::Error> {
        let dir = artifacts_dir.join(session_id.as_uuid().to_string());
        tokio::fs::create_dir_all(&dir).await?;
        let path = dir.join(format!("{}.log", call_id.as_str()));
        let file = tokio::fs::File::create(&path).await?;
        Ok(Self {
            limits,
            artifact: ArtifactRef {
                session_id,
                call_id,
                path,
            },
            file,
            head: Vec::new(),
            tail: VecDeque::new(),
            total_bytes: 0,
            total_lines: 0,
        })
    }

    /// The artifact this spool writes to (available before [`Self::finish`] for tools that want
    /// to reference it early, e.g. to show a "streaming to <path>" status line).
    pub fn artifact(&self) -> &ArtifactRef {
        &self.artifact
    }

    /// Streams `chunk` to the artifact file and updates the head/tail windows. Never grows RAM
    /// usage past `head_bytes + tail_bytes` regardless of how many chunks are fed (INV-7).
    pub async fn write_chunk(&mut self, chunk: &[u8]) -> Result<(), std::io::Error> {
        self.file.write_all(chunk).await?;
        self.total_bytes += chunk.len() as u64;
        self.total_lines += bytecount_newlines(chunk);

        if self.head.len() < self.limits.head_bytes {
            let take = (self.limits.head_bytes - self.head.len()).min(chunk.len());
            self.head.extend_from_slice(&chunk[..take]);
        }

        for &byte in chunk {
            if self.tail.len() >= self.limits.tail_bytes {
                self.tail.pop_front();
            }
            self.tail.push_back(byte);
        }
        Ok(())
    }

    /// Flushes the artifact file and returns the final summary.
    pub async fn finish(mut self) -> Result<SpooledOutput, std::io::Error> {
        self.file.flush().await?;
        let window_bytes = (self.head.len() + self.tail.len()) as u64;
        let truncated = self.total_bytes > window_bytes;
        Ok(SpooledOutput {
            head: self.head,
            tail: self.tail.into_iter().collect(),
            total_bytes: self.total_bytes,
            total_lines: self.total_lines,
            truncated,
            artifact: self.artifact,
        })
    }
}

fn bytecount_newlines(chunk: &[u8]) -> u64 {
    chunk.iter().filter(|&&b| b == b'\n').count() as u64
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use pretty_assertions::assert_eq;

    use super::*;

    #[tokio::test]
    async fn small_output_is_not_truncated() {
        let dir = tempfile::tempdir().unwrap();
        let mut spool = OutputSpool::create(
            dir.path(),
            SessionId::new(),
            ToolCallId::new("call-1"),
            SpoolLimits {
                head_bytes: 1024,
                tail_bytes: 1024,
            },
        )
        .await
        .unwrap();
        spool.write_chunk(b"hello\nworld\n").await.unwrap();
        let summary = spool.finish().await.unwrap();
        assert!(!summary.truncated);
        assert_eq!(summary.total_bytes, 12);
        assert_eq!(summary.total_lines, 2);
        assert_eq!(summary.head_text(), "hello\nworld\n");
        assert_eq!(summary.tail_text(), "hello\nworld\n");
    }

    #[tokio::test]
    async fn large_output_keeps_ram_bounded_and_marks_truncated() {
        let dir = tempfile::tempdir().unwrap();
        let mut spool = OutputSpool::create(
            dir.path(),
            SessionId::new(),
            ToolCallId::new("call-2"),
            SpoolLimits {
                head_bytes: 4,
                tail_bytes: 4,
            },
        )
        .await
        .unwrap();
        for chunk in [b"aaaa".as_slice(), b"bbbb", b"cccc", b"dddd"] {
            spool.write_chunk(chunk).await.unwrap();
        }
        let summary = spool.finish().await.unwrap();
        assert!(summary.truncated);
        assert_eq!(summary.total_bytes, 16);
        assert_eq!(summary.head, b"aaaa");
        assert_eq!(summary.tail, b"dddd");
    }

    #[tokio::test]
    async fn artifact_file_contains_the_full_output() {
        let dir = tempfile::tempdir().unwrap();
        let mut spool = OutputSpool::create(
            dir.path(),
            SessionId::new(),
            ToolCallId::new("call-3"),
            SpoolLimits {
                head_bytes: 2,
                tail_bytes: 2,
            },
        )
        .await
        .unwrap();
        spool.write_chunk(b"0123456789").await.unwrap();
        let summary = spool.finish().await.unwrap();
        let on_disk = tokio::fs::read_to_string(&summary.artifact.path)
            .await
            .unwrap();
        assert_eq!(on_disk, "0123456789");
    }
}
