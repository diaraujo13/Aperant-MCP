"""Tests for the providers/ abstraction (Phase 6e)."""

from __future__ import annotations

import asyncio
import os
import sys
from pathlib import Path
from unittest.mock import AsyncMock, MagicMock, patch

import pytest

sys.path.insert(0, str(Path(__file__).parent.parent / "apps" / "backend"))

from providers import (  # noqa: E402
    ProviderRequest,
    get_active_provider,
    reset_provider_cache,
)
from providers.codex import (  # noqa: E402
    CodexProvider,
    CodexRateLimitError,
    CodexSpawnError,
)


@pytest.fixture(autouse=True)
def _reset_cache():
    reset_provider_cache()
    yield
    reset_provider_cache()


def test_get_active_provider_default_is_anthropic(monkeypatch):
    monkeypatch.delenv("AUTO_CLAUDE_PROVIDER", raising=False)
    monkeypatch.delenv("APERANT_AI_PROVIDER", raising=False)
    provider = get_active_provider()
    assert provider.name == "anthropic"


def test_get_active_provider_codex_via_aperant_var(monkeypatch):
    """The pre-existing core/client.py dispatch reads APERANT_AI_PROVIDER.
    Profile env injection emits both vars; either should select Codex."""
    monkeypatch.delenv("AUTO_CLAUDE_PROVIDER", raising=False)
    monkeypatch.setenv("APERANT_AI_PROVIDER", "openai")
    provider = get_active_provider()
    assert provider.name == "codex"


def test_get_active_provider_codex_when_env_set(monkeypatch):
    monkeypatch.setenv("AUTO_CLAUDE_PROVIDER", "codex")
    provider = get_active_provider()
    assert provider.name == "codex"


def test_get_active_provider_codex_case_insensitive(monkeypatch):
    monkeypatch.setenv("AUTO_CLAUDE_PROVIDER", "Codex")
    provider = get_active_provider()
    assert provider.name == "codex"


def test_codex_resolve_binary_explicit(monkeypatch):
    monkeypatch.setenv("AUTO_CLAUDE_CODEX_BINARY", "/foo/bar/codex")
    provider = CodexProvider()
    assert provider._resolve_binary() == "/foo/bar/codex"


def test_codex_resolve_binary_missing_raises(monkeypatch):
    monkeypatch.delenv("AUTO_CLAUDE_CODEX_BINARY", raising=False)
    with patch("providers.codex.shutil.which", return_value=None):
        provider = CodexProvider()
        with pytest.raises(CodexSpawnError, match="codex binary not found"):
            provider._resolve_binary()


def test_codex_resolve_model_from_request(monkeypatch):
    monkeypatch.delenv("AUTO_CLAUDE_CODEX_MODEL", raising=False)
    provider = CodexProvider()
    assert provider._resolve_model("gpt-5.3-codex") == "gpt-5.3-codex"


def test_codex_resolve_model_from_env(monkeypatch):
    monkeypatch.setenv("AUTO_CLAUDE_CODEX_MODEL", "gpt-5.4")
    provider = CodexProvider()
    assert provider._resolve_model(None) == "gpt-5.4"


def test_codex_resolve_model_none(monkeypatch):
    monkeypatch.delenv("AUTO_CLAUDE_CODEX_MODEL", raising=False)
    provider = CodexProvider()
    assert provider._resolve_model(None) is None


def _make_proc(stdout: bytes, stderr: bytes, returncode: int):
    proc = MagicMock()
    proc.communicate = AsyncMock(return_value=(stdout, stderr))
    proc.returncode = returncode
    return proc


def test_codex_call_emits_rate_limit_signal_and_raises(
    monkeypatch, tmp_path, capsys
):
    monkeypatch.setenv("AUTO_CLAUDE_CODEX_BINARY", "/usr/bin/codex")
    monkeypatch.delenv("AUTO_CLAUDE_CODEX_MODEL", raising=False)

    proc = _make_proc(b"", b"Error: rate_limit_exceeded - try later", 1)

    async def run():
        with patch(
            "providers.codex.asyncio.create_subprocess_exec",
            new=AsyncMock(return_value=proc),
        ):
            provider = CodexProvider()
            req = ProviderRequest(
                system="sys", user="hi", cwd=str(tmp_path)
            )
            with pytest.raises(CodexRateLimitError):
                await provider.call(req)

    asyncio.run(run())
    captured = capsys.readouterr()
    assert "rate_limit_exceeded" in captured.out


def test_codex_call_happy_path(monkeypatch, tmp_path):
    monkeypatch.setenv("AUTO_CLAUDE_CODEX_BINARY", "/usr/bin/codex")
    monkeypatch.delenv("AUTO_CLAUDE_CODEX_MODEL", raising=False)

    final_text = "final assistant message"

    # Pre-populate the temp file that codex would write.
    captured_args: dict = {}

    async def fake_create_subprocess_exec(*args, **kwargs):
        # Find --output-last-message path and write into it.
        args_list = list(args)
        idx = args_list.index("--output-last-message")
        out_path = args_list[idx + 1]
        Path(out_path).write_text(final_text, encoding="utf-8")
        captured_args["args"] = args_list
        return _make_proc(
            b'{"type":"event","usage":{"input_tokens":10,"output_tokens":5}}\n',
            b"",
            0,
        )

    async def run():
        with patch(
            "providers.codex.asyncio.create_subprocess_exec",
            new=fake_create_subprocess_exec,
        ):
            provider = CodexProvider()
            req = ProviderRequest(
                system="sys", user="hi", cwd=str(tmp_path), model="gpt-5.3-codex"
            )
            return await provider.call(req)

    result = asyncio.run(run())
    assert result.text == final_text
    assert result.usage == {"input_tokens": 10, "output_tokens": 5}
    # Verify -m was passed through
    assert "-m" in captured_args["args"]
    assert "gpt-5.3-codex" in captured_args["args"]


def test_codex_call_nonzero_exit_raises(monkeypatch, tmp_path):
    monkeypatch.setenv("AUTO_CLAUDE_CODEX_BINARY", "/usr/bin/codex")
    monkeypatch.delenv("AUTO_CLAUDE_CODEX_MODEL", raising=False)

    proc = _make_proc(b"", b"some unrelated error", 2)

    async def run():
        with patch(
            "providers.codex.asyncio.create_subprocess_exec",
            new=AsyncMock(return_value=proc),
        ):
            provider = CodexProvider()
            req = ProviderRequest(system="sys", user="hi", cwd=str(tmp_path))
            with pytest.raises(CodexSpawnError, match="exited 2"):
                await provider.call(req)

    asyncio.run(run())
