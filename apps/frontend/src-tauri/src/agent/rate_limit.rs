//! Rate-limit detection for agent stdout/stderr lines.

const PATTERNS: &[&str] = &[
    "rate_limit_error",
    "rate limit exceeded",
    "429",
    "overloaded_error",
    "quota exceeded",
    "out of credits",
    "usage limit reached",
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
}
