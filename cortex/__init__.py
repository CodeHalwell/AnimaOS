"""AnimaOS Cortex — deliberative cognitive layer.

This package implements the Python-side of the E5.1 Cortex MVP.  The cortex
is a short-lived subprocess spawned by ``vita`` for each deliberative task
invocation.  It communicates with vita over a Unix Domain Socket using a
length-prefixed JSON protocol.

Lifecycle (per invocation):
1. vita creates a UDS socket and spawns ``python -m cortex`` with the path.
2. The cortex connects, receives an ``InvokeRequest`` from vita.
3. The cortex runs the plan / act / observe / revise loop.
   - Tool calls are routed back to vita, which dispatches them via ``praxis``.
   - LLM completions are either sourced locally (mock) or via vita's proxy.
4. On completion the cortex sends an ``InvokeComplete`` message carrying the
   output text and the episode summary.
5. vita writes the episode summary into the L3 archive and tears down the
   cortex process.

Public entry-point: ``cortex/__main__.py`` (``python -m cortex …``).
"""
