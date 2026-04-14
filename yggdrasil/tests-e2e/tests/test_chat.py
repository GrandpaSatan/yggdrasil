"""Chat completion smoke + home_automation router regression guard.

Ported from ``scripts/smoke/e2e-live.sh``:
  - ``turn on the kitchen light`` must route to HA and mention the light.
  - ``turn on kitchen light while I play Fallout`` must still route HA
    (Sprint 062 router regression guard — mixed HA+gaming intent must not
    collapse to gaming).
"""

from __future__ import annotations

import re

import pytest

from helpers import OdinClient


@pytest.mark.required_services("odin")
def test_chat_completion_200_with_non_empty_content(odin_client: OdinClient) -> None:
    content = odin_client.chat_content("say hello in one word")
    assert content.strip(), "chat must return non-empty assistant content"


_LIGHT_NOUN = re.compile(r"\b(light|lamp)\b", re.IGNORECASE)
# ``switch`` removed: a refusal like "I can switch topics" satisfied the action
# term without the model acting on the HA intent. ``kitchen`` anchors location,
# ``turn`` anchors the verb — both are specific enough that a refusal response
# would need to echo the user's words to match.
_HA_ACTION = re.compile(r"\b(kitchen|turn)\b", re.IGNORECASE)


@pytest.mark.required_services("odin")
def test_ha_flow_mentions_kitchen_light(odin_client: OdinClient) -> None:
    content = odin_client.chat_content("turn on the kitchen light")
    assert content, "chat must return content for HA message"
    # Require BOTH a lighting noun AND an action/location term — the previous
    # single-word regex accepted ``"on"`` which appears in virtually any English
    # refusal ("I'm only an assistant") so the model could ignore the intent
    # entirely and still pass.
    assert _LIGHT_NOUN.search(content) and _HA_ACTION.search(content), (
        f"HA response must reference BOTH a lighting noun and an action/location; "
        f"got: {content[:200]!r}"
    )


@pytest.mark.required_services("odin")
def test_mixed_ha_plus_gaming_still_routes_ha(odin_client: OdinClient) -> None:
    """Sprint 062 router regression guard — the HA intent must not be lost."""
    content = odin_client.chat_content(
        "turn on the kitchen light while I play Fallout"
    )
    assert content, "chat must return content for mixed-intent message"
    assert _LIGHT_NOUN.search(content) and _HA_ACTION.search(content), (
        "mixed HA+gaming message must still reference light control with both a "
        f"lighting noun and an action/location term; got: {content[:200]!r}"
    )
