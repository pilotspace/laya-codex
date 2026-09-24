//! `laya_core::Scorer` implementation on top of [`LayaModel`].

use std::time::{Duration, Instant};

use laya_core::{Chunk, Scorer};

use crate::model::LayaModel;

/// Default question template; `{task}` is replaced by the task text.
pub const DEFAULT_QUESTION_TEMPLATE: &str =
    "Is this source code relevant to the software change: \"{task}\"?";
/// Default state token budget per chunk.
pub const DEFAULT_MAX_STATE_TOKENS: usize = 256;

/// Scores chunks with the `noul` question `question_template.replace("{task}", task)` over the
/// state `file: {path} (lines a-b)\n{text}` truncated to `max_state_tokens` tokens.
#[derive(Debug)]
pub struct LayaScorer {
    /// The loaded model.
    pub model: LayaModel,
    /// Question template containing `{task}`.
    pub question_template: String,
    /// Maximum state tokens per chunk (the reference truncates from the right too).
    pub max_state_tokens: usize,
    /// Optional wall-clock budget for one `score` call; `None` = unbounded.
    pub deadline: Option<Duration>,
}

impl LayaScorer {
    /// Wrap `model` with the default template and a 256-token state budget.
    pub fn new(model: LayaModel) -> Self {
        Self {
            model,
            question_template: DEFAULT_QUESTION_TEMPLATE.to_string(),
            max_state_tokens: DEFAULT_MAX_STATE_TOKENS,
            deadline: None,
        }
    }

    /// Set the per-call wall-clock budget (see [`LayaModel::noul_ids`]).
    pub fn with_deadline(mut self, deadline: Duration) -> Self {
        self.deadline = Some(deadline);
        self
    }

    /// Render a chunk as the model state string.
    pub fn render_state(chunk: &Chunk) -> String {
        format!(
            "file: {} (lines {}-{})\n{}",
            chunk.path, chunk.start_line, chunk.end_line, chunk.text
        )
    }

    /// The question asked for `task`.
    pub fn question(&self, task: &str) -> String {
        self.question_template.replace("{task}", task)
    }
}

impl Scorer for LayaScorer {
    fn score(&self, task: &str, chunks: &[&Chunk]) -> laya_core::Result<Vec<f32>> {
        if chunks.is_empty() {
            return Ok(Vec::new());
        }
        let deadline = self.deadline.map(|d| Instant::now() + d);
        let question = self.question(task);
        let seqs = self.model.sequences();
        let state_ids = chunks
            .iter()
            .map(|c| seqs.encode_state(&Self::render_state(c), Some(self.max_state_tokens)))
            .collect::<Result<Vec<_>, _>>()?;
        Ok(self.model.noul_ids(&question, &state_ids, deadline)?)
    }

    fn score_within(
        &self,
        task: &str,
        chunks: &[&Chunk],
        deadline: Instant,
    ) -> laya_core::Result<Vec<Option<f32>>> {
        if chunks.is_empty() {
            return Ok(Vec::new());
        }
        let question = self.question(task);
        let seqs = self.model.sequences();
        let state_ids = chunks
            .iter()
            .map(|c| seqs.encode_state(&Self::render_state(c), Some(self.max_state_tokens)))
            .collect::<Result<Vec<_>, _>>()?;
        Ok(self
            .model
            .noul_ids_within(&question, &state_ids, deadline)?)
    }
}
