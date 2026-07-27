use crate::llm::{ChatContent, ChatMessage};

const IMAGE_TOKEN_ESTIMATE: usize = 765;
const RESERVED_RATIO: f32 = 0.1;
const MIN_RESERVED_TOKENS: usize = 4096;

/// Context-pressure ladder (FR-004 "三层阈值 soft / snip / compact / force").
///
/// Each level names the cheapest action that still relieves pressure, so the
/// agent escalates instead of jumping straight to an expensive LLM summary:
///   Healthy -> nothing
///   Soft    -> warn only (diagnostics); cheap, no context mutation
///   Snip    -> prune stale tool results in place (no LLM call)
///   Compact -> LLM summarisation of the visible history
///   Force   -> hard evict oldest turns; last resort before a provider reject
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
pub enum OverflowLevel {
    #[default]
    Healthy,
    Soft,
    Snip,
    Compact,
    Force,
}

impl OverflowLevel {
    pub fn label(&self) -> &'static str {
        match self {
            OverflowLevel::Healthy => "healthy",
            OverflowLevel::Soft => "soft",
            OverflowLevel::Snip => "snip",
            OverflowLevel::Compact => "compact",
            OverflowLevel::Force => "force",
        }
    }
}

/// Fractions of the context window at which each level engages.
#[derive(Debug, Clone, Copy)]
pub struct OverflowRatios {
    pub soft: f32,
    pub snip: f32,
    pub compact: f32,
    pub force: f32,
}

impl Default for OverflowRatios {
    fn default() -> Self {
        Self {
            soft: 0.5,
            snip: 0.6,
            compact: 0.8,
            force: 0.9,
        }
    }
}

impl OverflowRatios {
    /// Clamp to (0,1] and enforce monotonicity, so a hand-edited config can
    /// never produce an unreachable or inverted ladder.
    pub fn sanitized(self) -> Self {
        let clamp = |v: f32, fallback: f32| {
            if v.is_finite() && v > 0.0 && v <= 1.0 {
                v
            } else {
                fallback
            }
        };
        let d = OverflowRatios::default();
        let soft = clamp(self.soft, d.soft);
        let snip = clamp(self.snip, d.snip).max(soft);
        let compact = clamp(self.compact, d.compact).max(snip);
        let force = clamp(self.force, d.force).max(compact);
        Self {
            soft,
            snip,
            compact,
            force,
        }
    }
}

pub struct OverflowCheck {
    pub context_window: Option<usize>,
    pub reserved_tokens: usize,
    pub trim_at_ratio: f32,
    pub ratios: OverflowRatios,
}

impl OverflowCheck {
    pub fn new(
        context_window: Option<usize>,
        trim_at_ratio: f32,
        reserved_tokens: Option<usize>,
    ) -> Self {
        let reserved_tokens = reserved_tokens.unwrap_or_else(|| {
            context_window
                .map(|w| ((w as f32 * RESERVED_RATIO) as usize).max(MIN_RESERVED_TOKENS))
                .unwrap_or(MIN_RESERVED_TOKENS)
        });
        Self {
            context_window,
            reserved_tokens,
            trim_at_ratio,
            ratios: OverflowRatios::default(),
        }
    }

    pub fn with_ratios(mut self, ratios: OverflowRatios) -> Self {
        self.ratios = ratios.sanitized();
        self
    }

    /// Classify the current pressure level for a token count.
    pub fn level(&self, tokens: usize) -> OverflowLevel {
        let Some(window) = self.context_window else {
            return OverflowLevel::Healthy;
        };
        let window = window as f32;
        let t = tokens as f32;
        if t >= window * self.ratios.force {
            OverflowLevel::Force
        } else if t >= window * self.ratios.compact {
            OverflowLevel::Compact
        } else if t >= window * self.ratios.snip {
            OverflowLevel::Snip
        } else if t >= window * self.ratios.soft {
            OverflowLevel::Soft
        } else {
            OverflowLevel::Healthy
        }
    }

