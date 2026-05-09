//! Resolves the active Claude profile into a set of env vars to inject
//! into the Python agent subprocess. Supports auto-switching by accepting
//! an exclude list of profile ids that have already been tried.

use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProfileKind {
    Api,
    OAuth,
    Codex,
}

#[derive(Debug, Clone)]
pub struct ResolvedProfile {
    pub profile_id: String,
    pub profile_kind: ProfileKind,
    pub env: Vec<(String, String)>,
}

pub trait ProfileSource {
    fn api_profiles(&self) -> Value;
    fn oauth_profiles(&self) -> Value;
}

struct DefaultProfileSource;

impl ProfileSource for DefaultProfileSource {
    fn api_profiles(&self) -> Value {
        crate::api::profiles::read_api_profiles()
    }
    fn oauth_profiles(&self) -> Value {
        crate::api::profiles::read_profiles()
    }
}

pub fn resolve_profile_env(exclude: &[String]) -> Option<ResolvedProfile> {
    resolve_with(&DefaultProfileSource, exclude)
}

fn profile_kind_from_value(p: &Value) -> &'static str {
    match p.get("kind").and_then(|v| v.as_str()) {
        Some("codex") => "codex",
        _ => "anthropic",
    }
}

fn build_api_env(p: &Value) -> Option<(String, Vec<(String, String)>)> {
    let id = p.get("id").and_then(|v| v.as_str())?.to_string();
    if id.is_empty() {
        return None;
    }
    let base_url = p.get("baseUrl").and_then(|v| v.as_str()).unwrap_or("");
    let api_key = p.get("apiKey").and_then(|v| v.as_str()).unwrap_or("");
    if base_url.is_empty() || api_key.is_empty() {
        return None;
    }
    let mut env = vec![
        ("ANTHROPIC_BASE_URL".to_string(), base_url.to_string()),
        ("ANTHROPIC_AUTH_TOKEN".to_string(), api_key.to_string()),
    ];
    if let Some(model) = p.get("model").and_then(|v| v.as_str()) {
        if !model.is_empty() {
            env.push(("ANTHROPIC_MODEL".to_string(), model.to_string()));
        }
    }
    Some((id, env))
}

fn build_oauth_env(p: &Value) -> Option<(String, Vec<(String, String)>)> {
    let id = p.get("id").and_then(|v| v.as_str())?.to_string();
    if id.is_empty() {
        return None;
    }
    let token = p.get("oauthToken").and_then(|v| v.as_str()).unwrap_or("");
    if token.is_empty() {
        return None;
    }
    Some((
        id,
        vec![("CLAUDE_CODE_OAUTH_TOKEN".to_string(), token.to_string())],
    ))
}

fn build_codex_env(p: &Value) -> Option<(String, Vec<(String, String)>)> {
    let id = p.get("id").and_then(|v| v.as_str())?.to_string();
    if id.is_empty() {
        return None;
    }
    let api_key = p.get("apiKey").and_then(|v| v.as_str()).unwrap_or("");
    if api_key.is_empty() {
        return None;
    }
    let mut env = vec![
        ("OPENAI_API_KEY".to_string(), api_key.to_string()),
        ("AUTO_CLAUDE_PROVIDER".to_string(), "codex".to_string()),
    ];
    if let Some(model) = p.get("model").and_then(|v| v.as_str()) {
        if !model.is_empty() {
            env.push(("AUTO_CLAUDE_CODEX_MODEL".to_string(), model.to_string()));
        }
    }
    if let Some(binary) = p.get("binary").and_then(|v| v.as_str()) {
        if !binary.is_empty() {
            env.push(("AUTO_CLAUDE_CODEX_BINARY".to_string(), binary.to_string()));
        }
    }
    Some((id, env))
}

