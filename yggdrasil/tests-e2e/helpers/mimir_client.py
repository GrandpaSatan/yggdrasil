"""Thin client for Mimir HTTP endpoints used by E2E tests."""

from __future__ import annotations

from typing import Any

import requests

from .services import check_response, retry_policy


def _extract_list(payload: dict[str, Any], keys: tuple[str, ...]) -> list[Any]:
    """Return the first present key's value as a list, else ``[]``.

    Uses explicit ``in`` membership rather than truthiness chaining — a legitimate
    empty list under ``events`` would otherwise fall through to a populated
    ``results`` field and silently hide a schema regression.
    """
    for k in keys:
        if k in payload:
            v = payload[k]
            return v if isinstance(v, list) else []
    return []


_LIST_KEYS = ("events", "results", "engrams")


class MimirClient:
    def __init__(self, base_url: str, vault_token: str | None = None, timeout: float = 20.0):
        self.base_url = base_url.rstrip("/")
        self.vault_token = vault_token
        self.timeout = timeout

    def _url(self, path: str) -> str:
        return f"{self.base_url}{path}"

    def health(self) -> requests.Response:
        return requests.get(self._url("/health"), timeout=5.0)

    @retry_policy()
    def store(
        self,
        cause: str,
        effect: str,
        *,
        tags: list[str] | None = None,
        project: str | None = "yggdrasil",
        force: bool = True,
    ) -> str:
        """POST /api/v1/store with the actual NewEngram schema.

        ``force=True`` bypasses the novelty gate so test runs don't 409 on
        re-runs with similar content. Tests that specifically exercise the
        novelty gate should pass ``force=False``.
        """
        body: dict[str, Any] = {
            "cause": cause,
            "effect": effect,
            "tags": tags or [],
            "force": force,
        }
        if project:
            body["project"] = project
        resp = check_response(
            requests.post(self._url("/api/v1/store"), json=body, timeout=self.timeout)
        )
        resp.raise_for_status()
        payload = resp.json()
        return payload.get("id") or payload.get("engram_id") or ""

    @retry_policy()
    def recall(
        self,
        query: str,
        *,
        limit: int = 5,
        project: str | None = "yggdrasil",
        include_global: bool = True,
    ) -> list[dict[str, Any]]:
        """POST /api/v1/recall with the actual EngramQuery schema (field is `text`, not `query`)."""
        body: dict[str, Any] = {
            "text": query,
            "limit": limit,
            "include_global": include_global,
        }
        if project:
            body["project"] = project
        resp = check_response(
            requests.post(self._url("/api/v1/recall"), json=body, timeout=self.timeout)
        )
        resp.raise_for_status()
        payload = resp.json()
        return _extract_list(payload, _LIST_KEYS)

    def get_engram(self, engram_id: str) -> dict[str, Any] | None:
        resp = requests.get(self._url(f"/api/v1/engrams/{engram_id}"), timeout=10.0)
        if resp.status_code == 404:
            return None
        resp.raise_for_status()
        return resp.json()

    def delete_engram(self, engram_id: str) -> bool:
        """Return True only for 200/204 (actually removed something).

        404 is intentionally NOT a success: if the caller is asserting "delete
        worked" and the engram never existed (because a silent store failure
        upstream), we'd otherwise return True and mask the real bug. Callers
        doing idempotent cleanup should tolerate ``False`` explicitly via
        :meth:`delete_engram_idempotent`.
        """
        resp = requests.delete(self._url(f"/api/v1/engrams/{engram_id}"), timeout=10.0)
        return resp.status_code in (200, 204)

    def delete_engram_idempotent(self, engram_id: str) -> bool:
        """Cleanup-friendly delete: treats 404 as success.

        Used by bulk tag-purge on teardown where "already gone" is fine.
        """
        resp = requests.delete(self._url(f"/api/v1/engrams/{engram_id}"), timeout=10.0)
        return resp.status_code in (200, 204, 404)

    def delete_supported(self) -> bool:
        """Probe whether DELETE /api/v1/engrams/{id} is implemented (some builds 405)."""
        # Use a syntactically-valid but non-existent UUID; we only care about 405 vs 404.
        resp = requests.delete(
            self._url("/api/v1/engrams/00000000-0000-0000-0000-000000000000"),
            timeout=5.0,
        )
        return resp.status_code != 405

    def delete_by_tag(self, tag: str, project: str = "yggdrasil") -> int:
        """Best-effort cleanup helper. Returns count of successful deletions.

        Recall is a SEMANTIC search — it may surface engrams that share SDR
        overlap with the tag string but were created by other tests. We
        therefore require the result's ``tags`` field to literally contain
        ``tag`` before deleting. The idempotent delete variant treats 404 as
        success so a racing cleanup doesn't double-count.

        Silently no-ops if DELETE is not supported on this Mimir build (405).
        """
        if not self.delete_supported():
            return 0
        body = {"text": tag, "limit": 100, "project": project, "include_global": True}
        try:
            resp = requests.post(self._url("/api/v1/recall"), json=body, timeout=10.0)
            resp.raise_for_status()
            engrams = _extract_list(resp.json(), _LIST_KEYS)
        except requests.RequestException:
            return 0

        count = 0
        for e in engrams:
            tags = e.get("tags") or []
            if tag not in tags:
                continue  # semantic match but not actually tagged — skip
            eid = e.get("id") or e.get("engram_id")
            if eid and self.delete_engram_idempotent(eid):
                count += 1
        return count

    def timeline(
        self,
        *,
        text: str | None = None,
        after: str | None = None,
        before: str | None = None,
        limit: int = 20,
    ) -> list[dict[str, Any]]:
        """POST /api/v1/timeline with the actual TimelineRequest schema."""
        body: dict[str, Any] = {"limit": limit}
        if text:
            body["text"] = text
        if after:
            body["after"] = after
        if before:
            body["before"] = before
        resp = requests.post(self._url("/api/v1/timeline"), json=body, timeout=self.timeout)
        resp.raise_for_status()
        return _extract_list(resp.json(), _LIST_KEYS)

    def stats(self) -> dict[str, Any]:
        resp = requests.get(self._url("/api/v1/stats"), timeout=5.0)
        resp.raise_for_status()
        return resp.json()

    # ── Vault ────────────────────────────────────────────────────────────
    def _vault_headers(self, token: str | None = None) -> dict[str, str]:
        effective = token if token is not None else self.vault_token
        if not effective:
            return {}
        return {"Authorization": f"Bearer {effective}"}

    def vault_get(self, key: str, *, token: str | None = None) -> requests.Response:
        return requests.post(
            self._url("/api/v1/vault"),
            json={"action": "get", "key": key},
            headers=self._vault_headers(token),
            timeout=10.0,
        )

    def vault_set(self, key: str, value: str, *, token: str | None = None) -> requests.Response:
        return requests.post(
            self._url("/api/v1/vault"),
            json={"action": "set", "key": key, "value": value},
            headers=self._vault_headers(token),
            timeout=10.0,
        )

    def vault_delete(self, key: str, *, token: str | None = None) -> requests.Response:
        return requests.post(
            self._url("/api/v1/vault"),
            json={"action": "delete", "key": key},
            headers=self._vault_headers(token),
            timeout=10.0,
        )
