"""AnimaOS Unsloth adaptation entrypoint (E8 S8.4.5 / S8.4.6).

This is the Python side of the feature-gated ``UnslothFineTuner`` skeleton in
``crates/finetune/src/backend/unsloth.rs``.  The Rust ``live`` backend serialises
a *job spec* to JSON, spawns::

    python3 cortex/finetune_entrypoint.py <job_spec_path> <out_dir>

…and parses the *training result* JSON this script prints on **stdout**.

Two modes
=========
* **Real mode** — if ``torch`` and ``unsloth`` import successfully, the clearly
  marked ``TODO`` sections below are where the real S8.4.5 LoRA/QLoRA/HRA
  adaptation and the S8.4.6 merge + GGUF quantisation calls go.  Until those are
  wired, real mode is *not* reached on a box without those packages.
* **Stub mode** — if ``torch`` / ``unsloth`` are *not* importable (CI, dev
  laptop, no GPU), this script writes deterministic placeholder artifacts into
  ``<out_dir>`` and prints a well-formed training-result JSON.  This lets the
  Rust <-> Python contract be exercised end-to-end with no GPU and no network.

Determinism
===========
Stub-mode ``adapter_id`` / ``weights_digest`` are derived from a stable hash of
the job spec, so the same inputs always yield the same output (mirroring the
Rust ``FixtureFineTuner``).

----------------------------------------------------------------------------
Rust <-> Python contract (keep in sync with ``backend/unsloth.rs``)
----------------------------------------------------------------------------
Job spec (Rust -> Python), JSON file at argv[1]::

    {
      "version": 1,
      "config": <FineTuneConfig as serde JSON>,   # base_model, method{kind,..},
                                                   # dataset_ref, output_adapter_id,
                                                   # description, hyperparams{..}
      "pairs":  [ {"prompt": "...", "response": "..."}, ... ]
    }

Training result (Python -> Rust), single JSON object on the LAST non-empty
stdout line (progress lines may precede it)::

    {
      "adapter_id":      "string",   # stable id of the produced adapter (non-empty)
      "description":     "string",   # domain/description for task->adapter select
      "format":          "lora_adapter" | "structural_transform" | "baked_gguf",
      "merge_path":      "clean" | "hadamard",
      "serving_tier":    "mountable_adapter" | "baked_variant",
      "weights_digest":  "string",   # digest of the adapter weights on disk
      "adapter_path":    "string",   # filesystem path to the adapter (non-empty)
      "merged_gguf_path":"string" | null,   # merged GGUF (baked variants), or null
      "provenance": {
        "base_model":    "string",
        "method":        <AdaptationMethod as serde JSON>,  # echoed from config
        "source_job":    "string",
        "created_at_ns":  <u64>
      },
      "metrics": { ... }             # optional free-form; Rust captures, ignores
    }

The ``format`` / ``merge_path`` / ``serving_tier`` mapping mirrors the S8.4.8
two-tier map in ``crates/finetune/src/{artifact,method}.rs``.
"""

from __future__ import annotations

import hashlib
import json
import os
import sys
import time

# Contract schema version; must match JOB_SPEC_VERSION in backend/unsloth.rs.
JOB_SPEC_VERSION = 1


def _log(msg: str) -> None:
    """Human-readable progress goes to stderr (and stdout progress lines are
    tolerated by the Rust parser, which only reads the last non-empty line)."""
    print(f"[finetune_entrypoint] {msg}", file=sys.stderr)


def _backend_available() -> bool:
    """Whether the real GPU training stack is importable."""
    try:
        import torch  # noqa: F401
        import unsloth  # noqa: F401
    except Exception:
        return False
    return True


def _stable_digest(payload: bytes, salt: str) -> str:
    h = hashlib.sha256()
    h.update(salt.encode("utf-8"))
    h.update(payload)
    return h.hexdigest()[:16]


def _method_kind(method: dict) -> str:
    # serde tags AdaptationMethod with `kind` (snake_case): "lora", "q_lora",
    # "hra", "full_fine_tune".
    return method.get("kind", "q_lora") if isinstance(method, dict) else "q_lora"


def _hra_family(method: dict) -> str:
    return method.get("family", "") if isinstance(method, dict) else ""


def _format_for_method(method: dict) -> str:
    """Mirror ``AdapterFormat::for_method`` (artifact.rs)."""
    kind = _method_kind(method)
    if kind in ("lora", "q_lora"):
        return "lora_adapter"
    if kind == "hra":
        family = _hra_family(method)
        if family == "hrp":
            return "lora_adapter"
        if family == "hyper_adapt":
            return "structural_transform"
        # ohora, hira, boha
        return "baked_gguf"
    # full_fine_tune
    return "baked_gguf"


def _merge_path_for_method(method: dict) -> str:
    """Mirror ``AdaptationMethod::merge_path`` (method.rs)."""
    kind = _method_kind(method)
    if kind == "hra" and _hra_family(method) in ("hira", "boha"):
        return "hadamard"
    return "clean"


def _serving_tier_for_method(method: dict) -> str:
    """Mirror ``AdaptationMethod::serving_tier`` (method.rs)."""
    kind = _method_kind(method)
    if kind in ("lora", "q_lora"):
        return "mountable_adapter"
    if kind == "hra":
        if _hra_family(method) in ("hrp", "hyper_adapt"):
            return "mountable_adapter"
        return "baked_variant"
    return "baked_variant"


