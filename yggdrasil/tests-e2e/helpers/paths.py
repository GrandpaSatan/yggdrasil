"""Repo-root discovery for tests that grep source files.

Walks upward from the helper module looking for a marker pair (``.git`` and the
``yggdrasil/`` workspace dir). The result is cached so repeated calls are free.

Motivation: hard-coded ``Path(__file__).parent.parent.parent.parent`` chains
silently break when the ``tests-e2e/`` directory moves. Centralising the walk
here means test files can move and source-grep tests keep working.
"""

from __future__ import annotations

from functools import lru_cache
from pathlib import Path


@lru_cache(maxsize=1)
def repo_root() -> Path:
    """Return the absolute path to the Yggdrasil repo root.

    A valid root contains both ``.git`` and the ``yggdrasil/`` workspace dir.
    Raises ``RuntimeError`` if neither marker is found up to the filesystem root
    — that means the tests are running from an unexpected checkout layout and
    the caller should fix their invocation, not silently skip.
    """
    here = Path(__file__).resolve()
    for parent in [here, *here.parents]:
        if (parent / ".git").exists() and (parent / "yggdrasil").is_dir():
            return parent
    raise RuntimeError(f"cannot locate Yggdrasil repo root from {here}")
