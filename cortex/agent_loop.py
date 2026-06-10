"""LangGraph-style Plan / Act / Observe / Revise agent loop.

The loop drives a deliberative task through four explicit stages:

Plan
    Generate an ordered list of steps to achieve the task goal.  In the
    mock backend this is derived deterministically from the task description;
    with a real LLM the plan is produced via an ``LlmRequest``.

Act
    Execute the next step in the plan.  Steps that require external effects
    emit ``ToolCall`` messages and wait for ``ToolResponse`` replies from vita.

Observe
    Incorporate the tool response into the working context.

Revise
    Optionally revise the remaining plan in light of the latest observation.
    The mock backend skips revision to keep the test surface hermetic.

Termination
    The loop terminates when the plan queue is empty or when the configured
    ``max_tool_calls`` limit is reached.  The final output and an episode
    summary are emitted in a single ``InvokeComplete`` message.

Mock backend
~~~~~~~~~~~~
When ``backend="mock"`` (the default for CI), the loop uses a rule-based
plan generator that:

1. Calls ``clock`` to record the start time.
2. Calls ``echo`` with the task description to prove tool round-trips work.
3. Produces a deterministic summary.

This guarantees that every exit criterion of E5.1 can be exercised without a
live LLM API key.
"""

from __future__ import annotations

import json
import socket
import time
import uuid
from dataclasses import dataclass, field
from typing import Any

from .ipc import recv_message, send_message


@dataclass
class Step:
    """A single planned action."""
    tool_name: str
    args: str
    description: str


@dataclass
class AgentState:
    """Mutable state threaded through the PAOR loop."""
    task_id: str
    description: str
    available_tools: list[dict[str, str]]
    identity: dict[str, Any]
    plan: list[Step] = field(default_factory=list)
    observations: list[str] = field(default_factory=list)
    tool_calls_made: int = 0
    start_time: float = field(default_factory=time.time)