def _read_job_spec(path: str) -> dict:
    with open(path, "r", encoding="utf-8") as f:
        spec = json.load(f)
    version = spec.get("version")
    if version != JOB_SPEC_VERSION:
        raise ValueError(
            f"unsupported job spec version {version!r} "
            f"(this entrypoint expects {JOB_SPEC_VERSION})"
        )
    if not isinstance(spec.get("config"), dict):
        raise ValueError("job spec is missing a `config` object")
    if not isinstance(spec.get("pairs"), list):
        raise ValueError("job spec is missing a `pairs` array")
    return spec


def run_stub(spec: dict, out_dir: str) -> dict:
    """Write deterministic placeholder artifacts and return a result dict.

    Used when torch/unsloth are unavailable so the contract is exercisable
    without a GPU. The result mirrors what real mode must ultimately produce.
    """
    config = spec["config"]
    pairs = spec["pairs"]
    method = config.get("method", {})

    os.makedirs(out_dir, exist_ok=True)
    adapter_dir = os.path.join(out_dir, "adapter")
    os.makedirs(adapter_dir, exist_ok=True)

    # Deterministic id/digest from a canonical encoding of the job spec.
    canonical = json.dumps(spec, sort_keys=True, separators=(",", ":")).encode("utf-8")
    fingerprint = _stable_digest(canonical, "anima-finetune.stub.v1")
    weights_digest = _stable_digest(canonical, "anima-finetune.stub.weights.v1")

    output_adapter_id = config.get("output_adapter_id", "adapter")
    adapter_id = f"{output_adapter_id}-{fingerprint}"

    # Write a placeholder adapter weights file so adapter_path points at real
    # on-disk artifacts, exactly as real mode would.
    weights_path = os.path.join(adapter_dir, "adapter_model.safetensors.stub")
    with open(weights_path, "w", encoding="utf-8") as f:
        f.write(
            "STUB adapter weights (no torch/unsloth available)\n"
            f"adapter_id={adapter_id}\n"
            f"weights_digest={weights_digest}\n"
            f"num_pairs={len(pairs)}\n"
        )
    with open(os.path.join(adapter_dir, "adapter_config.json"), "w", encoding="utf-8") as f:
        json.dump({"method": method, "stub": True}, f)

    serving_tier = _serving_tier_for_method(method)
    fmt = _format_for_method(method)

    # Baked variants would emit a merged GGUF; in stub mode write a placeholder.
    merged_gguf_path = None
    if serving_tier == "baked_variant":
        merged_gguf_path = os.path.join(out_dir, "merged.gguf.stub")
        with open(merged_gguf_path, "w", encoding="utf-8") as f:
            f.write("STUB merged GGUF (no torch/unsloth available)\n")

    _log(f"stub mode: wrote {weights_path}")

    return {
        "adapter_id": adapter_id,
        "description": config.get("description", ""),
        "format": fmt,
        "merge_path": _merge_path_for_method(method),
        "serving_tier": serving_tier,
        "weights_digest": weights_digest,
        "adapter_path": adapter_dir,
        "merged_gguf_path": merged_gguf_path,
        "provenance": {
            "base_model": config.get("base_model", ""),
            # Echo the method back verbatim so it round-trips through serde.
            "method": method,
            "source_job": output_adapter_id,
            "created_at_ns": time.time_ns(),
        },
        "metrics": {
            "stub": True,
            "num_pairs": len(pairs),
        },
    }


def run_real(spec: dict, out_dir: str) -> dict:
    """Real S8.4.5 adaptation + S8.4.6 merge/quant pipeline.

    Reached only when torch + unsloth are importable. The structure mirrors the
    stub result so the Rust contract is identical regardless of mode.
    """
    config = spec["config"]
    method = config.get("method", {})
    os.makedirs(out_dir, exist_ok=True)
    adapter_dir = os.path.join(out_dir, "adapter")
    os.makedirs(adapter_dir, exist_ok=True)

    # === S8.4.5 — adaptation =================================================
    # TODO(S8.4.5): Load the base model + tokenizer via unsloth
    #   (FastLanguageModel.from_pretrained), build the PEFT adapter for the
    #   selected method (LoRA / QLoRA / HRA family from `method`), construct the
    #   SFT dataset from spec["pairs"], run the SFTTrainer with config
    #   hyperparams (max_steps, learning_rate, batch_size), and save the adapter
    #   into `adapter_dir`.
    #
    # === S8.4.6 — merge + quantisation =======================================
    # TODO(S8.4.6): Depending on serving tier, either keep the LoRA-format
    #   adapter mountable, or for baked variants materialise ΔW (clean broadcast
    #   vs Hadamard dequant->merge->requant), merge into the base, and export +
    #   quantise to a merged GGUF, setting `merged_gguf_path`.
    #
    # TODO: compute `weights_digest` from the actual saved adapter weights.
    raise NotImplementedError(
        "real Unsloth/PEFT training is not wired yet; see S8.4.5/6 TODOs"
    )


def main(argv: list[str]) -> int:
    if len(argv) != 3:
        _log(f"usage: {argv[0]} <job_spec_path> <out_dir>")
        return 2

    spec_path, out_dir = argv[1], argv[2]
    try:
        spec = _read_job_spec(spec_path)
    except Exception as exc:
        _log(f"failed to read job spec {spec_path}: {exc!r}")
        return 1

    try:
        if _backend_available():
            _log("torch + unsloth available: running real pipeline")
            result = run_real(spec, out_dir)
        else:
            _log("torch/unsloth unavailable: running deterministic stub pipeline")
            result = run_stub(spec, out_dir)
    except Exception as exc:
        _log(f"training failed: {exc!r}")
        return 1

    # The Rust parser reads the LAST non-empty stdout line as the result JSON.
    print(json.dumps(result))
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
