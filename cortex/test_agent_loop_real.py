"""Hermetic tests for the real (non-mock) LLM-backed planning path.

These tests use ``socket.socketpair()`` so the test process itself plays the
role of vita/the LLM peer: it answers ``LlmRequest`` frames with canned
``LlmResponse`` plans and answers each ``ToolCall`` with a ``ToolResponse``.
No network, no LLM API, no subprocess.

The ``AgentLoop`` is driven in a background thread (it blocks on
``recv_message``); the test thread acts as the peer on the other socket end.
A short reply timeout is patched in so a protocol bug fails fast instead of
hanging the suite.
"""

from __future__ import annotations

import socket
import threading
from typing import Any

import pytest

from .agent_loop import AgentLoop, Step
from .ipc import recv_message, send_message

# Keep the test peer from ever stalling the whole suite.
_PEER_TIMEOUT_S = 5.0

_TOOLS = [
    {"name": "clock", "description": "wall clock"},
    {"name": "echo", "description": "echo payload"},
]


def _run_loop_in_thread(
    backend: str, request: dict[str, Any]
) -> tuple[socket.socket, threading.Thread, dict[str, Exception]]:
    """Start an ``AgentLoop.run`` on one end of a socketpair in a thread.

    Returns the *peer* socket (the vita side), the thread, and a dict that will
    hold ``{"error": exc}`` if ``run`` raised.
    """
    cortex_sock, peer_sock = socket.socketpair()
    # Tight timeouts so a stuck protocol surfaces quickly rather than hanging.
    loop = AgentLoop(cortex_sock, backend=backend)
    loop.TOOL_REPLY_TIMEOUT_S = _PEER_TIMEOUT_S
    loop.LLM_REPLY_TIMEOUT_S = _PEER_TIMEOUT_S

    captured: dict[str, Exception] = {}

    def _target() -> None:
        try:
            loop.run(request)
        except Exception as exc:  # pylint: disable=broad-except
            captured["error"] = exc
        finally:
            cortex_sock.close()

    thread = threading.Thread(target=_target, daemon=True)
    thread.start()
    return peer_sock, thread, captured


def _recv(peer: socket.socket) -> dict[str, Any]:
    peer.settimeout(_PEER_TIMEOUT_S)
    msg = recv_message(peer)
    assert msg is not None, "cortex closed the socket unexpectedly"
    return msg


def _make_request() -> dict[str, Any]:
    return {
        "task_id": "t-1",
        "description": "do the thing",
        "tools": _TOOLS,
        "identity": {"name": "anima"},
    }


# ── Full Plan → Act → Observe → InvokeComplete over a real backend ──────────────

def test_real_backend_full_cycle() -> None:
    peer, thread, captured = _run_loop_in_thread("openai", _make_request())

    # 1) The loop should ask us to plan via an LlmRequest.
    req = _recv(peer)
    assert req["type"] == "LlmRequest"
    assert req["backend"] == "openai"
    assert req["purpose"] == "plan"
    assert req["description"] == "do the thing"
    assert req["identity"] == {"name": "anima"}
    assert {t["name"] for t in req["tools"]} == {"clock", "echo"}

    # 2) Answer with a structured two-step plan.
    send_message(peer, {
        "type": "LlmResponse",
        "request_id": req["request_id"],
        "plan": [
            {"tool_name": "clock", "args": {}, "description": "record time"},
            {"tool_name": "echo", "args": {"payload": "hi"},
             "description": "echo it"},
        ],
    })

    # 3) The loop now executes each step. Answer ToolCalls and the revise
    #    LlmRequests that follow each observation (purpose="revise" → empty).
    tool_calls: list[dict[str, Any]] = []
    while True:
        msg = _recv(peer)
        if msg["type"] == "ToolCall":
            tool_calls.append(msg)
            send_message(peer, {
                "type": "ToolResponse",
                "call_id": msg["call_id"],
                "result": f"ran {msg['tool_name']}",
            })
        elif msg["type"] == "LlmRequest":
            # Revision request: decline to revise (keep current plan).
            assert msg["purpose"] == "revise"
            assert "observations" in msg
            assert "remaining_plan" in msg
            send_message(peer, {
                "type": "LlmResponse",
                "request_id": msg["request_id"],
            })
        elif msg["type"] == "InvokeComplete":
            complete = msg
            break
        else:  # pragma: no cover - defensive
            raise AssertionError(f"unexpected message: {msg}")

    thread.join(timeout=_PEER_TIMEOUT_S)
    assert not thread.is_alive()
    assert "error" not in captured, captured.get("error")

    assert [c["tool_name"] for c in tool_calls] == ["clock", "echo"]
    assert complete["tool_calls_made"] == 2
    assert "do the thing" in complete["output"]
    peer.close()


