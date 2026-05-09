//! Rate-limit detection for agent stdout/stderr lines.

const PATTERNS: &[&str] = &[
    "rate_limit_error",
    "rate limit exceeded",
    "429",
    "overloaded_error",
    "quota exceeded",
    "out of credits",
    "usage limit reached",
    // OpenAI / Codex (Phase 6d)
    "insufficient_quota",
    "rate_limit_exceeded",
    "you exceeded your current quota",
    "tokens per min",
    "requests per min",
];

pub fn detect_rate_limit(line: &str) -> bool {
    let lower = line.to_lowercase();
    PATTERNS.iter().any(|p| lower.contains(p))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_rate_limit_error() {
        assert!(detect_rate_limit(
            r#"{"type":"rate_limit_error","message":"slow down"}"#
        ));
    }

    #[test]
    fn detects_rate_limit_exceeded_case_insensitive() {
        assert!(detect_rate_limit("ERROR: Rate Limit Exceeded for org"));
    }

    #[test]
    fn detects_429() {
        assert!(detect_rate_limit("HTTP 429 Too Many Requests"));
    }

    #[test]
    fn detects_overloaded() {
        assert!(detect_rate_limit("API returned overloaded_error"));
    }

    #[test]
    fn detects_quota_exceeded() {
        assert!(detect_rate_limit("Quota Exceeded — try again later"));
    }

    #[test]
    fn detects_out_of_credits() {
        assert!(detect_rate_limit("You are out of credits"));
    }

    #[test]
    fn detects_usage_limit_reached() {
        assert!(detect_rate_limit("Usage limit reached for this account"));
    }

    #[test]
    fn negative_normal_line() {
        assert!(!detect_rate_limit("[planner] generating subtasks"));
    }

    #[test]
    fn negative_empty() {
        assert!(!detect_rate_limit(""));
    }

    #[test]
    fn negative_unrelated_number() {
        assert!(!detect_rate_limit("processed 42 of 100"));
    }

    // ── OpenAI / Codex (Phase 6d) ─────────────────────────────────────────────

    #[test]
    fn detects_openai_insufficient_quota() {
        assert!(detect_rate_limit(
            r#"{"error":{"code":"insufficient_quota"}}"#
        ));
    }

    #[test]
    fn detects_openai_rate_limit_exceeded_code() {
        assert!(detect_rate_limit(
            r#"{"error":{"code":"rate_limit_exceeded"}}"#
        ));
    }

    #[test]
    fn detects_openai_quota_message() {
        assert!(detect_rate_limit(
            "You exceeded your current quota, please check your plan"
        ));
    }

    #[test]
    fn detects_openai_tokens_per_min() {
        assert!(detect_rate_limit(
            "Limit: 30000 tokens per min (TPM) reached"
        ));
    }

    #[test]
    fn detects_openai_requests_per_min() {
        assert!(detect_rate_limit("Limit: 500 requests per min (RPM)"));
    }

    #[test]
    fn negative_openai_throughput_unrelated() {
        assert!(!detect_rate_limit(
            "processing throughput is 1200 tokens per second"
        ));
    }
}
