"""Anthropic provider — one-shot adapter on top of claude-agent-sdk.

Uses the SDK's ClaudeSDKClient directly for single-turn calls. Heavyweight
session-style call sites (planner, coder, QA) continue to use
core.client.create_client(); this provider is for new code paths that need
a simple system+user → text exchange and want to be provider-agnostic.

NOTE: this preserves the "Anthropic via SDK only" rule — we go through
claude-agent-sdk, never anthropic.Anthropic() directly.
"""

from __future__ import annotations

import logging
from typing import Any

from .types import ProviderRequest, ProviderResult

logger = logging.getLogger(__name__)


class AnthropicProvider:
    name = "anthropic"

    async def call(self, request: ProviderRequest) -> ProviderResult:
        # Lazy-import the SDK so test environments without the SDK installed
        # can still import providers.anthropic for typing purposes.
        from claude_agent_sdk import ClaudeAgentOptions, ClaudeSDKClient
        from core.auth import configure_sdk_authentication, get_sdk_env_vars

        sdk_env = get_sdk_env_vars()
        configure_sdk_authentication(sdk_env.get("CLAUDE_CONFIG_DIR"))

        options_kwargs: dict[str, Any] = {
            "system_prompt": request.system,
            "cwd": request.cwd,
            "max_turns": 1,
            "env": sdk_env,
        }
        if request.model:
            options_kwargs["model"] = request.model

        text_chunks: list[str] = []
        usage: dict[str, int] | None = None
        raw: dict[str, Any] = {}

        client = ClaudeSDKClient(options=ClaudeAgentOptions(**options_kwargs))
        async with client:
            await client.query(request.user)
            async for message in client.receive_response():
                # Best-effort extraction — SDK message shape varies by version.
                content = getattr(message, "content", None)
                if isinstance(content, list):
                    for block in content:
                        chunk = getattr(block, "text", None)
                        if isinstance(chunk, str):
                            text_chunks.append(chunk)
                msg_usage = getattr(message, "usage", None)
                if isinstance(msg_usage, dict):
                    usage = msg_usage

        return ProviderResult(text="".join(text_chunks), raw=raw, usage=usage)
