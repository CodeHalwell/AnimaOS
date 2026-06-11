"""Unsloth sanity check — confirms the trainer container can see the GPU and
import the Unsloth toolchain.  Run from compose with:

    docker compose --profile training run --rm trainer python /app/check.py

The real fine-tuning loop lives in `sleep_phase.py`: it consumes the corpus
the agent's PolicyCompilation sleep phase writes to the shared volume, runs
a QLoRA pass with Unsloth, and exports a GGUF + Modelfile back into the
`ollama-models` volume.  This script only proves the runtime environment is
correctly wired — run it first on a new GPU host.
"""

from __future__ import annotations

import os
import sys


def main() -> int:
    print("anima-trainer: environment check")
    print(f"  base model             : {os.environ.get('TRAINER_BASE_MODEL', '<unset>')}")

    try:
        import torch
    except ImportError as e:  # pragma: no cover - environment-only
        print(f"  ✗ torch import failed: {e}", file=sys.stderr)
        return 1

    print(f"  torch                  : {torch.__version__}")
    print(f"  cuda available         : {torch.cuda.is_available()}")
    if not torch.cuda.is_available():
        print("  ✗ no CUDA device visible — check --gpus / nvidia-container-toolkit",
              file=sys.stderr)
        return 2

    device_count = torch.cuda.device_count()
    print(f"  cuda device count      : {device_count}")
    for idx in range(device_count):
        props = torch.cuda.get_device_properties(idx)
        vram_gb = props.total_memory / (1024 ** 3)
        print(f"  device[{idx}]              : {props.name} "
              f"(sm_{props.major}{props.minor}, {vram_gb:.1f} GB VRAM)")

    try:
        import unsloth  # noqa: F401
        from unsloth import FastLanguageModel  # noqa: F401
        print(f"  unsloth                : {unsloth.__version__}")
    except ImportError as e:  # pragma: no cover
        print(f"  ✗ unsloth import failed: {e}", file=sys.stderr)
        return 3

    try:
        import bitsandbytes as bnb
        print(f"  bitsandbytes           : {bnb.__version__}")
    except ImportError as e:  # pragma: no cover
        print(f"  ⚠ bitsandbytes import failed: {e}", file=sys.stderr)

    print("anima-trainer: ready")
    return 0


if __name__ == "__main__":
    sys.exit(main())
