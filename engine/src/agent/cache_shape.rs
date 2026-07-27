//! Prompt-prefix cache shape tracking (FR-004 "Cache shape").
//!
//! Providers only serve a prompt-cache hit when the *prefix* of the request
//! is byte-identical to the previous one. The prefix here is the system
//! prompt plus the tool definitions, so anything that perturbs either —
//! a persona edit, a lazily loaded tool, a reordered definition list,
//! injected wall-clock time — silently destroys the cache and re-bills the
//! whole context as fresh input tokens.
//!
//! This module fingerprints that prefix each turn, reports which component
//! changed, and exposes the result for diagnostics
//! (`display.show_cache_diagnostics`).

use crate::llm::ToolDefinition;
use sha2::{Digest, Sha256};

/// Fingerprint of the cacheable request prefix.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CacheShape {
    /// Hash of the system prompt (all leading system messages).
    pub system_hash: String,
    /// Hash of the tool definition list (names, descriptions, schemas).
    pub tools_hash: String,
    /// Number of tool definitions the request carried.
    pub tool_count: usize,
    /// Combined hash: what the provider actually keys its cache on.
    pub prefix_hash: String,
}

/// What changed between two consecutive turns' prefixes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShapeChange {
    /// First turn of the session; nothing to compare against.
    FirstTurn,
    /// Prefix is byte-identical: the provider cache can hit.
    Stable,
    /// The system prompt changed (persona/prompt edit, injected timestamp).
    SystemPromptChanged,
    /// The tool definition set changed (lazy load, plugin toggle, reorder).
    ToolsChanged { before: usize, after: usize },
    /// Both changed.
    BothChanged,
}

impl ShapeChange {
    /// Whether a provider prompt-cache hit is still possible this turn.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn cache_can_hit(&self) -> bool {
        matches!(self, ShapeChange::Stable)
    }

    /// One-line human-readable diagnostic.
    pub fn describe(&self) -> String {
        match self {
            ShapeChange::FirstTurn => "cache shape: first turn (no prior prefix)".to_string(),
            ShapeChange::Stable => "cache shape: stable (prefix cache can hit)".to_string(),
            ShapeChange::SystemPromptChanged => {
                "cache shape: MISS — system prompt changed".to_string()
            }
            ShapeChange::ToolsChanged { before, after } => {
                format!("cache shape: MISS — tool definitions changed ({before} -> {after} tools)")
            }
            ShapeChange::BothChanged => {
                "cache shape: MISS — system prompt and tool definitions changed".to_string()
            }
        }
    }
}

fn hash_hex(input: &[&str]) -> String {
    let mut hasher = Sha256::new();
    for part in input {
        // Length-prefix each part so concatenation cannot alias.
        hasher.update((part.len() as u64).to_le_bytes());
        hasher.update(part.as_bytes());
    }
    hex::encode(&hasher.finalize()[..16])
}

impl CacheShape {
    /// Capture the shape of a request prefix.
    ///
    /// `system_messages` must be the leading system messages in request
    /// order; `tools` the definition list exactly as sent (order matters to
    /// the provider, so it is hashed as-is rather than sorted).
    pub fn capture(system_messages: &[String], tools: &[ToolDefinition]) -> Self {
        let system_parts: Vec<&str> = system_messages.iter().map(String::as_str).collect();
        let system_hash = hash_hex(&system_parts);

        let mut tool_parts: Vec<String> = Vec::with_capacity(tools.len() * 3);
        for tool in tools {
            tool_parts.push(tool.function.name.clone());
            tool_parts.push(tool.function.description.clone());
            // The schema is serde_json::Value; serialize canonically so
            // equal schemas always hash equal.
            tool_parts.push(tool.function.parameters.to_string());
        }
        let tool_refs: Vec<&str> = tool_parts.iter().map(String::as_str).collect();
        let tools_hash = hash_hex(&tool_refs);
        let prefix_hash = hash_hex(&[system_hash.as_str(), tools_hash.as_str()]);

        Self {
            system_hash,
            tools_hash,
            tool_count: tools.len(),
            prefix_hash,
        }
    }

