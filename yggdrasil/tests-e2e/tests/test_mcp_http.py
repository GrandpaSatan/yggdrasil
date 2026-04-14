"""MCP HTTP server — SSE handshake + JSON-RPC session."""

from __future__ import annotations

import pytest

from helpers import McpHttpClient


@pytest.mark.required_services("mcp_http")
def test_mcp_sse_stream_opens(mcp_client: McpHttpClient) -> None:
    """Open the SSE stream and read at least one event (or the ': ping' keepalive)."""
    with mcp_client.open_sse() as resp:
        assert resp.status_code == 200, f"SSE endpoint must be 200, got {resp.status_code}"
        ctype = resp.headers.get("content-type", "")
        assert "text/event-stream" in ctype, (
            f"SSE endpoint must serve text/event-stream, got {ctype!r}"
        )
        # Read up to 1KB or 1 line — don't block forever.
        got_any = False
        for raw_line in resp.iter_lines(decode_unicode=True):
            got_any = True
            break
        assert got_any, "SSE stream must send at least one line"


@pytest.mark.required_services("mcp_http")
def test_mcp_messages_endpoint_reachable(mcp_client: McpHttpClient) -> None:
    """A JSON-RPC initialize call must return a valid JSON-RPC envelope.

    We intentionally send without a session_id to probe the endpoint directly.
    The server may legitimately reject this with a JSON-RPC error (session
    required) OR accept and return a result — either is a healthy response.
    404 (endpoint missing), 5xx (server broken), or non-JSON bodies are failures.
    """
    resp = mcp_client.send_message(
        session_id="",
        payload={"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {}},
    )
    # 404 = endpoint missing; 5xx = server failure. Both are real regressions.
    assert resp.status_code in (200, 202, 400, 422), (
        f"MCP /messages must return a structured JSON-RPC response "
        f"(2xx accept or 4xx typed error), got {resp.status_code}: {resp.text[:200]}"
    )
    # The body must be a valid JSON-RPC 2.0 envelope regardless of accept/reject.
    try:
        body = resp.json()
    except ValueError as exc:
        pytest.fail(f"MCP /messages returned non-JSON body: {exc}; text={resp.text[:200]!r}")
    assert isinstance(body, dict), f"JSON-RPC body must be an object, got {type(body).__name__}"
    assert body.get("jsonrpc") == "2.0", f"envelope must declare jsonrpc=2.0; got {body!r}"
    # Exactly one of `result` or `error` must be present — never both, never neither.
    has_result = "result" in body
    has_error = "error" in body
    assert has_result ^ has_error, (
        f"JSON-RPC envelope must carry exactly one of result/error; got {body!r}"
    )
    if has_error:
        err = body["error"]
        assert isinstance(err, dict) and "code" in err and "message" in err, (
            f"JSON-RPC error must carry code+message; got {err!r}"
        )
