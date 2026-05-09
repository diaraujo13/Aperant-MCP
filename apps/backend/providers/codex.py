"""OpenAI Codex CLI provider. Shells out to `codex exec` per phase.
No session continuity (one process per LLM turn). MCP and hooks parity
deferred to Phase 6f.
"""

from __future__ import annotations

import asyncio
import json
import logging
import os
import shutil
import tempfile
from pathlib import Path
from typing import Any

from .types import ProviderRequest, ProviderResult

logger = logging.getLogger(__name__)

CODEX_RATE_LIMIT_SUBSTRINGS = (
    "rate_limit_exceeded",
    "insufficient_quota",
    "you exceeded your current quota",
    "tokens per min",
    "requests per min",
    "429",
)


class CodexRateLimitError(RuntimeError):
    """Raised when Codex output indicates a rate-limit / quota event.
    Surfaced on stdout/stderr so Rust's rate_limit detector picks it up
    and triggers the auto-switch to the next profile."""


class CodexSpawnError(RuntimeError):
    pass


class CodexProvider:
    name = "codex"

    def _resolve_binary(self) -> str:
        explicit = os.environ.get("AUTO_CLAUDE_CODEX_BINARY", "").strip()
        if explicit:
            return explicit
        path_lookup = shutil.which("codex")
        if not path_lookup:
            raise CodexSpawnError(
                "codex binary not found on PATH and AUTO_CLAUDE_CODEX_BINARY is unset"
            )
        return path_lookup

    def _resolve_model(self, request_model: str | None) -> str | None:
        return (
            request_model
            or os.environ.get("AUTO_CLAUDE_CODEX_MODEL", "").strip()
            or None
        )

    async def call(self, request: ProviderRequest) -> ProviderResult:
        binary = self._resolve_binary()
        model = self._resolve_model(request.model)

        # Codex exec doesn't separate system/user — render as one prompt.
        full_prompt = (
            f"## Instructions\n\n{request.system}\n\n## Task\n\n{request.user}\n"
        )

        with tempfile.NamedTemporaryFile(
            mode="w", suffix=".last.txt", delete=False
        ) as out_f:
            out_path = out_f.name

        args = [
            binary,
            "exec",
            "--cd",
            request.cwd,
            "--sandbox",
            "workspace-write",
            "--skip-git-repo-check",
            "--json",
            "--output-last-message",
            out_path,
        ]
        if model:
            args.extend(["-m", model])
        args.append("-")  # read prompt from stdin

        env = os.environ.copy()

        proc = await asyncio.create_subprocess_exec(
            *args,
            stdin=asyncio.subprocess.PIPE,
            stdout=asyncio.subprocess.PIPE,
            stderr=asyncio.subprocess.PIPE,
            env=env,
        )

        stdout_b, stderr_b = await proc.communicate(full_prompt.encode("utf-8"))
        stdout = stdout_b.decode("utf-8", errors="replace")
        stderr = stderr_b.decode("utf-8", errors="replace")

        # Surface rate-limit lines on stdout so Rust's detector picks them up.
        combined_lower = (stdout + "\n" + stderr).lower()
        for pat in CODEX_RATE_LIMIT_SUBSTRINGS:
            if pat in combined_lower:
                print(pat, flush=True)
                try:
                    os.unlink(out_path)
                except OSError:
                    pass
                raise CodexRateLimitError(
                    f"codex returned rate-limit signal: {pat}"
                )

        if proc.returncode != 0:
            try:
                os.unlink(out_path)
            except OSError:
                pass
            raise CodexSpawnError(
                f"codex exec exited {proc.returncode}: stderr={stderr[:500]}"
            )

        try:
            text = Path(out_path).read_text(encoding="utf-8")
        except Exception:
            text = ""
        finally:
            try:
                os.unlink(out_path)
            except OSError:
                pass

        # Best-effort: parse last JSON line for usage telemetry.
        usage: dict[str, int] | None = None
        raw: dict[str, Any] = {"stdout_tail": stdout[-2000:]}
        for line in reversed(stdout.splitlines()):
            line = line.strip()
            if not line:
                continue
            try:
                obj = json.loads(line)
                if isinstance(obj, dict) and "usage" in obj:
                    usage = obj["usage"]
                    raw["last_event"] = obj
                    break
            except json.JSONDecodeError:
                continue

        return ProviderResult(text=text, raw=raw, usage=usage)