pub(crate) fn resolve_with<S: ProfileSource>(
    src: &S,
    exclude: &[String],
) -> Option<ResolvedProfile> {
    let api_store = src.api_profiles();
    let oauth_store = src.oauth_profiles();

    let api_list: Vec<Value> = api_store
        .get("profiles")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    let oauth_list: Vec<Value> = oauth_store
        .get("profiles")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();

    let api_active = api_store
        .get("activeProfileId")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(String::from);
    let oauth_active = oauth_store
        .get("activeProfileId")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(String::from);

    let active_profile = api_active.as_ref().and_then(|active_id| {
        api_list
            .iter()
            .find(|p| p.get("id").and_then(|v| v.as_str()) == Some(active_id))
    });
    let active_is_codex = active_profile
        .map(|p| profile_kind_from_value(p) == "codex")
        .unwrap_or(false);

    // 1. Active API profile (only if NOT codex)
    if let (Some(active_id), Some(p)) = (&api_active, active_profile) {
        if !active_is_codex && !exclude.iter().any(|e| e == active_id) {
            if let Some((id, env)) = build_api_env(p) {
                return Some(ResolvedProfile {
                    profile_id: id,
                    profile_kind: ProfileKind::Api,
                    env,
                });
            }
        }
    }

    // 2. Active OAuth profile
    if let Some(active_id) = &oauth_active {
        if !exclude.iter().any(|e| e == active_id) {
            if let Some(p) = oauth_list
                .iter()
                .find(|p| p.get("id").and_then(|v| v.as_str()) == Some(active_id))
            {
                if let Some((id, env)) = build_oauth_env(p) {
                    return Some(ResolvedProfile {
                        profile_id: id,
                        profile_kind: ProfileKind::OAuth,
                        env,
                    });
                }
            }
        }
    }

    // 3. First non-excluded non-Codex API profile in order
    for p in &api_list {
        if profile_kind_from_value(p) == "codex" {
            continue;
        }
        if let Some((id, env)) = build_api_env(p) {
            if !exclude.iter().any(|e| e == &id) {
                return Some(ResolvedProfile {
                    profile_id: id,
                    profile_kind: ProfileKind::Api,
                    env,
                });
            }
        }
    }

    // 4. First non-excluded OAuth profile in order
    for p in &oauth_list {
        if let Some((id, env)) = build_oauth_env(p) {
            if !exclude.iter().any(|e| e == &id) {
                return Some(ResolvedProfile {
                    profile_id: id,
                    profile_kind: ProfileKind::OAuth,
                    env,
                });
            }
        }
    }

    // 5. Active API profile if it's Codex (so a user with Codex set as active
    //    still gets it picked first within the Codex tier).
    if let (Some(active_id), Some(p)) = (&api_active, active_profile) {
        if active_is_codex && !exclude.iter().any(|e| e == active_id) {
            if let Some((id, env)) = build_codex_env(p) {
                return Some(ResolvedProfile {
                    profile_id: id,
                    profile_kind: ProfileKind::Codex,
                    env,
                });
            }
        }
    }

    // 6. First non-excluded Codex API profile in order
    for p in &api_list {
        if profile_kind_from_value(p) != "codex" {
            continue;
        }
        if let Some((id, env)) = build_codex_env(p) {
            if !exclude.iter().any(|e| e == &id) {
                return Some(ResolvedProfile {
                    profile_id: id,
                    profile_kind: ProfileKind::Codex,
                    env,
                });
            }
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    struct StaticSource {
        api: Value,
        oauth: Value,
    }
    impl ProfileSource for StaticSource {
        fn api_profiles(&self) -> Value {
            self.api.clone()
        }
        fn oauth_profiles(&self) -> Value {
            self.oauth.clone()
        }
    }

    fn empty_oauth() -> Value {
        json!({ "profiles": [], "activeProfileId": "" })
    }
    fn empty_api() -> Value {
        json!({ "profiles": [], "activeProfileId": null, "version": 1 })
    }

    #[test]
    fn picks_active_api_first() {
        let src = StaticSource {
            api: json!({
                "profiles": [
                    { "id": "a1", "baseUrl": "https://api.example.com", "apiKey": "k1", "model": "claude-x" }
                ],
                "activeProfileId": "a1",
                "version": 1
            }),
            oauth: json!({
                "profiles": [{ "id": "o1", "oauthToken": "tok" }],
                "activeProfileId": "o1"
            }),
        };
        let r = resolve_with(&src, &[]).unwrap();
        assert_eq!(r.profile_id, "a1");
        assert_eq!(r.profile_kind, ProfileKind::Api);
        assert!(r
            .env
            .iter()
            .any(|(k, v)| k == "ANTHROPIC_BASE_URL" && v == "https://api.example.com"));
        assert!(r
            .env
            .iter()
            .any(|(k, v)| k == "ANTHROPIC_AUTH_TOKEN" && v == "k1"));
        assert!(r
            .env
            .iter()
            .any(|(k, v)| k == "ANTHROPIC_MODEL" && v == "claude-x"));
    }

    #[test]
    fn excludes_active_falls_back_to_oauth() {
        let src = StaticSource {
            api: json!({
                "profiles": [
                    { "id": "a1", "baseUrl": "https://x", "apiKey": "k1" }
                ],
                "activeProfileId": "a1"
            }),
            oauth: json!({
                "profiles": [{ "id": "o1", "oauthToken": "tok" }],
                "activeProfileId": "o1"
            }),
        };
        let r = resolve_with(&src, &["a1".to_string()]).unwrap();
        assert_eq!(r.profile_id, "o1");
        assert_eq!(r.profile_kind, ProfileKind::OAuth);
        assert_eq!(
            r.env,
            vec![("CLAUDE_CODE_OAUTH_TOKEN".to_string(), "tok".to_string())]
        );
    }

    #[test]
    fn skips_api_missing_fields_uses_next() {
        let src = StaticSource {
            api: json!({
                "profiles": [
                    { "id": "a1", "baseUrl": "", "apiKey": "k1" },
                    { "id": "a2", "baseUrl": "https://y", "apiKey": "k2" }
                ],
                "activeProfileId": null
            }),
            oauth: empty_oauth(),
        };
        let r = resolve_with(&src, &[]).unwrap();
        assert_eq!(r.profile_id, "a2");
    }

    #[test]
    fn no_model_omits_anthropic_model() {
        let src = StaticSource {
            api: json!({
                "profiles": [{ "id": "a1", "baseUrl": "https://x", "apiKey": "k" }],
                "activeProfileId": "a1"
            }),
            oauth: empty_oauth(),
        };
        let r = resolve_with(&src, &[]).unwrap();
        assert!(!r.env.iter().any(|(k, _)| k == "ANTHROPIC_MODEL"));
    }

    #[test]
    fn empty_model_omits_anthropic_model() {
        let src = StaticSource {
            api: json!({
                "profiles": [{ "id": "a1", "baseUrl": "https://x", "apiKey": "k", "model": "" }],
                "activeProfileId": "a1"
            }),
            oauth: empty_oauth(),
        };
        let r = resolve_with(&src, &[]).unwrap();
        assert!(!r.env.iter().any(|(k, _)| k == "ANTHROPIC_MODEL"));
    }

    #[test]
    fn all_exhausted_returns_none() {
        let src = StaticSource {
            api: json!({
                "profiles": [{ "id": "a1", "baseUrl": "https://x", "apiKey": "k" }],
                "activeProfileId": "a1"
            }),
            oauth: json!({
                "profiles": [{ "id": "o1", "oauthToken": "tok" }],
                "activeProfileId": "o1"
            }),
        };
        let r = resolve_with(&src, &["a1".to_string(), "o1".to_string()]);
        assert!(r.is_none());
    }

    #[test]
    fn no_profiles_returns_none() {
        let src = StaticSource {
            api: empty_api(),
            oauth: empty_oauth(),
        };
        assert!(resolve_with(&src, &[]).is_none());
    }

    #[test]
    fn oauth_active_no_api() {
        let src = StaticSource {
            api: empty_api(),
            oauth: json!({
                "profiles": [{ "id": "o1", "oauthToken": "tok" }],
                "activeProfileId": "o1"
            }),
        };
        let r = resolve_with(&src, &[]).unwrap();
        assert_eq!(r.profile_id, "o1");
        assert_eq!(r.profile_kind, ProfileKind::OAuth);
    }

    #[test]
    fn skips_oauth_missing_token() {
        let src = StaticSource {
            api: empty_api(),
            oauth: json!({
                "profiles": [
                    { "id": "o1", "oauthToken": "" },
                    { "id": "o2", "oauthToken": "tok2" }
                ],
                "activeProfileId": "o1"
            }),
        };
        let r = resolve_with(&src, &[]).unwrap();
        assert_eq!(r.profile_id, "o2");
    }

    // ── Codex (Phase 6d) ──────────────────────────────────────────────────────

    #[test]
    fn resolve_picks_active_codex() {
        let src = StaticSource {
            api: json!({
                "profiles": [
                    { "id": "cx1", "kind": "codex", "apiKey": "sk-openai", "model": "gpt-5" }
                ],
                "activeProfileId": "cx1"
            }),
            oauth: empty_oauth(),
        };
        let r = resolve_with(&src, &[]).unwrap();
        assert_eq!(r.profile_id, "cx1");
        assert_eq!(r.profile_kind, ProfileKind::Codex);
        assert!(r
            .env
            .iter()
            .any(|(k, v)| k == "OPENAI_API_KEY" && v == "sk-openai"));
        assert!(r
            .env
            .iter()
            .any(|(k, v)| k == "AUTO_CLAUDE_CODEX_MODEL" && v == "gpt-5"));
    }

    #[test]
    fn resolve_picks_anthropic_before_codex() {
        let src = StaticSource {
            api: json!({
                "profiles": [
                    { "id": "a1", "baseUrl": "https://x", "apiKey": "k1" },
                    { "id": "cx1", "kind": "codex", "apiKey": "sk-openai" }
                ],
                "activeProfileId": "a1"
            }),
            oauth: empty_oauth(),
        };
        let r = resolve_with(&src, &[]).unwrap();
        assert_eq!(r.profile_id, "a1");
        assert_eq!(r.profile_kind, ProfileKind::Api);
    }

    #[test]
    fn resolve_falls_back_to_codex_when_anthropic_excluded() {
        let src = StaticSource {
            api: json!({
                "profiles": [
                    { "id": "a1", "baseUrl": "https://x", "apiKey": "k1" },
                    { "id": "cx1", "kind": "codex", "apiKey": "sk-openai" }
                ],
                "activeProfileId": "a1"
            }),
            oauth: empty_oauth(),
        };
        let r = resolve_with(&src, &["a1".to_string()]).unwrap();
        assert_eq!(r.profile_id, "cx1");
        assert_eq!(r.profile_kind, ProfileKind::Codex);
    }

    #[test]
    fn resolve_codex_skipped_when_apikey_missing() {
        let src = StaticSource {
            api: json!({
                "profiles": [
                    { "id": "cx1", "kind": "codex", "apiKey": "" },
                    { "id": "cx2", "kind": "codex", "apiKey": "sk-ok" }
                ],
                "activeProfileId": null
            }),
            oauth: empty_oauth(),
        };
        let r = resolve_with(&src, &[]).unwrap();
        assert_eq!(r.profile_id, "cx2");
    }

    #[test]
    fn resolve_codex_env_includes_provider_sentinel() {
        let src = StaticSource {
            api: json!({
                "profiles": [
                    { "id": "cx1", "kind": "codex", "apiKey": "sk-ok" }
                ],
                "activeProfileId": "cx1"
            }),
            oauth: empty_oauth(),
        };
        let r = resolve_with(&src, &[]).unwrap();
        assert!(r
            .env
            .iter()
            .any(|(k, v)| k == "AUTO_CLAUDE_PROVIDER" && v == "codex"));
    }

    #[test]
    fn resolve_codex_with_binary_override() {
        let src = StaticSource {
            api: json!({
                "profiles": [
                    { "id": "cx1", "kind": "codex", "apiKey": "sk-ok", "binary": "/usr/local/bin/codex" }
                ],
                "activeProfileId": "cx1"
            }),
            oauth: empty_oauth(),
        };
        let r = resolve_with(&src, &[]).unwrap();
        assert!(r
            .env
            .iter()
            .any(|(k, v)| k == "AUTO_CLAUDE_CODEX_BINARY" && v == "/usr/local/bin/codex"));
    }
}
