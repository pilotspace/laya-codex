//! Question rendering and input-sequence construction.
//!
//! Exact port of `render_options` / `build_sequence` from the reference `rl_common.py`:
//!
//! ```text
//! [CLS] "<type> question: <instructions>" [SEP] ([MASK] " <option>")* [SEP] <state> [SEP]
//! ```
//!
//! The head (`question + options`) is budgeted to `head_max_len` tokens, the whole sequence to
//! `max_len`; the state is truncated (from the right) to whatever room is left.

use std::path::Path;

use tokenizers::Tokenizer;

use crate::config::{AgentConfig, ModelFiles};
use crate::error::{ModelError, Result};

/// Max tokens of one option text (excluding its `[MASK]` marker), as in the reference.
const OPTION_TOKENS: usize = 48;
/// Minimum head budget that must remain for the instructions before options are shrunk.
const MIN_OPT_BUDGET: isize = 16;
/// Minimum instruction tokens kept.
const MIN_HEAD_TOKENS: isize = 8;
/// Minimum option tokens (incl. marker) after shrinking.
const MIN_OPT_TOKENS: isize = 4;
/// The mask token text; occurrences in user text are blanked so they cannot forge markers.
const MASK_TEXT: &str = "[MASK]";

/// Question type. The discriminant is the `qtype` index used for `type_emb` and temperatures.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(usize)]
pub enum QType {
    /// Pick one of `k` labelled options.
    Choice = 0,
    /// Ordinal levels (`level i: ...`).
    Score = 1,
    /// Yes/no statement (`false` / `true`; `P(true)` is `probs[1]`).
    Noul = 2,
}

impl QType {
    /// Name used in the rendered sequence and the temperature bucket key.
    pub fn name(self) -> &'static str {
        match self {
            QType::Choice => "choice",
            QType::Score => "score",
            QType::Noul => "noul",
        }
    }
}

/// A typed question with its rendered option texts (label-index order).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Question {
    /// Type of question.
    pub qtype: QType,
    /// Instructions text (`ins` in the reference).
    pub instructions: String,
    /// Rendered option texts, exactly as `render_options` produces them.
    pub options: Vec<String>,
}

impl Question {
    /// A `noul` question with the default option texts.
    pub fn noul(instructions: &str) -> Self {
        Self::noul_with(instructions, None, None)
    }

    /// A `noul` question with custom criteria for the `false` / `true` options.
    pub fn noul_with(
        instructions: &str,
        false_crit: Option<&str>,
        true_crit: Option<&str>,
    ) -> Self {
        Self {
            qtype: QType::Noul,
            instructions: instructions.to_string(),
            options: vec![
                format!(
                    "false: {}",
                    false_crit.unwrap_or("no, the statement does not hold")
                ),
                format!("true: {}", true_crit.unwrap_or("yes, the statement holds")),
            ],
        }
    }

    /// A `choice` question; each criterion is `(key, Some(description))` or `(key, None)`.
    pub fn choice(instructions: &str, criteria: &[(&str, Option<&str>)]) -> Self {
        Self {
            qtype: QType::Choice,
            instructions: instructions.to_string(),
            options: criteria
                .iter()
                .map(|(k, v)| match v {
                    Some(v) => format!("{k}: {v}"),
                    None => (*k).to_string(),
                })
                .collect(),
        }
    }

    /// A `score` question with ordinal level descriptions.
    pub fn score(instructions: &str, levels: &[&str]) -> Self {
        Self {
            qtype: QType::Score,
            instructions: instructions.to_string(),
            options: levels
                .iter()
                .enumerate()
                .map(|(i, c)| format!("level {i}: {c}"))
                .collect(),
        }
    }
}

/// Token ids and marker positions of one model input.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuiltSequence {
    /// Token ids (`<= max_len`).
    pub ids: Vec<u32>,
    /// Position of the `[MASK]` marker of each option, in option order.
    pub markers: Vec<usize>,
}

/// Tokenizer plus the budgets needed to build model inputs.
pub struct SequenceBuilder {
    tok: Tokenizer,
    cls_id: u32,
    sep_id: u32,
    mask_id: u32,
    pad_id: u32,
    max_len: usize,
    head_max_len: usize,
}

impl std::fmt::Debug for SequenceBuilder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SequenceBuilder")
            .field("max_len", &self.max_len)
            .field("head_max_len", &self.head_max_len)
            .finish_non_exhaustive()
    }
}

impl SequenceBuilder {
    /// Load `tokenizer/tokenizer.json` and the budgets from `rl_agent_config.json` in `dir`.
    pub fn from_dir(dir: &Path) -> Result<Self> {
        let files = ModelFiles::resolve(dir)?;
        let agent = AgentConfig::load(&files.agent_config)?;
        Self::from_files(&files.tokenizer, &agent)
    }

