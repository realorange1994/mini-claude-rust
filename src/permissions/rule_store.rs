//! Rule store - Store and query permission rules

use std::sync::Mutex;

use super::auto_strip::is_dangerous_allow_rule;
use super::rule_parser::ParsedRule;

/// Entry for fast tool-name-based lookup
#[derive(Clone)]
pub struct RuleEntry {
    pub source: String,
    pub behavior: String,
}

/// Stores permission rules by source and behavior.
/// Rules can be queried by tool name or content.
pub struct RuleStore {
    inner: Mutex<RuleStoreInner>,
}

impl std::fmt::Debug for RuleStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RuleStore").finish_non_exhaustive()
    }
}

struct RuleStoreInner {
    /// key: "source|behavior" (e.g., "userSettings|deny")
    rules: Vec<(String, Vec<ParsedRule>)>,
    /// tool name → list of entries for fast lookup
    index_by_tool: Vec<(String, Vec<RuleEntry>)>,
}

impl RuleStore {
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(RuleStoreInner {
                rules: Vec::new(),
                index_by_tool: Vec::new(),
            }),
        }
    }

    /// Add rules from a source with a given behavior
    pub fn add_rules(&self, source: &str, behavior: &str, rules: &[ParsedRule]) {
        let key = format!("{}|{}", source, behavior);
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());

        // Add to rules list
        let existing = inner.rules.iter_mut().find(|(k, _)| k == &key);
        if let Some((_, rule_list)) = existing {
            rule_list.extend_from_slice(rules);
        } else {
            inner.rules.push((key, rules.to_vec()));
        }

        // Update index
        for tool_name in rules.iter().map(|r| &r.tool_name) {
            let existing_idx = inner.index_by_tool.iter().position(|(t, _)| t == tool_name);
            let entry = RuleEntry {
                source: source.to_string(),
                behavior: behavior.to_string(),
            };
            if let Some(idx) = existing_idx {
                inner.index_by_tool[idx].1.push(entry);
            } else {
                inner.index_by_tool.push((tool_name.clone(), vec![entry]));
            }
        }
    }

    /// Returns true if there's a tool-level deny rule for this tool
    pub fn has_deny_rule(&self, tool_name: &str) -> bool {
        let inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        inner
            .rules
            .iter()
            .filter(|(key, _)| key.ends_with("|deny"))
            .any(|(_, rules)| {
                rules
                    .iter()
                    .any(|r| r.tool_matches(tool_name) && r.is_tool_level())
            })
    }

    /// Returns true if there's a tool-level ask rule for this tool
    pub fn has_ask_rule(&self, tool_name: &str) -> bool {
        let inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        inner
            .rules
            .iter()
            .filter(|(key, _)| key.ends_with("|ask"))
            .any(|(_, rules)| {
                rules
                    .iter()
                    .any(|r| r.tool_matches(tool_name) && r.is_tool_level())
            })
    }

    /// Returns true if there's a tool-level allow rule for this tool
    pub fn has_allow_rule(&self, tool_name: &str) -> bool {
        let inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        inner
            .rules
            .iter()
            .filter(|(key, _)| key.ends_with("|allow"))
            .any(|(_, rules)| {
                rules
                    .iter()
                    .any(|r| r.tool_matches(tool_name) && r.is_tool_level())
            })
    }

    /// Find a content-specific rule matching tool + content + behavior
    pub fn find_content_rule(
        &self,
        tool_name: &str,
        content: &str,
        behavior: &str,
    ) -> Option<ParsedRule> {
        let inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        for (key, rules) in &inner.rules {
            if !key.ends_with(&format!("|{}", behavior)) {
                continue;
            }
            for rule in rules {
                if rule.tool_matches(tool_name)
                    && !rule.is_tool_level()
                    && rule.content_matches(content)
                {
                    return Some(rule.clone());
                }
            }
        }
        None
    }

    /// Get all rules with the given behavior
    pub fn get_all_rules(&self, behavior: &str) -> Vec<ParsedRule> {
        let inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        inner
            .rules
            .iter()
            .filter(|(key, _)| key.ends_with(&format!("|{}", behavior)))
            .flat_map(|(_, rules)| rules.iter().cloned())
            .collect()
    }

    /// Get all rules applicable to a tool (tool-level only)
    pub fn get_rules_for_tool(&self, tool_name: &str) -> Vec<ParsedRule> {
        let inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        inner
            .rules
            .iter()
            .flat_map(|(_, rules)| rules.clone())
            .filter(|r| r.is_tool_level() && r.tool_matches(tool_name))
            .collect()
    }

    /// Strip dangerous allow rules and return them as a stash.
    /// Returns Vec of (key, stripped_rules) tuples.
    pub fn strip_dangerous_allow_rules(&self) -> Vec<(String, Vec<ParsedRule>)> {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let mut stash = Vec::new();

        for (key, rules) in &mut inner.rules {
            let parts: Vec<&str> = key.splitn(2, '|').collect();
            if parts.len() != 2 || parts[1] != "allow" {
                continue;
            }

            let mut kept = Vec::new();
            let mut stripped = Vec::new();
            for rule in rules.iter() {
                if is_dangerous_allow_rule(rule) {
                    stripped.push(rule.clone());
                } else {
                    kept.push(rule.clone());
                }
            }

            if !stripped.is_empty() {
                stash.push((key.clone(), stripped));
                *rules = kept;
            }
        }

        // Rebuild index from scratch (avoiding borrow conflicts)
        let mut new_index: Vec<(String, Vec<RuleEntry>)> = Vec::new();
        for (key, rules) in &inner.rules {
            let parts: Vec<&str> = key.splitn(2, '|').collect();
            let behavior = parts.get(1).copied().unwrap_or("");
            for rule in rules {
                if rule.is_tool_level() {
                    if let Some((_, entries)) = new_index.iter_mut().find(|(t, _)| t == &rule.tool_name) {
                        entries.push(RuleEntry {
                            source: parts.get(0).copied().unwrap_or("").to_string(),
                            behavior: behavior.to_string(),
                        });
                    } else {
                        new_index.push((rule.tool_name.clone(), vec![RuleEntry {
                            source: parts.get(0).copied().unwrap_or("").to_string(),
                            behavior: behavior.to_string(),
                        }]));
                    }
                }
            }
        }
        inner.index_by_tool = new_index;

        stash
    }

    /// Restore stripped rules back into the store
    pub fn restore_stripped_rules(&self, stash: Vec<(String, Vec<ParsedRule>)>) {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());

        for (key, rules) in stash {
            let existing = inner.rules.iter_mut().find(|(k, _)| k == &key);
            if let Some((_, rule_list)) = existing {
                rule_list.extend_from_slice(&rules);
            } else {
                inner.rules.push((key.clone(), rules.clone()));
            }

            // Rebuild index for restored rules
            for rule in &rules {
                if rule.is_tool_level() {
                    let parts: Vec<&str> = key.splitn(2, '|').collect();
                    let behavior = parts.get(1).copied().unwrap_or("");
                    let source = parts.get(0).copied().unwrap_or("");
                    if let Some((_, entries)) = inner.index_by_tool.iter_mut().find(|(t, _)| t == &rule.tool_name) {
                        entries.push(RuleEntry {
                            source: source.to_string(),
                            behavior: behavior.to_string(),
                        });
                    } else {
                        inner.index_by_tool.push((rule.tool_name.clone(), vec![RuleEntry {
                            source: source.to_string(),
                            behavior: behavior.to_string(),
                        }]));
                    }
                }
            }
        }
    }
}
