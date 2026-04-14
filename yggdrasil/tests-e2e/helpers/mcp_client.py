"""Thin client for the MCP HTTP server (SSE + JSON-RPC)."""

from __future__ import annotations

import json
from typing import Any

import requests


class McpHttpClient:
    """Drives the /sse + /messages endpoints of ygg-mcp-server.

    The full SSE handshake is intentionally minimal here — the E2E test that
    wants to verify a session lifecycle opens the SSE stream itself via
    ``requests.get(stream=True)`` so it can assert event framing directly.
    """

    def __init__(self, base_url: str, timeout: float = 15.0):
        self.base_url = base_url.rstrip("/")
        self.timeout = timeout

    def _url(self, path: str) -> str:
        return f"{self.base_url}{path}"

    def open_sse(self) -> requests.Response:
        """Return a streaming response for the SSE channel. Caller must close."""
        return requests.get(
            self._url("/sse"),
            stream=True,
            timeout=(self.timeout, None),
            headers={"Accept": "text/event-stream"},
        )

    def send_message(self, session_id: str, payload: dict[str, Any]) -> requests.Response:
        return requests.post(
            self._url("/messages"),
            params={"session_id": session_id} if session_id else None,
            json=payload,
            timeout=self.timeout,
        )

    def rpc(self, session_id: str, method: str, params: dict[str, Any] | None = None, *, rpc_id: int = 1) -> dict[str, Any]:
        payload = {
            "jsonrpc": "2.0",
            "id": rpc_id,
            "method": method,
            "params": params or {},
        }
        resp = self.send_message(session_id, payload)
        resp.raise_for_status()
        return resp.json()

    @staticmethod
    def parse_sse_block(lines: list[str]) -> tuple[str, Any] | None:
        """Parse a contiguous SSE block into (event_type, payload).

        The SSE spec allows ``data:`` to repeat — consecutive ``data:`` lines
        are joined with ``\\n`` before the payload is decoded. This is the
        correct entry point for streams that may emit multi-line JSON events.
        Returns None if the block contains no ``data:`` lines.
        """
        data_parts: list[str] = []
        for line in lines:
            if not line or line.startswith(":"):
                continue
            if line.startswith("data:"):
                data_parts.append(line[len("data:"):].lstrip())
        if not data_parts:
            return None
        raw = "\n".join(data_parts)
        try:
            return ("data", json.loads(raw))
        except json.JSONDecodeError:
            return ("data", raw)

    @staticmethod
    def parse_sse_event(line: str) -> tuple[str, Any] | None:
        """Back-compat single-line wrapper around :meth:`parse_sse_block`.

        Prefer ``parse_sse_block`` for real streams — a JSON payload split
        across multiple ``data:`` lines would be lost here.
        """
        return McpHttpClient.parse_sse_block([line])
