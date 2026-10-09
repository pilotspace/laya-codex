//! Serving settings that belong to the model: how much of each candidate it reads (the state
//! window, in tokens) and how many candidates it scores for a prompt and for a search.
//!
//! A model is trained and calibrated at one window, and its speed decides how many candidates fit
//! the time budget, so the model directory says how to serve it, in a `serving` block of its
//! `rl_agent_config.json`:
//!
//! ```json
//! "serving": {"state_tokens": 128, "score_top": 12, "search_score_top": 12}
//! ```
//!
//! A model without the block (laya-code-r1 and older) is served as before: 128 tokens, 16
//! candidates per prompt, and the same 16 per search (where the 400 ms lookup budget fits 8 of
//! them). `LAYA_CODEX_STATE_TOKENS` and `LAYA_CODEX_SCORE_TOP` override the model either way; the
//! score-top override applies to prompts and searches alike.

use std::path::Path;

use serde::Deserialize;

/// State window when neither the model nor the environment names one.
pub const DEFAULT_STATE_TOKENS: usize = 128;

/// The `serving` block of a model directory; a missing field falls back like a missing block.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
pub struct ModelServing {
    /// Tokens of each candidate the model reads (the window it was calibrated at).
    pub state_tokens: Option<usize>,
    /// Candidates scored for a prompt (the hook), best first; `0` = all of them.
    pub score_top: Option<usize>,
    /// Candidates scored for a search without a session (the MCP `search` tool); `0` = all.
    pub search_score_top: Option<usize>,
}

/// `LAYA_CODEX_STATE_TOKENS` and `LAYA_CODEX_SCORE_TOP`, when set to a number.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct EnvServing {
    pub state_tokens: Option<usize>,
    pub score_top: Option<usize>,
}

impl EnvServing {
    pub fn from_env() -> Self {
        let num = |k: &str| std::env::var(k).ok()?.trim().parse().ok();
        EnvServing {
            state_tokens: num("LAYA_CODEX_STATE_TOKENS"),
            score_top: num("LAYA_CODEX_SCORE_TOP"),
        }
    }
}

/// The settings the daemon serves with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Serving {
    pub state_tokens: usize,
    pub score_top: usize,
    pub search_score_top: usize,
}

/// The `serving` block of `<dir>/rl_agent_config.json`: `Ok(None)` when the model has none,
/// `Err` when the file cannot be read or the block is malformed.
pub fn read_model_serving(dir: &Path) -> Result<Option<ModelServing>, String> {
    let path = dir.join("rl_agent_config.json");
    let text = std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    parse_model_serving(&text).map_err(|e| format!("{}: {e}", path.display()))
}

/// The `serving` block of an `rl_agent_config.json` document (see [`read_model_serving`]).
pub fn parse_model_serving(text: &str) -> Result<Option<ModelServing>, String> {
    #[derive(Deserialize)]
    struct Doc {
        #[serde(default)]
        serving: Option<ModelServing>,
    }
    let doc: Doc = serde_json::from_str(text).map_err(|e| format!("serving block: {e}"))?;
    if doc.serving.and_then(|s| s.state_tokens) == Some(0) {
        return Err("serving block: state_tokens must be at least 1".into());
    }
    Ok(doc.serving)
}

/// Settings from the model's block, then the defaults (`default_score_top` is the retriever's),
/// with the environment over both.
pub fn resolve(model: Option<ModelServing>, env: EnvServing, default_score_top: usize) -> Serving {
    let model = model.unwrap_or_default();
    let score_top = env
        .score_top
        .or(model.score_top)
        .unwrap_or(default_score_top);
    Serving {
        state_tokens: env
            .state_tokens
            .or(model.state_tokens)
            .unwrap_or(DEFAULT_STATE_TOKENS),
        score_top,
        search_score_top: env
            .score_top
            .or(model.search_score_top)
            .unwrap_or(score_top),
    }
}