class AgentLoop:
    """Plan / Act / Observe / Revise loop over an IPC socket."""

    MAX_TOOL_CALLS: int = 10
    # Liveness backstop: if vita does not reply to a ToolCall within this many
    # seconds the invocation is aborted rather than blocking forever (M2).
    TOOL_REPLY_TIMEOUT_S: float = 120.0

    def __init__(
        self,
        sock: socket.socket,
        backend: str = "mock",
    ) -> None:
        self._sock = sock
        self._backend = backend

    # ── Public API ─────────────────────────────────────────────────────────────

    def run(self, request: dict[str, Any]) -> None:
        """Execute the full PAOR loop for *request* and send ``InvokeComplete``.

        Sends exactly one ``InvokeComplete`` (or ``CortexError``) before
        returning.
        """
        state = AgentState(
            task_id=request.get("task_id", str(uuid.uuid4())),
            description=request.get("description", ""),
            available_tools=request.get("tools", []),
            identity=request.get("identity", {}),
        )

        try:
            # Plan
            state.plan = self._plan(state)

            # Act → Observe → (Revise) loop
            while state.plan and state.tool_calls_made < self.MAX_TOOL_CALLS:
                step = state.plan.pop(0)
                result = self._act(state, step)
                self._observe(state, step, result)
                self._revise(state)

            # Produce final output and summary
            output = self._synthesise_output(state)
            summary = self._summarise_episode(state)

            send_message(self._sock, {
                "type": "InvokeComplete",
                "output": output,
                "episode_summary": summary,
                "tool_calls_made": state.tool_calls_made,
            })

        except Exception as exc:  # pylint: disable=broad-except
            send_message(self._sock, {
                "type": "CortexError",
                "message": str(exc),
            })
            raise

    # ── Phase implementations ──────────────────────────────────────────────────

    def _plan(self, state: AgentState) -> list[Step]:
        """Generate the initial action plan."""
        if self._backend == "mock":
            return self._mock_plan(state)
        # Real LLM backends are not yet implemented. Fail loudly rather than
        # silently impersonating success by running the mock plan, which would
        # archive a fake episode to L3 as if real deliberation occurred (M3).
        raise NotImplementedError(
            f"backend {self._backend!r} is not implemented; only 'mock' is "
            "currently supported"
        )

    def _mock_plan(self, state: AgentState) -> list[Step]:
        """Deterministic two-step plan for hermetic testing.

        Step 1: call ``clock`` to record wall time.
        Step 2: call ``echo`` with the task description to demonstrate a
                real tool round-trip.
        """
        tool_names = {t["name"] for t in state.available_tools}

        steps: list[Step] = []
        if "clock" in tool_names:
            steps.append(Step(
                tool_name="clock",
                args="{}",
                description="Record current wall time",
            ))
        if "echo" in tool_names:
            steps.append(Step(
                tool_name="echo",
                # Build the args with json.dumps so a task description
                # containing quotes/backslashes/control chars cannot break out
                # of the JSON string or inject extra keys (H1).
                args=json.dumps({"payload": state.description[:80]}),
                description="Echo task description to confirm tool round-trip",
            ))

        # Fallback: if neither clock nor echo is available, use the first tool.
        if not steps and state.available_tools:
            first = state.available_tools[0]
            steps.append(Step(
                tool_name=first["name"],
                args="{}",
                description=f'Call {first["name"]}',
            ))

        return steps

    def _act(self, state: AgentState, step: Step) -> str:
        """Execute *step* by issuing a ``ToolCall`` and waiting for the reply."""
        call_id = f"call-{state.tool_calls_made}-{state.task_id}"
        send_message(self._sock, {
            "type": "ToolCall",
            "call_id": call_id,
            "tool_name": step.tool_name,
            "args": step.args,
        })
        state.tool_calls_made += 1

        # Wait for the matching ToolResponse, bounding the wait with a deadline
        # so a stalled or silent vita cannot wedge the cortex forever (M2). A
        # Shutdown/Cancel/CortexError control message aborts the invocation.
        deadline = time.monotonic() + self.TOOL_REPLY_TIMEOUT_S
        while True:
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                raise TimeoutError(
                    f"timed out waiting for ToolResponse to {call_id!r} after "
                    f"{self.TOOL_REPLY_TIMEOUT_S:.0f}s"
                )
            self._sock.settimeout(remaining)
            try:
                msg = recv_message(self._sock)
            except (socket.timeout, TimeoutError) as exc:
                raise TimeoutError(
                    f"timed out waiting for ToolResponse to {call_id!r} after "
                    f"{self.TOOL_REPLY_TIMEOUT_S:.0f}s"
                ) from exc
            finally:
                self._sock.settimeout(None)

            if msg is None:
                raise RuntimeError("vita closed the IPC socket unexpectedly")
            msg_type = msg.get("type", "")
            if msg_type == "ToolResponse" and msg.get("call_id") == call_id:
                if msg.get("error"):
                    return f"[error: {msg['error']}]"
                return msg.get("result", "")
            if msg_type in ("Shutdown", "Cancel", "CortexError"):
                raise RuntimeError(
                    f"invocation aborted by vita control message: {msg_type}"
                )
            # Ignore other unexpected messages (future protocol extensions).

    def _observe(self, state: AgentState, step: Step, result: str) -> None:
        """Record the tool result as an observation."""
        state.observations.append(
            f"[{step.tool_name}] {step.description} → {result!r}"
        )

    def _revise(self, state: AgentState) -> None:
        """Optionally revise the plan (no-op in the mock backend)."""

    # ── Output synthesis ───────────────────────────────────────────────────────

    def _synthesise_output(self, state: AgentState) -> str:
        """Produce a human-readable task completion report."""
        lines = [f"Task completed: {state.description!r}"]
        lines.append(f"  Tool calls: {state.tool_calls_made}")
        for obs in state.observations:
            lines.append(f"  • {obs}")
        return "\n".join(lines)

    def _summarise_episode(self, state: AgentState) -> str:
        """Produce a compact episode summary for L3 archival."""
        duration = time.time() - state.start_time
        return (
            f"task_id={state.task_id} "
            f"description={state.description!r} "
            f"tool_calls={state.tool_calls_made} "
            f"observations={len(state.observations)} "
            f"duration_s={duration:.3f}"
        )