# ── Malformed LlmResponse → CortexError ─────────────────────────────────────────

def test_malformed_llm_response_yields_cortex_error() -> None:
    peer, thread, captured = _run_loop_in_thread("openai", _make_request())

    req = _recv(peer)
    assert req["type"] == "LlmRequest"

    # Neither 'plan' nor 'content' → ValueError → CortexError.
    send_message(peer, {"type": "LlmResponse", "request_id": req["request_id"]})

    err = _recv(peer)
    assert err["type"] == "CortexError"
    assert "plan" in err["message"] or "content" in err["message"]

    thread.join(timeout=_PEER_TIMEOUT_S)
    assert not thread.is_alive()
    # run() re-raises after sending CortexError.
    assert isinstance(captured.get("error"), ValueError)
    peer.close()


def test_bad_json_content_yields_cortex_error() -> None:
    peer, thread, captured = _run_loop_in_thread("openai", _make_request())

    req = _recv(peer)
    send_message(peer, {
        "type": "LlmResponse",
        "request_id": req["request_id"],
        "content": "this is not json at all",
    })

    err = _recv(peer)
    assert err["type"] == "CortexError"
    thread.join(timeout=_PEER_TIMEOUT_S)
    assert isinstance(captured.get("error"), ValueError)
    peer.close()


# ── Unknown tool names are skipped ──────────────────────────────────────────────

def test_unknown_tools_are_skipped() -> None:
    peer, thread, captured = _run_loop_in_thread("anthropic", _make_request())

    req = _recv(peer)
    assert req["backend"] == "anthropic"

    send_message(peer, {
        "type": "LlmResponse",
        "request_id": req["request_id"],
        "plan": [
            {"tool_name": "nonexistent", "args": {}, "description": "skip me"},
            {"tool_name": "echo", "args": {"payload": "kept"},
             "description": "kept"},
        ],
    })

    tool_calls: list[dict[str, Any]] = []
    while True:
        msg = _recv(peer)
        if msg["type"] == "ToolCall":
            tool_calls.append(msg)
            send_message(peer, {
                "type": "ToolResponse",
                "call_id": msg["call_id"],
                "result": "ok",
            })
        elif msg["type"] == "LlmRequest":
            send_message(peer, {
                "type": "LlmResponse",
                "request_id": msg["request_id"],
            })
        elif msg["type"] == "InvokeComplete":
            complete = msg
            break
        else:  # pragma: no cover
            raise AssertionError(f"unexpected: {msg}")

    thread.join(timeout=_PEER_TIMEOUT_S)
    assert "error" not in captured, captured.get("error")
    # Only the known tool ran.
    assert [c["tool_name"] for c in tool_calls] == ["echo"]
    # The skip was recorded as an observation surfaced in the output.
    assert "skipped" in complete["output"]
    assert "nonexistent" in complete["output"]
    peer.close()


# ── content-as-JSON-string plan is accepted ─────────────────────────────────────

def test_content_string_plan_is_parsed() -> None:
    peer, thread, captured = _run_loop_in_thread("ollama", _make_request())

    req = _recv(peer)
    # Model emitted a JSON array embedded in prose / markdown fences.
    send_message(peer, {
        "type": "LlmResponse",
        "request_id": req["request_id"],
        "content": 'Here is the plan:\n```json\n'
                   '[{"tool_name": "clock", "args": {}, "description": "t"}]\n'
                   '```\n',
    })

    tool_calls: list[dict[str, Any]] = []
    while True:
        msg = _recv(peer)
        if msg["type"] == "ToolCall":
            tool_calls.append(msg)
            send_message(peer, {
                "type": "ToolResponse",
                "call_id": msg["call_id"],
                "result": "ok",
            })
        elif msg["type"] == "LlmRequest":
            send_message(peer, {
                "type": "LlmResponse",
                "request_id": msg["request_id"],
            })
        elif msg["type"] == "InvokeComplete":
            break

    thread.join(timeout=_PEER_TIMEOUT_S)
    assert "error" not in captured, captured.get("error")
    assert [c["tool_name"] for c in tool_calls] == ["clock"]
    peer.close()


