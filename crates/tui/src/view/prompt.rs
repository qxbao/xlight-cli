// SPDX-License-Identifier: GPL-3.0-only

//! Prompt view (docs/PLAN.md §18.2): the user's input line, `@file` mention autocomplete
//! (`core.mention`), and `/command` autocomplete. Rendering + autocomplete matching are Wave B.

#[derive(Debug, Clone, Default)]
pub struct PromptView {
    pub buffer: String,
    pub cursor: usize,
}

impl PromptView {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn clear(&mut self) {
        self.buffer.clear();
        self.cursor = 0;
    }
}
