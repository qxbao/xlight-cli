// SPDX-License-Identifier: GPL-3.0-only

//! Transcript view (docs/PLAN.md §18.2): the scrollable message history for the active agent.
//! Populated from `UiEvent::TextDelta`/`ReasoningDelta`/`ToolCallStarted`/`ToolCallFinished`
//! (coalescing deltas per PATTERNS.md §3). Rendering is Wave B.

/// One rendered line of the transcript (Wave B fills in richer formatting — role, timestamps,
/// tool-call summaries — this is just the shape the view accumulates into).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TranscriptLine {
    pub text: String,
}

#[derive(Debug, Clone, Default)]
pub struct TranscriptView {
    pub lines: Vec<TranscriptLine>,
    pub scroll_offset: u16,
}

impl TranscriptView {
    pub fn new() -> Self {
        Self::default()
    }

    /// Appends a line (Wave B: coalesce trailing `TextDelta`s into the last line instead of
    /// always pushing a new one).
    pub fn push_line(&mut self, text: impl Into<String>) {
        self.lines.push(TranscriptLine { text: text.into() });
    }
}