# ── Revision replaces the remaining plan (bounded) ──────────────────────────────

def test_revise_replaces_remaining_plan() -> None:
    peer, thread, captured = _run_loop_in_thread("openai", _make_request())

    # Initial two-step plan: clock then clock again. After the first step runs
    # (one step still remaining) revision rewrites the tail to an echo step.
    req = _recv(peer)
    send_message(peer, {
        "type": "LlmResponse",
        "request_id": req["request_id"],
        "plan": [
            {"tool_name": "clock", "args": {}, "description": "first"},
            {"tool_name": "clock", "args": {}, "description": "to be revised"},
        ],
    })

    tool_calls: list[dict[str, Any]] = []
    revised_once = False
    while True:
        msg = _recv(peer)
        if msg["type"] == "ToolCall":
            tool_calls.append(msg)
            send_message(peer, {
                "type": "ToolResponse",
                "call_id": msg["call_id"],
                "result": "ok",
            })
        elif msg["type"] == "LlmRequest":
            assert msg["purpose"] == "revise"
            if not revised_once:
                revised_once = True
                # One step remains; replace it with an echo step.
                assert len(msg["remaining_plan"]) == 1
                send_message(peer, {
                    "type": "LlmResponse",
                    "request_id": msg["request_id"],
                    "plan": [{"tool_name": "echo",
                              "args": {"payload": "added"},
                              "description": "added by revise"}],
                })
            else:
                send_message(peer, {
                    "type": "LlmResponse",
                    "request_id": msg["request_id"],
                })
        elif msg["type"] == "InvokeComplete":
            complete = msg
            break

    thread.join(timeout=_PEER_TIMEOUT_S)
    assert "error" not in captured, captured.get("error")
    # First clock ran; the second step was revised into an echo.
    assert [c["tool_name"] for c in tool_calls] == ["clock", "echo"]
    assert complete["tool_calls_made"] == 2
    peer.close()


# ── Mock backend remains fully behaviour-compatible ─────────────────────────────

def test_mock_backend_still_works() -> None:
    peer, thread, captured = _run_loop_in_thread("mock", _make_request())

    tool_calls: list[dict[str, Any]] = []
    while True:
        msg = _recv(peer)
        if msg["type"] == "ToolCall":
            tool_calls.append(msg)
            send_message(peer, {
                "type": "ToolResponse",
                "call_id": msg["call_id"],
                "result": "ok",
            })
        elif msg["type"] == "InvokeComplete":
            complete = msg
            break
        else:  # pragma: no cover - mock must not emit LlmRequest
            raise AssertionError(f"mock emitted unexpected message: {msg}")

    thread.join(timeout=_PEER_TIMEOUT_S)
    assert "error" not in captured, captured.get("error")
    # Mock plan is clock then echo; no LlmRequest is ever sent.
    assert [c["tool_name"] for c in tool_calls] == ["clock", "echo"]
    assert complete["tool_calls_made"] == 2
    peer.close()


# ── Unit-level parser checks (no socket needed) ─────────────────────────────────

def _state() -> Any:
    from .agent_loop import AgentState
    return AgentState(
        task_id="t", description="d", available_tools=_TOOLS, identity={},
    )


def test_parse_plan_non_list_raises() -> None:
    a, b = socket.socketpair()
    loop = AgentLoop(a, backend="openai")
    try:
        with pytest.raises(ValueError):
            loop._parse_llm_plan(_state(), {"plan": {"not": "a list"}})
    finally:
        a.close()
        b.close()


def test_parse_plan_all_unknown_raises() -> None:
    a, b = socket.socketpair()
    loop = AgentLoop(a, backend="openai")
    try:
        with pytest.raises(ValueError):
            loop._parse_llm_plan(
                _state(),
                {"plan": [{"tool_name": "ghost", "args": {},
                           "description": ""}]},
            )
    finally:
        a.close()
        b.close()


def test_normalise_args_passes_through_valid_json_string() -> None:
    assert AgentLoop._normalise_args('{"a": 1}') == '{"a": 1}'
    assert AgentLoop._normalise_args({"a": 1}) == '{"a":1}'
    with pytest.raises(ValueError):
        AgentLoop._normalise_args("not json")


def test_step_dataclass_shape() -> None:
    s = Step(tool_name="echo", args="{}", description="x")
    assert (s.tool_name, s.args, s.description) == ("echo", "{}", "x")
