"""Length-prefixed JSON protocol over Unix Domain Sockets.

Wire format
-----------
Each message is encoded as::

    ┌─────────────────────────────┐
    │  4-byte big-endian uint32   │  ← byte count of the JSON body
    │  JSON body (UTF-8)          │  ← the message dict
    └─────────────────────────────┘

All messages carry a ``"type"`` field that disambiguates the payload.

vita → cortex messages
~~~~~~~~~~~~~~~~~~~~~~
``InvokeRequest``
    Sent once at the start of each invocation.

``ToolResponse``
    Reply to a ``ToolCall`` emitted by the cortex.

``LlmResponse``
    Reply to an ``LlmRequest`` emitted by the cortex.  ``done=True``
    signals that the full response has been streamed.

cortex → vita messages
~~~~~~~~~~~~~~~~~~~~~~
``ToolCall``
    Request vita to dispatch a named tool through praxis.

``LlmRequest``
    Request vita to forward a completion request to the active LLM
    backend.  (Optional in the MVP — the mock loop produces its own
    outputs without going to vita for LLM completions.)

``InvokeComplete``
    Final message: output text + episode summary + tool-call count.

``CortexError``
    Unrecoverable fault inside the cortex — vita logs it and the
    invocation is aborted.
"""

from __future__ import annotations

import json
import socket
import struct
from typing import Any

_LENGTH_PREFIX_FMT = ">I"  # big-endian unsigned 32-bit
_LENGTH_PREFIX_SIZE = 4


# ── Low-level helpers ──────────────────────────────────────────────────────────

def _recv_exactly(sock: socket.socket, n: int) -> bytes | None:
    """Read exactly *n* bytes from *sock*, returning ``None`` on EOF."""
    buf = bytearray()
    while len(buf) < n:
        chunk = sock.recv(n - len(buf))
        if not chunk:
            return None
        buf.extend(chunk)
    return bytes(buf)


# ── Public API ─────────────────────────────────────────────────────────────────

def recv_message(sock: socket.socket) -> dict[str, Any] | None:
    """Receive one length-prefixed JSON message from *sock*.

    Returns ``None`` when the peer has closed the connection.
    Raises ``ValueError`` for malformed JSON or length overflow.
    """
    header = _recv_exactly(sock, _LENGTH_PREFIX_SIZE)
    if header is None:
        return None

    (length,) = struct.unpack(_LENGTH_PREFIX_FMT, header)
    if length > 64 * 1024 * 1024:  # sanity cap: 64 MiB
        raise ValueError(f"IPC message too large: {length} bytes")

    body = _recv_exactly(sock, length)
    if body is None:
        return None

    return json.loads(body.decode("utf-8"))


def send_message(sock: socket.socket, msg: dict[str, Any]) -> None:
    """Send *msg* as a length-prefixed JSON frame on *sock*."""
    body = json.dumps(msg, separators=(",", ":")).encode("utf-8")
    header = struct.pack(_LENGTH_PREFIX_FMT, len(body))
    sock.sendall(header + body)
