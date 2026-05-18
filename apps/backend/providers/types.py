from dataclasses import dataclass, field
from typing import Any


@dataclass
class ProviderRequest:
    system: str
    user: str
    cwd: str
    model: str | None = None
    extra: dict[str, Any] = field(default_factory=dict)


@dataclass
class ProviderResult:
    text: str
    raw: dict[str, Any] = field(default_factory=dict)
    usage: dict[str, int] | None = None
