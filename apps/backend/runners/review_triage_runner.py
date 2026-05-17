#!/usr/bin/env python3
"""
Review Triage Runner
====================

Reads a JSON payload from stdin describing inline review comments left by a
human on a task's diff. Calls the `review_triage` agent (Haiku 4.5 by default)
through the Claude Agent SDK and emits a JSON TriageReport on stdout.

This runner is intentionally minimal: read-only tools, low thinking budget,
no MCP servers. It is invoked by the Electron main process whenever the user
clicks "Done Review" with open inline comments.

Payload (stdin):

    {
      "specId": "...",
      "projectPath": "...",
      "comments": [
        { "id": "...", "file": "...", "line": N, "side": "LEFT"|"RIGHT", "body": "..." }
      ]
    }

Stdout: a single JSON object — see prompts/review_triage.md for schema.
Stderr: human-readable debug log.

Exit codes: 0 success, non-zero on failure (the IPC handler falls back to a
heuristic so the user is never blocked).
"""

from __future__ import annotations

import asyncio
import json
import sys
from pathlib import Path

# Ensure backend root is importable.
sys.path.insert(0, str(Path(__file__).parent.parent))


def _log(msg: str) -> None:
    print(f"[review_triage] {msg}", file=sys.stderr, flush=True)


async def _run(payload: dict) -> dict:
    project_path = Path(payload["projectPath"]).resolve()
    spec_id = payload["specId"]
    comments = payload.get("comments") or []

    spec_dir = project_path / ".auto-claude" / "specs" / spec_id
    if not spec_dir.exists():
        # Fall back to project_path so create_client doesn't choke on missing dir.
        spec_dir = project_path

    prompt_path = Path(__file__).parent.parent / "prompts" / "review_triage.md"
    if not prompt_path.exists():
        raise FileNotFoundError(f"Triage prompt missing: {prompt_path}")
    base_prompt = prompt_path.read_text(encoding="utf-8")

    # Compose the user query — system prompt is the markdown file; the input
    # JSON is passed in the same turn so the model can correlate ids.
    payload_for_model = {
        "specId": spec_id,
        "projectPath": str(project_path),
        "comments": comments,
    }
    query = (
        base_prompt
        + "\n\n---\n\n## Input\n\n```json\n"
        + json.dumps(payload_for_model, indent=2)
        + "\n```\n\nReturn the JSON now."
    )

    # Lazy import — keeps cold-start cheap when sys.argv triggers --help, etc.
    from core.client import create_client
    from phase_config import (
        get_thinking_kwargs_for_model,
        resolve_model_id,
    )

    # Haiku 4.5 — fast, structured-classification friendly. Fallback to the
    # canonical id when phase_config can't resolve it (older configs).
    model_id = resolve_model_id("haiku") or "claude-haiku-4-5-20251001"
    thinking_kwargs = get_thinking_kwargs_for_model(model_id, "low")

    client = create_client(
        project_dir=project_path,
        spec_dir=spec_dir,
        model=model_id,
        agent_type="review_triage",
        **thinking_kwargs,
    )

    response_text = ""
    async with client:
        _log(f"Sending {len(comments)} comment(s) to {model_id}")
        await client.query(query)
        async for msg in client.receive_response():
            msg_type = type(msg).__name__
            if msg_type == "AssistantMessage" and hasattr(msg, "content"):
                for block in msg.content:
                    if type(block).__name__ == "TextBlock" and hasattr(block, "text"):
                        response_text += block.text

    # Extract the JSON object — the model may pad with prose despite the prompt.
    parsed = _extract_json(response_text)
    if not parsed or "decisions" not in parsed:
        raise ValueError(f"Model returned no parseable decisions; raw: {response_text[:500]}")

    # Defensive normalization: ensure every input comment has a decision.
    decided = {d.get("commentId") for d in parsed.get("decisions", [])}
    for c in comments:
        if c["id"] not in decided:
            parsed.setdefault("decisions", []).append(
                {
                    "commentId": c["id"],
                    "verdict": "redo",
                    "rationale": "Model omitted this comment — defaulting to redo.",
                }
            )

    return parsed


def _extract_json(text: str) -> dict | None:
    """Find the largest JSON object in the text and parse it."""
    if not text:
        return None
    start = text.find("{")
    end = text.rfind("}")
    if start == -1 or end == -1 or end <= start:
        return None
    candidate = text[start : end + 1]
    try:
        return json.loads(candidate)
    except json.JSONDecodeError:
        # Try removing surrounding code fences.
        cleaned = candidate.replace("```json", "").replace("```", "").strip()
        try:
            return json.loads(cleaned)
        except json.JSONDecodeError:
            return None


def main() -> int:
    try:
        raw = sys.stdin.read()
        if not raw.strip():
            print(json.dumps({"decisions": [], "summary": "Empty payload."}))
            return 0
        payload = json.loads(raw)
    except Exception as err:  # noqa: BLE001 — top-level boundary
        _log(f"Failed to parse stdin: {err}")
        return 2

    try:
        report = asyncio.run(_run(payload))
    except Exception as err:  # noqa: BLE001 — top-level boundary
        _log(f"Triage failed: {err}")
        return 1

    print(json.dumps(report))
    return 0


if __name__ == "__main__":
    sys.exit(main())
