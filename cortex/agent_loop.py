"""LangGraph-style Plan / Act / Observe / Revise agent loop.

The loop drives a deliberative task through four explicit stages:

Plan
    Generate an ordered list of steps to achieve the task goal.  In the
    mock backend this is derived deterministically from the task description;
    with a real LLM backend (``openai``/``anthropic``/``ollama``/
    ``hf_transformers``) the plan is produced via an ``LlmRequest`` round-trip
    over IPC — cortex never talks to a model API directly; it asks vita (the
    Rust side) to run the completion and return an ``LlmResponse``.

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

LLM planning protocol (real backends)
~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~
For any non-mock backend, ``_plan`` and ``_revise`` exchange a pair of frames
with vita::

    cortex → vita   {"type": "LlmRequest",
                     "request_id": <str>,
                     "backend": <str>,
                     "purpose": "plan" | "revise",
                     "description": <str>,
                     "tools": [<tool spec>, ...],
                     "identity": <dict>,
                     # only present for purpose == "revise":
                     "observations": [<str>, ...],
                     "remaining_plan": [{"tool_name", "args", "description"}, ...]}

    vita → cortex   {"type": "LlmResponse",
                     "request_id": <str>,          # echoed back (optional)
                     "plan": [{"tool_name", "args", "description"}, ...],
                     # ── or ──
                     "content": <str>}             # JSON the model emitted

The response is parsed into ``list[Step]`` (see ``_parse_llm_plan``).  Steps
referencing tools that are not in ``available_tools`` are skipped (recorded as
an observation).  A malformed response raises ``ValueError`` which ``run``
converts into a ``CortexError`` frame.
"""

from __future__ import annotations