    pub fn is_enabled(&self) -> bool {
        self.context_window.is_some()
    }

    #[allow(dead_code)]
    pub fn usable_tokens(&self) -> Option<usize> {
        self.context_window
            .map(|w| w.saturating_sub(self.reserved_tokens))
    }

    pub fn threshold(&self) -> Option<usize> {
        self.context_window
            .map(|w| (w as f32 * self.trim_at_ratio).max(1.0) as usize)
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub fn check_tokens(&self, tokens: usize) -> bool {
        let Some(threshold) = self.threshold() else {
            return false;
        };
        tokens >= threshold
    }

    #[allow(dead_code)]
    pub fn check_estimate(&self, messages: &[ChatMessage]) -> bool {
        let Some(threshold) = self.threshold() else {
            return false;
        };
        estimate_messages_tokens(messages) >= threshold
    }
}

#[allow(dead_code)]
pub fn estimate_messages_tokens(messages: &[ChatMessage]) -> usize {
    let tokens: usize = messages.iter().map(message_tokens).sum();
    tokens.max(1)
}

pub fn estimate_tokens(text: &str) -> usize {
    crate::token_estimate::estimate_tokens(text).max(1)
}

fn text_tokens(text: &str) -> usize {
    crate::token_estimate::estimate_tokens(text)
}

#[allow(dead_code)]
fn message_tokens(msg: &ChatMessage) -> usize {
    let role_tokens = text_tokens(&msg.role);
    let content_tokens = match &msg.content {
        Some(ChatContent::Text(s)) => text_tokens(s),
        Some(ChatContent::Parts(parts)) => parts
            .iter()
            .map(|p| match p {
                crate::llm::ChatContentPart::Text { text } => text_tokens(text),
                crate::llm::ChatContentPart::ImageUrl { .. } => IMAGE_TOKEN_ESTIMATE,
            })
            .sum(),
        None => 0,
    };
    let tool_tokens = msg
        .tool_calls
        .as_ref()
        .map(|calls| {
            calls
                .iter()
                .map(|c| text_tokens(&c.function.name) + text_tokens(&c.function.arguments))
                .sum::<usize>()
        })
        .unwrap_or(0);
    role_tokens + content_tokens + tool_tokens
}

#[cfg(test)]
mod ladder_tests {
    use super::*;

    fn check(window: usize) -> OverflowCheck {
        OverflowCheck::new(Some(window), 0.9, None)
    }

    #[test]
    fn ladder_levels_engage_in_order() {
        let c = check(1000);
        assert_eq!(c.level(0), OverflowLevel::Healthy);
        assert_eq!(c.level(499), OverflowLevel::Healthy);
        assert_eq!(c.level(500), OverflowLevel::Soft);
        assert_eq!(c.level(600), OverflowLevel::Snip);
        assert_eq!(c.level(800), OverflowLevel::Compact);
        assert_eq!(c.level(900), OverflowLevel::Force);
        assert_eq!(c.level(5000), OverflowLevel::Force);
    }

    #[test]
    fn no_window_means_healthy() {
        let c = OverflowCheck::new(None, 0.9, None);
        assert_eq!(c.level(1_000_000), OverflowLevel::Healthy);
    }

    #[test]
    fn broken_ratios_are_sanitized() {
        let r = OverflowRatios {
            soft: -1.0,
            snip: 0.2,
            compact: f32::NAN,
            force: 0.1,
        }
        .sanitized();
        assert!(r.soft > 0.0 && r.soft <= 1.0);
        assert!(r.snip >= r.soft);
        assert!(r.compact >= r.snip);
        assert!(r.force >= r.compact);
    }

    #[test]
    fn levels_are_ordered() {
        assert!(OverflowLevel::Healthy < OverflowLevel::Soft);
        assert!(OverflowLevel::Soft < OverflowLevel::Snip);
        assert!(OverflowLevel::Snip < OverflowLevel::Compact);
        assert!(OverflowLevel::Compact < OverflowLevel::Force);
    }
}
