"""Dream flows — consolidation, exploration, speculation, self-improvement.

Each dream flow is dispatched via the chat endpoint with ``flow`` set. These
tests are marked ``slow`` because cold LLM loads can push individual responses
past 20s — not a fit for sprint-end.
"""

from __future__ import annotations

import pytest

from helpers import OdinClient


DREAM_FLOWS = [
    "dream_consolidation",
    "dream_exploration",
    "dream_speculation",
    "dream_self_improvement",
]

# Each flow has a signature vocabulary. A response that contains none of the
# listed substrings (case-insensitive) is indistinguishable from an off-topic
# refusal — the router most likely didn't dispatch to the correct flow.
_FLOW_KEYWORDS: dict[str, tuple[str, ...]] = {
    "dream_consolidation": ("consolidat", "merge", "engram", "memory", "sprint"),
    "dream_exploration": ("explor", "discover", "novel", "hypoth", "search"),
    "dream_speculation": ("speculat", "hypothe", "imagine", "possibilit", "could"),
    "dream_self_improvement": ("improv", "optimi", "refactor", "enhance", "better"),
}


@pytest.mark.slow
@pytest.mark.parametrize("flow", DREAM_FLOWS)
@pytest.mark.required_services("odin", "mimir")
def test_dream_flow_returns_non_empty_content(odin_client: OdinClient, flow: str) -> None:
    content = odin_client.chat_content(
        f"run dream flow {flow} for sprint 066",
        flow=flow,
    ).strip()
    # Length floor rejects ``"."`` / ``"ok"`` / ``"I don't know"`` — responses
    # that technically pass a non-empty check but don't exercise the flow.
    assert len(content) > 30, (
        f"{flow} returned suspiciously short content (<30 chars): {content!r}"
    )
    keywords = _FLOW_KEYWORDS[flow]
    lowered = content.lower()
    assert any(k in lowered for k in keywords), (
        f"{flow} response has none of the expected keywords {keywords}; "
        f"the router likely didn't dispatch to this flow. Got: {content[:200]!r}"
    )