/// Environment overrides that differ from what the model's block asks for, one line each
/// (`LAYA_CODEX_STATE_TOKENS=256 overrides the model's 128`); empty when they agree or either
/// side is silent.
pub fn env_conflicts(model: Option<ModelServing>, env: EnvServing) -> Vec<String> {
    let Some(model) = model else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let mut check = |name: &str, env: Option<usize>, model: Option<usize>| {
        if let (Some(e), Some(m)) = (env, model)
            && e != m
        {
            out.push(format!("{name}={e} overrides the model's {m}"));
        }
    };
    check(
        "LAYA_CODEX_STATE_TOKENS",
        env.state_tokens,
        model.state_tokens,
    );
    check("LAYA_CODEX_SCORE_TOP", env.score_top, model.score_top);
    if env.score_top.is_some() && model.search_score_top != model.score_top {
        check(
            "LAYA_CODEX_SCORE_TOP",
            env.score_top,
            model.search_score_top,
        );
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const R2: &str = r#"{"max_len": 704, "temperature": [1.0, 1.0, 0.84],
        "serving": {"state_tokens": 128, "score_top": 12, "search_score_top": 12}}"#;
    const R1: &str = r#"{"max_len": 512, "temperature": [1.6, 1.25, 0.81]}"#;

    fn block(w: usize, k: usize, ks: usize) -> ModelServing {
        ModelServing {
            state_tokens: Some(w),
            score_top: Some(k),
            search_score_top: Some(ks),
        }
    }

    #[test]
    fn the_block_is_read_from_the_model_config() {
        assert_eq!(parse_model_serving(R2), Ok(Some(block(128, 12, 12))));
    }

    #[test]
    fn a_model_without_the_block_has_none() {
        assert_eq!(parse_model_serving(R1), Ok(None));
    }

    #[test]
    fn a_malformed_block_is_an_error_not_a_guess() {
        assert!(parse_model_serving(r#"{"serving": {"score_top": "twelve"}}"#).is_err());
        assert!(parse_model_serving(r#"{"serving": {"state_tokens": 0}}"#).is_err());
        assert!(parse_model_serving("not json").is_err());
    }

    #[test]
    fn a_partial_block_leaves_the_rest_to_the_defaults() {
        let m = parse_model_serving(r#"{"serving": {"score_top": 12}}"#).unwrap();
        let s = resolve(m, EnvServing::default(), 16);
        assert_eq!(
            (s.state_tokens, s.score_top, s.search_score_top),
            (128, 12, 12)
        );
    }

    #[test]
    fn without_a_block_today_s_settings_apply() {
        // r1 keeps behaving exactly as before: 128 tokens, 16 per prompt and 16 per search
        // (the search's 400 ms budget is what cuts that to 8 for r1).
        let s = resolve(None, EnvServing::default(), 16);
        assert_eq!(
            s,
            Serving {
                state_tokens: 128,
                score_top: 16,
                search_score_top: 16
            }
        );
    }

    #[test]
    fn the_model_s_block_replaces_the_defaults() {
        let s = resolve(Some(block(128, 12, 10)), EnvServing::default(), 16);
        assert_eq!(
            s,
            Serving {
                state_tokens: 128,
                score_top: 12,
                search_score_top: 10
            }
        );
    }

    #[test]
    fn the_environment_overrides_the_model_for_prompts_and_searches() {
        let env = EnvServing {
            state_tokens: Some(256),
            score_top: Some(24),
        };
        let s = resolve(Some(block(128, 12, 10)), env, 16);
        assert_eq!(
            s,
            Serving {
                state_tokens: 256,
                score_top: 24,
                search_score_top: 24
            }
        );
    }

    #[test]
    fn conflicts_name_each_override_that_differs_from_the_model() {
        let env = EnvServing {
            state_tokens: Some(256),
            score_top: Some(12),
        };
        assert_eq!(
            env_conflicts(Some(block(128, 12, 12)), env),
            vec!["LAYA_CODEX_STATE_TOKENS=256 overrides the model's 128".to_string()]
        );
        // Agreeing overrides, no block, or no overrides: nothing to report.
        let same = EnvServing {
            state_tokens: Some(128),
            score_top: Some(12),
        };
        assert!(env_conflicts(Some(block(128, 12, 12)), same).is_empty());
        assert!(env_conflicts(None, env).is_empty());
        assert!(env_conflicts(Some(block(128, 12, 12)), EnvServing::default()).is_empty());
    }

    #[test]
    fn the_model_dir_file_is_read() {
        let d = std::env::temp_dir().join(format!("laya-serving-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("rl_agent_config.json"), R2).unwrap();
        assert_eq!(read_model_serving(&d), Ok(Some(block(128, 12, 12))));
        let missing = read_model_serving(&d.join("nowhere")).unwrap_err();
        assert!(missing.contains("rl_agent_config.json"), "{missing}");
        let _ = std::fs::remove_dir_all(&d);
    }
}
