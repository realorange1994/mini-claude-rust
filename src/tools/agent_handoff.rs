/// HandoffClassification represents the result of classifying sub-agent output
/// before returning it to the parent agent.
#[derive(Debug, Clone)]
pub struct HandoffClassification {
    pub safe: bool,
    pub reason: Option<String>,
    pub filtered: Option<String>,
}

/// Secret pattern prefixes commonly found in leaked credentials.
const SECRET_PATTERNS: &[&str] = &[
    "sk-ant-api03-",   // Anthropic API key prefix
    "sk-ant-tool03-",  // Anthropic tool key prefix
    "sk-proj-",        // OpenAI project key
    "AKIA",            // AWS access key prefix
    "ghp_",            // GitHub personal access token
    "gho_",            // GitHub OAuth token
    "ghs_",            // GitHub server-to-server token
    "xoxb-",           // Slack bot token
    "xoxp-",           // Slack user token
    "-----BEGIN PRIVATE KEY-----",
    "-----BEGIN RSA PRIVATE KEY-----",
];

/// ClassifyHandoff reviews sub-agent output before returning to parent.
/// This is a lightweight check — not a full permission classification.
/// It detects potential secrets/credentials and excessively long outputs.
pub fn classify_handoff(output: &str) -> HandoffClassification {
    // Secret pattern detection
    for &pattern in SECRET_PATTERNS {
        if output.contains(pattern) {
            return HandoffClassification {
                safe: false,
                reason: Some(format!(
                    "output contains potential secret/credential pattern: {}",
                    pattern
                )),
                filtered: Some("[REDACTED: output contained potential secrets]".to_string()),
            };
        }
    }

    // Length check: if output > 50000 chars, suggest truncation
    if output.len() > 50000 {
        return HandoffClassification {
            safe: true,
            reason: Some("output very long, consider truncation".to_string()),
            filtered: None,
        };
    }

    HandoffClassification {
        safe: true,
        reason: None,
        filtered: None,
    }
}

/// SanitizeHandoffOutput returns the output if safe, or the filtered version
/// if the handoff classification detected issues.
pub fn sanitize_handoff_output(output: &str) -> (String, bool) {
    let class = classify_handoff(output);
    if !class.safe {
        (class.filtered.unwrap_or_default(), false)
    } else {
        (output.to_string(), true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_safe_output() {
        let result = classify_handoff("Hello, world!");
        assert!(result.safe);
        assert!(result.reason.is_none());
    }

    #[test]
    fn test_detect_anthropic_key() {
        let result = classify_handoff("sk-ant-api03-abc123");
        assert!(!result.safe);
        assert!(result.reason.unwrap().contains("sk-ant-api03-"));
    }

    #[test]
    fn test_detect_aws_key() {
        let result = classify_handoff("AKIAIOSFODNN7EXAMPLE");
        assert!(!result.safe);
    }

    #[test]
    fn test_detect_github_token() {
        let result = classify_handoff("ghp_ABCDEFGHIJKLMNOP");
        assert!(!result.safe);
    }

    #[test]
    fn test_detect_private_key() {
        let result = classify_handoff("-----BEGIN PRIVATE KEY-----\nMIIEvgIBADANBgkq");
        assert!(!result.safe);
    }

    #[test]
    fn test_long_output() {
        let long_output = "x".repeat(60000);
        let result = classify_handoff(&long_output);
        assert!(result.safe);
        assert!(result.reason.unwrap().contains("truncation"));
    }

    #[test]
    fn test_sanitize_safe() {
        let (output, safe) = sanitize_handoff_output("Hello");
        assert_eq!(output, "Hello");
        assert!(safe);
    }

    #[test]
    fn test_sanitize_unsafe() {
        let (output, safe) = sanitize_handoff_output("sk-ant-api03-abc");
        assert!(!safe);
        assert!(output.contains("REDACTED"));
    }
}
