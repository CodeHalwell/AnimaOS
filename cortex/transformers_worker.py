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
    if length > 64 * 1024 * 1024:  # sanity cap: 64 MiB (mirrors ipc.py) — INF-7
        raise ValueError(f"frame too large: {length} bytes")
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

    # Upper bound on generated tokens regardless of the request, to bound work
    # and memory on a per-call basis (L3).
    MAX_NEW_TOKENS_CAP = 4096
    prompt: str = request.get("prompt", "")
    try:
        max_new_tokens = int(request.get("max_new_tokens", 256))
    except (TypeError, ValueError):
        _send_frame(conn, {
            "type": "error",
            "message": f"invalid max_new_tokens: {request.get('max_new_tokens')!r}",
        })
        return
    max_new_tokens = max(1, min(max_new_tokens, MAX_NEW_TOKENS_CAP))

    # Optional sampling parameters (L4). temperature<=0 means greedy/argmax.
    try:
        temperature = float(request.get("temperature", 0.0))
        top_p = float(request.get("top_p", 1.0))
        top_k = int(request.get("top_k", 0))
    except (TypeError, ValueError) as exc:
        _send_frame(conn, {"type": "error", "message": f"invalid sampling params: {exc}"})
        return
    if temperature < 0.0:
        _send_frame(conn, {"type": "error", "message": "temperature must be non-negative"})
        return
    if not 0.0 <= top_p <= 1.0:
        _send_frame(conn, {"type": "error", "message": "top_p must be between 0.0 and 1.0"})
        return
    if top_k < 0:
        _send_frame(conn, {"type": "error", "message": "top_k must be non-negative"})
        return

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

    # Truncate the prompt to the model's context window, leaving room for the
    # generated tokens, so an oversized prompt cannot OOM or hang (L3). Also
    # cap max_new_tokens to the window so prompt + generated never exceeds it
    # (which would crash models with absolute position embeddings).
    max_ctx = getattr(model.config, "max_position_embeddings", None) or 4096
    max_new_tokens = max(1, min(max_new_tokens, max_ctx - 1))
    max_prompt_tokens = max(1, max_ctx - max_new_tokens)
    inputs = tokenizer(
        prompt,
        return_tensors="pt",
        truncation=True,
        max_length=max_prompt_tokens,
    ).to(model.device)

    # Stop on any of the model's declared EOS ids, not just the tokenizer's
    # single eos_token_id — instruct models (e.g. Phi-3.5) end turns on a
    # different id (<|end|>) declared in generation_config (M1).
    eos_ids: set[int] = set()
    if tokenizer.eos_token_id is not None:
        eos_ids.add(int(tokenizer.eos_token_id))
    gen_cfg = getattr(model, "generation_config", None)
    cfg_eos = getattr(gen_cfg, "eos_token_id", None) if gen_cfg is not None else None
    if isinstance(cfg_eos, int):
        eos_ids.add(cfg_eos)
    elif isinstance(cfg_eos, (list, tuple)):
        eos_ids.update(int(x) for x in cfg_eos)

    def _pick_next(logits: "torch.Tensor") -> int:
        """Select the next token id from the final-position logits."""
        if temperature <= 0.0:
            return int(logits.argmax(dim=-1)[0].item())
        scaled = logits / temperature
        if top_k > 0:
            kth = torch.topk(scaled, min(top_k, scaled.shape[-1]), dim=-1).values[..., -1, None]
            scaled = scaled.masked_fill(scaled < kth, float("-inf"))
        probs = torch.softmax(scaled, dim=-1)
        if 0.0 < top_p < 1.0:
            sorted_probs, sorted_idx = torch.sort(probs, descending=True, dim=-1)
            cumulative = torch.cumsum(sorted_probs, dim=-1)
            mask = cumulative - sorted_probs > top_p
            sorted_probs = sorted_probs.masked_fill(mask, 0.0)
            probs = torch.zeros_like(probs).scatter_(-1, sorted_idx, sorted_probs)
            probs = probs / probs.sum(dim=-1, keepdim=True)
        return int(torch.multinomial(probs, num_samples=1)[0].item())

    total = 0
    # Decode cumulatively and emit only the newly-appended suffix each step.
    # Decoding tokens one-at-a-time corrupts output: SentencePiece strips the
    # leading-space marker on isolated pieces and multi-byte chars split across
    # tokens decode to U+FFFD per fragment (H3).
    generated_ids: list[int] = []
    prev_text = ""
    try:
        with torch.no_grad():
            past_key_values = None
            next_input = inputs["input_ids"]
            attention_mask = inputs.get("attention_mask")
            for _ in range(max_new_tokens):
                if past_key_values is None:
                    outputs = model(
                        input_ids=next_input,
                        attention_mask=attention_mask,
                        use_cache=True,
                    )
                else:
                    outputs = model(
                        input_ids=next_input,
                        attention_mask=attention_mask,
                        past_key_values=past_key_values,
                        use_cache=True,
                    )

                past_key_values = outputs.past_key_values
                token_int = _pick_next(outputs.logits[:, -1, :])
                generated_ids.append(token_int)
                total += 1

                # Stop before surfacing the EOS token's text.
                if token_int in eos_ids:
                    break

                full_text = tokenizer.decode(generated_ids, skip_special_tokens=True)
                delta = full_text[len(prev_text):]
                prev_text = full_text
                if delta:
                    _send_frame(conn, {"type": "token", "text": delta})

                next_input = torch.tensor([[token_int]], device=model.device)
                if attention_mask is not None:
                    ones = torch.ones(
                        (1, 1), dtype=attention_mask.dtype, device=attention_mask.device,
                    )
                    attention_mask = torch.cat([attention_mask, ones], dim=1)
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
        except Exception as send_exc:
            # The peer is likely gone (closed socket / broken pipe) during
            # shutdown, so we cannot deliver the error frame. Don't re-raise
            # from cleanup, but make the failure observable rather than
            # silently swallowing it.
            print(
                f"[transformers_worker] failed to send error frame during "
                f"shutdown: {send_exc!r}",
                file=sys.stderr,
            )
    finally:
        sock.close()


if __name__ == "__main__":
    main()