    /// Build from an explicit tokenizer file and agent config.
    pub fn from_files(tokenizer_path: &Path, agent: &AgentConfig) -> Result<Self> {
        let tok = Tokenizer::from_file(tokenizer_path)
            .map_err(|e| ModelError::Tokenizer(format!("{}: {e}", tokenizer_path.display())))?;
        let special = |name: &str| {
            tok.token_to_id(name)
                .ok_or_else(|| ModelError::Tokenizer(format!("tokenizer has no {name} token")))
        };
        Ok(Self {
            cls_id: special("[CLS]")?,
            sep_id: special("[SEP]")?,
            mask_id: special(MASK_TEXT)?,
            pad_id: special("[PAD]")?,
            tok,
            max_len: agent.max_len,
            head_max_len: agent.head_max_len,
        })
    }

    /// Total sequence budget.
    pub fn max_len(&self) -> usize {
        self.max_len
    }

    /// Question + options budget.
    pub fn head_max_len(&self) -> usize {
        self.head_max_len
    }

    /// Padding token id.
    pub fn pad_id(&self) -> u32 {
        self.pad_id
    }

    /// Tokenize without special tokens (`tok(text, add_special_tokens=False)`).
    pub fn encode(&self, text: &str) -> Result<Vec<u32>> {
        let enc = self.tok.encode(text, false)?;
        Ok(enc.get_ids().to_vec())
    }

    /// Tokenize a state string (mask text blanked) and optionally cap it at `max_tokens`
    /// (keeping the first tokens, like the reference's right truncation).
    pub fn encode_state(&self, state: &str, max_tokens: Option<usize>) -> Result<Vec<u32>> {
        let mut ids = self.encode(&state.replace(MASK_TEXT, " "))?;
        if let Some(n) = max_tokens {
            ids.truncate(n);
        }
        Ok(ids)
    }

    /// Exact port of `build_sequence(tok, state, q, max_len, head_max_len)` (right truncation).
    pub fn build(&self, state: &str, q: &Question) -> Result<BuiltSequence> {
        let state_ids = self.encode_state(state, None)?;
        self.build_from_state_ids(&state_ids, q)
    }

    /// Like [`build`](Self::build) but with an already tokenized state (`encode_state`).
    pub fn build_from_state_ids(&self, state_ids: &[u32], q: &Question) -> Result<BuiltSequence> {
        let head_max_len = self.head_max_len as isize;
        let ins = q.instructions.replace(MASK_TEXT, " ");
        let mut head_ids = self.encode(&format!("{} question: {}", q.qtype.name(), ins))?;

        let mut opt_ids: Vec<Vec<u32>> = Vec::with_capacity(q.options.len());
        for opt in &q.options {
            let mut o = self.encode(&format!(" {}", opt.replace(MASK_TEXT, " ")))?;
            o.truncate(OPTION_TOKENS);
            o.insert(0, self.mask_id);
            opt_ids.push(o);
        }
        let opt_len = |opts: &[Vec<u32>]| opts.iter().map(|o| o.len() as isize).sum::<isize>();
        let mut opt_budget = head_max_len - opt_len(&opt_ids);
        if opt_budget < MIN_OPT_BUDGET {
            // Too many / too long options: shrink every option text evenly.
            let per = MIN_OPT_TOKENS
                .max((head_max_len - MIN_OPT_BUDGET) / (opt_ids.len().max(1) as isize));
            for o in &mut opt_ids {
                o.truncate(per as usize);
            }
            opt_budget = head_max_len - opt_len(&opt_ids);
        }
        head_ids.truncate(MIN_HEAD_TOKENS.max(opt_budget) as usize);

        let mut ids = Vec::with_capacity(self.max_len);
        ids.push(self.cls_id);
        ids.extend_from_slice(&head_ids);
        ids.push(self.sep_id);
        let mut markers = Vec::with_capacity(opt_ids.len());
        for o in &opt_ids {
            markers.push(ids.len());
            ids.extend_from_slice(o);
        }
        ids.push(self.sep_id);
        let room = (self.max_len as isize - ids.len() as isize - 1).max(0) as usize;
        ids.extend_from_slice(&state_ids[..state_ids.len().min(room)]);
        ids.push(self.sep_id);
        ids.truncate(self.max_len);
        markers.retain(|&m| m < self.max_len);
        Ok(BuiltSequence { ids, markers })
    }

    /// [`build_from_state_ids`](Self::build_from_state_ids) that fails when any option marker
    /// was truncated away (the reference API raises in that case).
    pub fn build_checked(&self, state_ids: &[u32], q: &Question) -> Result<BuiltSequence> {
        let seq = self.build_from_state_ids(state_ids, q)?;
        if seq.markers.len() != q.options.len() {
            return Err(ModelError::OptionsDoNotFit(format!(
                "{} options, {} markers fit (head_max_len={})",
                q.options.len(),
                seq.markers.len(),
                self.head_max_len
            )));
        }
        Ok(seq)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_options_matches_reference() {
        let q = Question::noul("x");
        assert_eq!(
            q.options,
            [
                "false: no, the statement does not hold",
                "true: yes, the statement holds"
            ]
        );
        let q = Question::noul_with("x", Some("nope"), None);
        assert_eq!(q.options[0], "false: nope");
        let q = Question::choice("x", &[("a", None), ("b", Some("bee"))]);
        assert_eq!(q.options, ["a", "b: bee"]);
        let q = Question::score("x", &["low", "high"]);
        assert_eq!(q.options, ["level 0: low", "level 1: high"]);
    }
}
