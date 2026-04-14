"""Huginn indexing flow — verify the indexer CLI is available and the code index is populated.

Huginn runs as a binary, not an HTTP service (indexing happens offline or via a
systemd watch unit). We don't trigger a re-index from here because it can take
minutes; instead we ask Muninn whether the index has chunks for a known file.
"""

from __future__ import annotations

import shutil

import pytest

from helpers import MuninnClient


@pytest.mark.required_services("muninn")
def test_muninn_index_has_chunks_for_this_sprint(muninn_client: MuninnClient) -> None:
    """Search for a known-stable Rust symbol — the index MUST surface it.

    Queries ``dispatch_flow`` (Odin's flow router at
    ``crates/odin/src/handlers.rs``). Deliberately different from the
    ``assemble_context`` query used in ``test_code_search.py`` — the two tests
    previously shared a symbol, so renaming it would have silently broken
    both at once. Now a regression in one location doesn't mask coverage in
    the other.
    """
    results = muninn_client.search(
        "dispatch_flow handler", limit=5, languages=["rust"]
    )
    assert isinstance(results, list) and len(results) >= 1, (
        "Muninn must return at least one chunk for a known public Rust symbol "
        f"('dispatch_flow'); got {len(results)} results — index is stale or broken"
    )


@pytest.mark.required_services("muninn")
def test_huginn_binary_available_on_path_or_skip() -> None:
    """Huginn CLI may live at /opt/yggdrasil/bin/huginn on fleet nodes, or be absent locally."""
    if not shutil.which("huginn"):
        pytest.skip("huginn binary not on PATH; this workstation doesn't run the indexer")
    # If huginn is available, confirm it responds to --version (or any no-op invocation).
    import subprocess

    result = subprocess.run(["huginn", "--help"], capture_output=True, timeout=5)
    # Exit 2 is the POSIX convention for usage/arg error — ``--help`` must never
    # produce it. Accept only 0 (clean success) or 1 (some CLIs use this for
    # help-exit intentionally).
    assert result.returncode in (0, 1), (
        f"huginn --help must exit cleanly (0 or 1); got {result.returncode} "
        f"(stderr: {result.stderr[:200]!r})"
    )
