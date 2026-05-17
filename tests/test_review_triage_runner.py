"""Tests for the JSON extraction helper of the review triage runner.

The full runner exercises the Claude Agent SDK, so we only unit-test the
deterministic helpers here. End-to-end coverage is left to manual QA + the IPC
fallback (which the handler exercises automatically).
"""

from pathlib import Path
import sys

# The runner lives in apps/backend/runners; expose it on sys.path.
BACKEND_ROOT = Path(__file__).resolve().parent.parent / "apps" / "backend"
sys.path.insert(0, str(BACKEND_ROOT))

import importlib.util


def _load_runner():
    spec_path = BACKEND_ROOT / "runners" / "review_triage_runner.py"
    spec = importlib.util.spec_from_file_location("review_triage_runner", spec_path)
    assert spec and spec.loader
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def test_extract_json_handles_plain_object():
    runner = _load_runner()
    text = '{"decisions": [{"commentId": "c1", "verdict": "redo"}]}'
    result = runner._extract_json(text)
    assert result == {"decisions": [{"commentId": "c1", "verdict": "redo"}]}


def test_extract_json_strips_prose_around_object():
    runner = _load_runner()
    text = "Sure, here is the JSON:\n\n```json\n{\"decisions\": []}\n```\n\nDone."
    result = runner._extract_json(text)
    assert result == {"decisions": []}


def test_extract_json_returns_none_for_invalid():
    runner = _load_runner()
    assert runner._extract_json("not even close") is None
    assert runner._extract_json("") is None