    /// Classify the difference against the previous turn's shape.
    pub fn compare(&self, previous: Option<&CacheShape>) -> ShapeChange {
        let Some(prev) = previous else {
            return ShapeChange::FirstTurn;
        };
        let system_changed = prev.system_hash != self.system_hash;
        let tools_changed = prev.tools_hash != self.tools_hash;
        match (system_changed, tools_changed) {
            (false, false) => ShapeChange::Stable,
            (true, false) => ShapeChange::SystemPromptChanged,
            (false, true) => ShapeChange::ToolsChanged {
                before: prev.tool_count,
                after: self.tool_count,
            },
            (true, true) => ShapeChange::BothChanged,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::{FunctionDefinition, ToolDefinition};
    use serde_json::json;

    fn tool(name: &str, desc: &str) -> ToolDefinition {
        ToolDefinition {
            kind: "function",
            function: FunctionDefinition {
                name: name.to_string(),
                description: desc.to_string(),
                parameters: json!({"type": "object", "properties": {}}),
            },
        }
    }

    #[test]
    fn identical_prefix_is_stable() {
        let sys = vec!["you are moss".to_string()];
        let tools = vec![tool("run_command", "run a command")];
        let a = CacheShape::capture(&sys, &tools);
        let b = CacheShape::capture(&sys, &tools);
        assert_eq!(a, b);
        assert_eq!(b.compare(Some(&a)), ShapeChange::Stable);
        assert!(b.compare(Some(&a)).cache_can_hit());
    }

    #[test]
    fn first_turn_has_no_baseline() {
        let shape = CacheShape::capture(&["x".to_string()], &[]);
        assert_eq!(shape.compare(None), ShapeChange::FirstTurn);
        assert!(!shape.compare(None).cache_can_hit());
    }

    #[test]
    fn system_prompt_change_is_detected() {
        let tools = vec![tool("read_file", "read")];
        let a = CacheShape::capture(&["persona A".to_string()], &tools);
        let b = CacheShape::capture(&["persona B".to_string()], &tools);
        assert_eq!(b.compare(Some(&a)), ShapeChange::SystemPromptChanged);
        assert!(!b.compare(Some(&a)).cache_can_hit());
    }

    #[test]
    fn tool_set_change_reports_counts() {
        let sys = vec!["s".to_string()];
        let a = CacheShape::capture(&sys, &[tool("a", "d")]);
        let b = CacheShape::capture(&sys, &[tool("a", "d"), tool("b", "d")]);
        assert_eq!(
            b.compare(Some(&a)),
            ShapeChange::ToolsChanged {
                before: 1,
                after: 2
            }
        );
    }

    #[test]
    fn tool_order_affects_the_hash() {
        // Providers cache on bytes, so a reordered tool list is a real miss.
        let sys = vec!["s".to_string()];
        let a = CacheShape::capture(&sys, &[tool("a", "d"), tool("b", "d")]);
        let b = CacheShape::capture(&sys, &[tool("b", "d"), tool("a", "d")]);
        assert_ne!(a.tools_hash, b.tools_hash);
    }

    #[test]
    fn schema_change_alone_is_detected() {
        let sys = vec!["s".to_string()];
        let mut t2 = tool("a", "d");
        t2.function.parameters = json!({"type": "object", "properties": {"x": {"type": "string"}}});
        let a = CacheShape::capture(&sys, &[tool("a", "d")]);
        let b = CacheShape::capture(&sys, &[t2]);
        assert_ne!(a.tools_hash, b.tools_hash);
        assert!(matches!(
            b.compare(Some(&a)),
            ShapeChange::ToolsChanged { .. }
        ));
    }

    #[test]
    fn concatenation_cannot_alias() {
        // ["ab","c"] must not hash like ["a","bc"].
        let x = CacheShape::capture(&["ab".to_string(), "c".to_string()], &[]);
        let y = CacheShape::capture(&["a".to_string(), "bc".to_string()], &[]);
        assert_ne!(x.system_hash, y.system_hash);
    }

    #[test]
    fn both_changed_is_reported() {
        let a = CacheShape::capture(&["s1".to_string()], &[tool("a", "d")]);
        let b = CacheShape::capture(&["s2".to_string()], &[tool("b", "d")]);
        assert_eq!(b.compare(Some(&a)), ShapeChange::BothChanged);
    }
}
