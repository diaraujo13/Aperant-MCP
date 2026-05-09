"""LLM provider abstraction. Anthropic via claude-agent-sdk is the default;
OpenAI Codex CLI is a fallback used when an Anthropic profile is rate-limited
and rotation falls through to a Codex profile (see Rust agent::profile_env).

Provider selection is driven by the AUTO_CLAUDE_PROVIDER env var, set by Rust
at spawn time. Default "anthropic". Code paths that need a one-shot LLM call
should route through `get_active_provider()` rather than instantiating clients
directly.

SDK features lost in Codex mode (deferred to Phase 6f):
- PreToolUse / PostToolUse hooks (Codex has no equivalent — security validation
  must move to a wrapper layer).
- Fine-grained Bash command allowlist (apps/backend/security/) — Codex's
  `--sandbox workspace-write` is the closest analog.
- MCP servers (graphiti memory etc.). Codex CLI supports MCP via
  ~/.codex/config.toml but we don't wire it in 6e.
- Streaming token telemetry parity — best-effort via JSON tail parsing.
"""
import os
from typing import Protocol

from .types import ProviderRequest, ProviderResult


class Provider(Protocol):
    """Minimal protocol shared by all LLM providers."""

    name: str

    async def call(self, request: ProviderRequest) -> ProviderResult: ...


_provider_cache: Provider | None = None


def get_active_provider() -> Provider:
    global _provider_cache
    if _provider_cache is not None:
        return _provider_cache
    kind = os.environ.get("AUTO_CLAUDE_PROVIDER", "anthropic").strip().lower()
    if kind == "codex":
        from .codex import CodexProvider

        _provider_cache = CodexProvider()
    else:
        from .anthropic import AnthropicProvider

        _provider_cache = AnthropicProvider()
    return _provider_cache


def reset_provider_cache() -> None:
    """Test hook — resets the cached provider so next call re-reads env."""
    global _provider_cache
    _provider_cache = None


__all__ = [
    "Provider",
    "ProviderRequest",
    "ProviderResult",
    "get_active_provider",
    "reset_provider_cache",
]
