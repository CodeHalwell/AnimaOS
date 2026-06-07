"""Hugging Face Transformers inference worker (E8 S8.2.2).

This script is the Python-side of the ``HfTransformersBackend``.  It loads a
model using the ``transformers`` library and streams tokens back to the Rust
caller through a Unix Domain Socket using the same **4-byte big-endian
length-prefix + JSON body** wire protocol as ``ipc.py``.

Wire protocol (same as ``ipc.py``)
-----------------------------------
Rust → worker messages
~~~~~~~~~~~~~~~~~~~~~~
``infer``
    Request a completion for ``prompt``.  Optional ``max_new_tokens`` cap.

Worker → Rust messages
~~~~~~~~~~~~~~~~~~~~~~
``token``
    A single generated token (``{"type": "token", "text": "<tok>"}``).

``done``
    Signals end-of-generation (``{"type": "done", "total_tokens": N}``).

``error``
    Unrecoverable fault (``{"type": "error", "message": "<msg>"}``).

Usage
-----
::

    python3 cortex/transformers_worker.py \\
        --socket /tmp/anima-hf-12345.sock \\
        --model microsoft/Phi-3.5-mini-instruct

Prerequisites
-------------
::

    pip install transformers accelerate torch

The script exits immediately after serving one ``infer`` request so the Rust
caller can manage the subprocess lifecycle (one process per invocation).  For
a persistent worker the caller would need to re-connect; the current design
keeps lifecycle management simple at the cost of per-call startup overhead.
"""

from __future__ import annotations

import argparse
import json
import socket
import struct
import sys
import traceback
from typing import Any


# ── Wire-protocol helpers ─────────────────────────────────────────────────────

def _recv_frame(conn: socket.socket) -> dict[str, Any]:
    """Read one length-prefixed JSON frame from *conn*."""
    header = _recv_exact(conn, 4)
    length = struct.unpack(">I", header)[0]
    body = _recv_exact(conn, length)
    return json.loads(body.decode("utf-8"))


def _send_frame(conn: socket.socket, msg: dict[str, Any]) -> None:
    """Write one length-prefixed JSON frame to *conn*."""
    body = json.dumps(msg).encode("utf-8")
    header = struct.pack(">I", len(body))
    conn.sendall(header + body)


def _recv_exact(conn: socket.socket, n: int) -> bytes:
    """Read exactly *n* bytes, blocking until all arrive."""
    buf = bytearray()
    while len(buf) < n:
        chunk = conn.recv(n - len(buf))
        if not chunk:
            raise EOFError("connection closed before all bytes arrived")
        buf.extend(chunk)
    return bytes(buf)


# ── Inference ─────────────────────────────────────────────────────────────────

def run_inference(conn: socket.socket, model_id: str, request: dict[str, Any]) -> None:
    """Load *model_id* and stream tokens for the given *request* to *conn*."""
    try:
        from transformers import AutoTokenizer, AutoModelForCausalLM  # type: ignore
        import torch  # type: ignore
    except ImportError as exc:
        _send_frame(conn, {
            "type": "error",
            "message": f"transformers/torch not installed: {exc}",
        })
        return

    prompt: str = request.get("prompt", "")
    max_new_tokens: int = int(request.get("max_new_tokens", 256))

    try:
        tokenizer = AutoTokenizer.from_pretrained(model_id)
        model = AutoModelForCausalLM.from_pretrained(
            model_id,
            torch_dtype=torch.float16 if torch.cuda.is_available() else torch.float32,
            device_map="auto",
        )
    except Exception as exc:
        _send_frame(conn, {"type": "error", "message": f"model load failed: {exc}"})
        return

    model.eval()
    inputs = tokenizer(prompt, return_tensors="pt").to(model.device)

    total = 0
    try:
        with torch.no_grad():
            past_key_values = None
            for _ in range(max_new_tokens):
                if past_key_values is None:
                    # First step: full forward pass over the prompt.
                    outputs = model(**inputs, use_cache=True)
                else:
                    # Subsequent steps: only the new token; KV cache covers the rest.
                    outputs = model(
                        input_ids=next_token_id.unsqueeze(0),
                        attention_mask=inputs.get("attention_mask"),
                        past_key_values=past_key_values,
                        use_cache=True,
                    )

                past_key_values = outputs.past_key_values
                next_token_id = outputs.logits[:, -1, :].argmax(dim=-1)
                # Use .item() to convert the tensor scalar to a Python int before
                # decoding — decode() is most reliable with a plain int or list.
                token_int = next_token_id[0].item()
                token_text = tokenizer.decode([token_int], skip_special_tokens=False)

                _send_frame(conn, {"type": "token", "text": token_text})
                total += 1

                # Guard against tokenizers that expose eos_token_id as None.
                eos_id = tokenizer.eos_token_id
                if eos_id is not None and token_int == eos_id:
                    break

                if "attention_mask" in inputs:
                    ones = torch.ones(
                        (1, 1), dtype=inputs["attention_mask"].dtype,
                        device=inputs["attention_mask"].device,
                    )
                    inputs["attention_mask"] = torch.cat(
                        [inputs["attention_mask"], ones], dim=1
                    )
    except Exception as exc:
        _send_frame(conn, {"type": "error", "message": f"generation error: {exc}"})
        return

    _send_frame(conn, {"type": "done", "total_tokens": total})


# ── Entry point ───────────────────────────────────────────────────────────────

def main() -> None:
    parser = argparse.ArgumentParser(
        description="HF Transformers UDS inference worker for AnimaOS."
    )
    parser.add_argument(
        "--socket",
        required=True,
        help="Path to the Unix Domain Socket to connect to (created by Rust caller).",
    )
    parser.add_argument(
        "--model",
        default="microsoft/Phi-3.5-mini-instruct",
        help="Hugging Face model ID to load (default: microsoft/Phi-3.5-mini-instruct).",
    )
    args = parser.parse_args()

    sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    try:
        sock.connect(args.socket)
    except OSError as exc:
        print(f"[transformers_worker] failed to connect to {args.socket}: {exc}", file=sys.stderr)
        sys.exit(1)

    try:
        request = _recv_frame(sock)
        if request.get("type") != "infer":
            _send_frame(sock, {
                "type": "error",
                "message": f"unexpected message type: {request.get('type')}",
            })
            return
        run_inference(sock, args.model, request)
    except Exception:
        try:
            _send_frame(sock, {
                "type": "error",
                "message": traceback.format_exc(),
            })
        except Exception:
            pass
    finally:
        sock.close()


if __name__ == "__main__":
    main()
