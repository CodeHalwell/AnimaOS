"""Entry-point for the AnimaOS cortex subprocess.

Usage::

    python -m cortex \\
        --socket /tmp/anima-cortex-<task_id>.sock \\
        --state-dir /var/lib/anima/<agent_id> \\
        [--backend mock|anthropic|openai]

The cortex process:
1. Connects to the UDS socket created by vita.
2. Reads a single ``InvokeRequest`` message.
3. Runs the Plan/Act/Observe/Revise loop (see ``agent_loop.py``).
4. Sends an ``InvokeComplete`` (or ``CortexError``) message.
5. Closes the socket and exits.

The process is intentionally short-lived: it is spawned once per invocation
and torn down immediately after the ``InvokeComplete`` message is sent.  Any
uncaught exception causes the process to exit with a non-zero status code,
which vita detects and records in the audit log as a ``CortexFault`` entry.
"""

from __future__ import annotations

import argparse
import socket
import sys
from pathlib import Path

from .agent_loop import AgentLoop
from .identity_memory import IdentityMemory
from .ipc import recv_message, send_message


def _parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        prog="python -m cortex",
        description="AnimaOS cortex subprocess",
    )
    parser.add_argument(
        "--socket",
        required=True,
        help="Path to the Unix Domain Socket created by vita",
    )
    parser.add_argument(
        "--state-dir",
        required=True,
        help="Agent state directory (holds identity.json, etc.)",
    )
    parser.add_argument(
        "--backend",
        default="mock",
        choices=("mock", "anthropic", "openai"),
        help="LLM backend to use for planning (default: mock)",
    )
    return parser.parse_args()


def main() -> int:
    args = _parse_args()

    # Connect to vita's UDS socket FIRST, before any fallible work. vita blocks
    # on accept() with a connect deadline; if we died before connecting (e.g. a
    # corrupt identity.json raising ValueError) it would observe a dead child,
    # but connecting first lets us report load failures as a CortexError frame
    # instead of an opaque non-zero exit (H2).
    sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    try:
        sock.connect(args.socket)
    except OSError as exc:
        print(f"[cortex] failed to connect to vita socket: {exc}", file=sys.stderr)
        return 1

    try:
        # Load identity memory now that the socket is open, so a corrupt file
        # surfaces as a CortexError rather than a pre-connect death.
        state_dir = Path(args.state_dir)
        identity_path = state_dir / "identity.json"
        identity = IdentityMemory(identity_path)
        try:
            identity.load()
        except Exception as exc:  # pylint: disable=broad-except
            send_message(sock, {
                "type": "CortexError",
                "message": f"identity load failed: {exc}",
            })
            print(f"[cortex] identity load failed: {exc}", file=sys.stderr)
            return 1

        # Receive the InvokeRequest.
        request = recv_message(sock)
        if request is None:
            print("[cortex] vita closed socket before sending InvokeRequest", file=sys.stderr)
            return 1

        if request.get("type") != "InvokeRequest":
            print(f"[cortex] unexpected first message type: {request.get('type')}", file=sys.stderr)
            return 1

        # Merge loaded identity into the request (request takes precedence).
        if "identity" not in request or not request["identity"]:
            request["identity"] = identity.as_dict()

        # Run the PAOR loop.
        loop = AgentLoop(sock, backend=args.backend)
        loop.run(request)

    finally:
        sock.close()

    return 0


if __name__ == "__main__":
    sys.exit(main())
