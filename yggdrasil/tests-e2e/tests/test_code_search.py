"""Muninn retrieval flows: code search, assembly.

Response shape matches ``SearchResponse`` / ``SearchResult`` in
crates/ygg-domain/src/chunk.rs: each result carries a nested ``chunk`` dict
with ``file_path``, ``language`` (snake_case — "rust", "python"), ``content``.
The ``MuninnClient.search`` helper returns the ``results`` array directly.
"""

from __future__ import annotations

import pytest

from helpers import MuninnClient


@pytest.mark.required_services("muninn")
def test_code_search_returns_chunks_for_known_symbol(muninn_client: MuninnClient) -> None:
    """Search for ``assemble_context`` — a stable public symbol in the Muninn crate.

    Defined at crates/muninn/src/assembler.rs:36 and used across the codebase,
    so the index must contain at least one result whose path points at that file.
    An empty result list means the index is stale or the filter is broken — both
    are real regressions, not silent passes.
    """
    results = muninn_client.search("assemble_context token budget", limit=5)
    assert isinstance(results, list), "search must return a list"
    assert len(results) >= 1, (
        "search for a known stable symbol must return at least one result; "
        "empty means the code index is stale or the search is broken"
    )
    top = results[0]
    assert isinstance(top, dict), f"each result must be a dict; got {type(top).__name__}"
    chunk = top.get("chunk")
    assert isinstance(chunk, dict), f"result.chunk must be a dict; got {top!r}"
    file_path = chunk.get("file_path", "")
    content = chunk.get("content", "")
    assert isinstance(file_path, str) and file_path, (
        f"chunk.file_path must be a non-empty string; got {chunk!r}"
    )
    assert isinstance(content, str) and content, (
        f"chunk.content must be a non-empty string; got {chunk!r}"
    )
    # The top hit for this query should anchor on the definition file. We tolerate
    # any result ranked in the top 5 matching the expected file, since BM25+vector
    # fusion can shuffle the top slot depending on index freshness.
    anchor_paths = [r["chunk"]["file_path"] for r in results if isinstance(r.get("chunk"), dict)]
    assert any("assembler.rs" in p for p in anchor_paths), (
        f"expected at least one hit under muninn/src/assembler.rs; "
        f"got paths: {anchor_paths!r}"
    )


@pytest.mark.required_services("muninn")
def test_code_search_language_filter_respected(muninn_client: MuninnClient) -> None:
    """``languages=["rust"]`` must restrict every result to Rust chunks."""
    results = muninn_client.search("async fn main", limit=10, languages=["rust"])
    assert isinstance(results, list)
    assert len(results) >= 1, (
        "filtering for a ubiquitous Rust pattern must return at least one hit; "
        "empty means the filter dropped everything or the index lacks Rust content"
    )
    for r in results:
        assert isinstance(r, dict), f"each result must be a dict; got {type(r).__name__}"
        chunk = r.get("chunk")
        assert isinstance(chunk, dict), f"result.chunk must be a dict; got {r!r}"
        # ``Language`` serializes snake_case: "rust", "type_script", "java_script"...
        lang = chunk.get("language")
        assert isinstance(lang, str) and lang, (
            f"chunk.language must be a non-empty string; got {chunk!r}"
        )
        assert lang.lower() == "rust", (
            f"language filter violated — expected 'rust', got {lang!r}; chunk={chunk!r}"
        )


@pytest.mark.required_services("muninn")
def test_assemble_returns_context_block(muninn_client: MuninnClient) -> None:
    payload = muninn_client.assemble("engram storage flow", limit=3)
    assert isinstance(payload, dict), "assemble must return a dict"
    # Find the first present key AND verify its value is non-empty — a payload
    # shaped like ``{"results": []}`` otherwise passes the "key present" check
    # while masking a broken retriever.
    key, value = next(
        ((k, payload[k]) for k in ("context", "chunks", "assembled", "results") if k in payload),
        (None, None),
    )
    assert key is not None, (
        f"assemble response missing any known content key; got keys {list(payload)}"
    )
    assert value, (
        f"assemble returned empty {type(value).__name__} under {key!r}; retriever broken"
    )