import json
import socket
import time
import uuid
from collections.abc import Callable
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
    # Liveness backstop for LLM planning round-trips. Completions can be slower
    # than tool dispatch, so this bound is more generous than the tool timeout.
    LLM_REPLY_TIMEOUT_S: float = 300.0

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
        """Generate the initial action plan.

        The mock backend uses a deterministic rule-based generator.  Every
        other backend asks vita to run a planning completion via an
        ``LlmRequest`` and converts the returned ``LlmResponse`` into steps.
        """
        if self._backend == "mock":
            return self._mock_plan(state)
        return self._llm_plan(state, purpose="plan")

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

    # ── LLM planning (real backends) ────────────────────────────────────────────

    def _llm_plan(self, state: AgentState, *, purpose: str) -> list[Step]:
        """Ask vita to run a planning completion and convert it to steps.

        Sends an ``LlmRequest`` frame and blocks (bounded by
        ``LLM_REPLY_TIMEOUT_S``) for the matching ``LlmResponse``, reusing
        ``_recv_until`` so control messages and socket closes are handled
        exactly as in ``_act``.  The response is parsed by ``_parse_llm_plan``.

        Raises ``ValueError`` (converted to a ``CortexError`` by ``run``) on a
        malformed or empty response.
        """
        request_id = f"llm-{purpose}-{state.tool_calls_made}-{state.task_id}"
        send_message(self._sock, {
            "type": "LlmRequest",
            "request_id": request_id,
            "backend": self._backend,
            "purpose": purpose,
            "description": state.description,
            "tools": state.available_tools,
            "identity": state.identity,
        })

        def _is_response(msg: dict[str, Any]) -> bool:
            # request_id is echoed when present; if vita omits it, accept any
            # LlmResponse so older peers remain compatible.
            if msg.get("type") != "LlmResponse":
                return False
            rid = msg.get("request_id")
            return rid is None or rid == request_id

        msg = self._recv_until(
            _is_response,
            timeout_s=self.LLM_REPLY_TIMEOUT_S,
            what=f"LlmResponse to {request_id!r}",
        )
        return self._parse_llm_plan(state, msg)

    def _parse_llm_plan(
        self, state: AgentState, response: dict[str, Any]
    ) -> list[Step]:
        """Parse an ``LlmResponse`` into a validated ``list[Step]``.

        The response carries the plan in one of two shapes:

        * ``plan`` — a JSON list of ``{tool_name, args, description}`` objects,
          already structured.
        * ``content`` — a string the model returned; expected to be (or to
          contain) a JSON list with the same object shape.

        Each entry is validated and converted to a ``Step``.  ``args`` is
        normalised to a JSON string (objects are re-serialised).  Steps that
        reference a tool not in ``state.available_tools`` are skipped and
        recorded as an observation.

        Raises ``ValueError`` (→ ``CortexError``) when the response contains
        neither field, when the JSON is malformed, when it is not a list, or
        when no valid step survives validation.
        """
        raw_plan = response.get("plan")
        if raw_plan is None:
            content = response.get("content")
            if content is None:
                raise ValueError(
                    "LlmResponse missing both 'plan' and 'content' fields"
                )
            raw_plan = self._extract_plan_from_content(content)

        if not isinstance(raw_plan, list):
            raise ValueError(
                f"LLM plan must be a JSON list, got {type(raw_plan).__name__}"
            )

        tool_names = {t["name"] for t in state.available_tools}
        steps: list[Step] = []
        for index, entry in enumerate(raw_plan):
            if not isinstance(entry, dict):
                raise ValueError(
                    f"plan step {index} must be an object, got "
                    f"{type(entry).__name__}"
                )
            tool_name = entry.get("tool_name")
            if not isinstance(tool_name, str) or not tool_name:
                raise ValueError(
                    f"plan step {index} missing a string 'tool_name'"
                )
            if tool_name not in tool_names:
                # Unknown tools are skipped rather than aborting the whole
                # invocation: the model occasionally hallucinates a tool name.
                state.observations.append(
                    f"[plan] skipped step {index}: unknown tool {tool_name!r}"
                )
                continue
            steps.append(Step(
                tool_name=tool_name,
                args=self._normalise_args(entry.get("args", "{}")),
                description=str(entry.get("description", "")),
            ))

        if not steps:
            # An entirely unusable plan (no recognised tools) is a hard error;
            # silently completing with no actions would mask a broken backend.
            raise ValueError(
                "LLM plan contained no steps referencing available tools"
            )
        return steps

    @staticmethod
    def _extract_plan_from_content(content: str) -> Any:
        """Decode a model ``content`` string into a plan list.

        Accepts either a bare JSON array or one embedded in surrounding prose
        (e.g. fenced in markdown), extracting the first ``[...]`` span as a
        last resort.  Raises ``ValueError`` on unparseable input.
        """
        if not isinstance(content, str):
            raise ValueError(
                f"LlmResponse 'content' must be a string, got "
                f"{type(content).__name__}"
            )
        text = content.strip()
        try:
            return json.loads(text)
        except json.JSONDecodeError:
            pass
        # Defensive fallback: pull the first bracketed array out of prose.
        start = text.find("[")
        end = text.rfind("]")
        if start != -1 and end != -1 and end > start:
            try:
                return json.loads(text[start:end + 1])
            except json.JSONDecodeError as exc:
                raise ValueError(
                    f"LLM content did not contain valid JSON plan: {exc}"
                ) from exc
        raise ValueError("LLM content did not contain a JSON plan array")

    @staticmethod
    def _normalise_args(args: Any) -> str:
        """Coerce a step's ``args`` to a JSON string.

        A dict/list is re-serialised; a string is validated as JSON (and
        passed through) so downstream ``ToolCall`` consumers always receive a
        JSON document.  ``None`` becomes an empty object ``"{}"`` (the Rust side
        treats null args as ``{}``), never the JSON string ``"null"`` which
        would break tool-argument parsing.  Raises ``ValueError`` on a non-JSON
        string.
        """
        if args is None:
            return "{}"
        if isinstance(args, str):
            try:
                json.loads(args)
            except json.JSONDecodeError as exc:
                raise ValueError(
                    f"step 'args' is not valid JSON: {exc}"
                ) from exc
            return args
        return json.dumps(args, separators=(",", ":"))

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
        def _is_response(msg: dict[str, Any]) -> bool:
            return (
                msg.get("type") == "ToolResponse"
                and msg.get("call_id") == call_id
            )

        msg = self._recv_until(
            _is_response,
            timeout_s=self.TOOL_REPLY_TIMEOUT_S,
            what=f"ToolResponse to {call_id!r}",
        )
        if msg.get("error"):
            return f"[error: {msg['error']}]"
        return msg.get("result", "")

    # ── IPC receive helper ──────────────────────────────────────────────────────

    def _recv_until(
        self,
        match: "Callable[[dict[str, Any]], bool]",
        *,
        timeout_s: float,
        what: str,
    ) -> dict[str, Any]:
        """Block until a message satisfying *match* arrives, or fail.

        Shared by ``_act`` (waiting for ``ToolResponse``) and the LLM planning
        path (waiting for ``LlmResponse``).  The wait is bounded by a monotonic
        deadline so a stalled or silent vita cannot wedge the cortex forever
        (M2).  A ``Shutdown``/``Cancel``/``CortexError`` control message aborts
        the invocation; a closed socket raises ``RuntimeError``; other message
        types are ignored (forward-compatible).

        *what* is a human-readable description of the awaited message used in
        timeout/error messages.
        """
        deadline = time.monotonic() + timeout_s
        while True:
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                raise TimeoutError(
                    f"timed out waiting for {what} after {timeout_s:.0f}s"
                )
            self._sock.settimeout(remaining)
            try:
                msg = recv_message(self._sock)
            except (socket.timeout, TimeoutError) as exc:
                raise TimeoutError(
                    f"timed out waiting for {what} after {timeout_s:.0f}s"
                ) from exc
            finally:
                self._sock.settimeout(None)

            if msg is None:
                raise RuntimeError("vita closed the IPC socket unexpectedly")
            if match(msg):
                return msg
            if msg.get("type", "") in ("Shutdown", "Cancel", "CortexError"):
                raise RuntimeError(
                    f"invocation aborted by vita control message: "
                    f"{msg.get('type')}"
                )
            # Ignore other unexpected messages (future protocol extensions).

    def _observe(self, state: AgentState, step: Step, result: str) -> None:
        """Record the tool result as an observation."""
        state.observations.append(
            f"[{step.tool_name}] {step.description} → {result!r}"
        )

    def _revise(self, state: AgentState) -> None:
        """Optionally revise the remaining plan in light of observations.

        The mock backend is a no-op (keeps the test surface hermetic).  Real
        backends send an ``LlmRequest`` with ``purpose="revise"`` carrying the
        latest observations and the remaining plan; if vita returns a usable
        ``LlmResponse`` plan it replaces ``state.plan``.

        Revision is conservative:

        * It is skipped when there is no remaining plan (nothing to revise) or
          when the tool-call budget is already exhausted.
        * The revised plan is truncated so the *total* number of tool calls
          cannot exceed ``MAX_TOOL_CALLS``.
        * Any failure to obtain a usable revised plan leaves the existing plan
          untouched (revision is best-effort, not load-bearing).
        """
        if self._backend == "mock":
            return
        if not state.plan:
            return
        remaining_budget = self.MAX_TOOL_CALLS - state.tool_calls_made
        if remaining_budget <= 0:
            return

        request_id = f"llm-revise-{state.tool_calls_made}-{state.task_id}"
        send_message(self._sock, {
            "type": "LlmRequest",
            "request_id": request_id,
            "backend": self._backend,
            "purpose": "revise",
            "description": state.description,
            "tools": state.available_tools,
            "identity": state.identity,
            "observations": list(state.observations),
            "remaining_plan": [
                {
                    "tool_name": s.tool_name,
                    "args": s.args,
                    "description": s.description,
                }
                for s in state.plan
            ],
        })

        def _is_response(msg: dict[str, Any]) -> bool:
            if msg.get("type") != "LlmResponse":
                return False
            rid = msg.get("request_id")
            return rid is None or rid == request_id

        msg = self._recv_until(
            _is_response,
            timeout_s=self.LLM_REPLY_TIMEOUT_S,
            what=f"LlmResponse to {request_id!r}",
        )

        # When vita has no revision to offer it may omit both fields; treat that
        # as "keep the current plan" rather than an error.
        if msg.get("plan") is None and msg.get("content") is None:
            return
        try:
            revised = self._parse_llm_plan(state, msg)
        except ValueError as exc:
            # A bad revision must not abort an otherwise-healthy invocation.
            state.observations.append(f"[revise] ignored bad revision: {exc}")
            return
        # Never let revision blow the tool-call budget.
        state.plan = revised[:remaining_budget]

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
