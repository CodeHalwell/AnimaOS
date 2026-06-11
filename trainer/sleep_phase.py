"""Sleep-phase fine-tuning — the loop trainer/check.py promises.

Consumes the training corpus that the agent's PolicyCompilation sleep phase
(E3.8, `memory::compilation`) writes under the shared volume, runs a QLoRA
pass with Unsloth, and exports the adapted weights so they round-trip back
into the inference stack:

    agent (vita sleep cycle)
      └─ training_corpus/{alpaca,conversation,chain_of_thought}.jsonl
           └─ sleep_phase.py  ──►  /models/adapters/<run-id>/
                ├── adapter/            LoRA weights (hot-mountable)
                ├── gguf/model.gguf     baked Q4_K_M variant (--gguf)
                ├── Modelfile           Ollama registration recipe
                └── manifest.json       provenance (anima-trainer-manifest/v1)

Run from compose (GPU host):

    docker compose --profile training run --rm trainer \
        python /app/sleep_phase.py --gguf

Validate the corpus and the wiring anywhere (no GPU, stdlib only):

    docker compose --profile training run --rm trainer \
        python /app/sleep_phase.py --dry-run

Register the baked variant with the sibling Ollama service afterwards:

    docker compose exec ollama \
        ollama create anima-adapted -f /models/adapters/<run-id>/Modelfile

The manifest's ``adapter_artifact`` block mirrors the Rust
``finetune::AdapterArtifact`` schema (adapter_id, format, weights_digest,
provenance{base_model, method, source_job, created_at_ns}) so the E8 S8.4.8
adapter library can ingest these runs when its live wiring lands.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import sys
import time
from pathlib import Path

# ── Corpus loading (stdlib only — shared by dry-run and live paths) ──────────

CORPUS_FILES = ("alpaca.jsonl", "conversation.jsonl", "chain_of_thought.jsonl")


def _iter_jsonl(path: Path):
    with path.open(encoding="utf-8") as fh:
        for lineno, line in enumerate(fh, 1):
            line = line.strip()
            if not line:
                continue
            try:
                yield json.loads(line)
            except json.JSONDecodeError as e:
                print(f"  ! {path.name}:{lineno}: skipping malformed line ({e})")


def load_corpus(corpus_dir: Path, cot_in_response: bool) -> tuple[list[dict], dict]:
    """Read every corpus format into unified {prompt, response} pairs.

    Returns (pairs, per-format counts). Duplicate (prompt, response) pairs —
    the same task compiled into several formats, or accumulated across sleep
    cycles with `append: true` — are collapsed.
    """
    pairs: list[dict] = []
    counts = {name: 0 for name in CORPUS_FILES}

    alpaca = corpus_dir / "alpaca.jsonl"
    if alpaca.exists():
        for rec in _iter_jsonl(alpaca):
            prompt = (rec.get("instruction") or "").strip()
            extra = (rec.get("input") or "").strip()
            if extra:
                prompt = f"{prompt}\n\n{extra}"
            response = (rec.get("output") or "").strip()
            if prompt and response:
                pairs.append({"prompt": prompt, "response": response})
                counts["alpaca.jsonl"] += 1

    convo = corpus_dir / "conversation.jsonl"
    if convo.exists():
        # memory::compilation emits ShareGPT-style roles "human"/"gpt"
        # (compilation.rs `to_conversation_record`); accept the OpenAI-style
        # names too so hand-curated corpora also load.
        user_roles = {"human", "user"}
        agent_roles = {"gpt", "assistant"}
        for rec in _iter_jsonl(convo):
            turns = rec.get("conversations") or []
            # Pair each user turn with the agent turn that follows it.
            for i in range(len(turns) - 1):
                if turns[i].get("role") in user_roles and turns[i + 1].get("role") in agent_roles:
                    prompt = (turns[i].get("content") or "").strip()
                    response = (turns[i + 1].get("content") or "").strip()
                    if prompt and response:
                        pairs.append({"prompt": prompt, "response": response})
                        counts["conversation.jsonl"] += 1

    cot = corpus_dir / "chain_of_thought.jsonl"
    if cot.exists():
        for rec in _iter_jsonl(cot):
            prompt = (rec.get("prompt") or "").strip()
            answer = (rec.get("answer") or "").strip()
            chain = (rec.get("chain_of_thought") or "").strip()
            response = f"{chain}\n\n{answer}".strip() if (cot_in_response and chain) else answer
            if prompt and response:
                pairs.append({"prompt": prompt, "response": response})
                counts["chain_of_thought.jsonl"] += 1

    seen: set[tuple[str, str]] = set()
    unique: list[dict] = []
    for p in pairs:
        key = (p["prompt"], p["response"])
        if key not in seen:
            seen.add(key)
            unique.append(p)
    return unique, counts


def corpus_digest(corpus_dir: Path) -> str:
    """A stable sha256 over the corpus bytes, for provenance."""
    h = hashlib.sha256()
    for name in CORPUS_FILES:
        path = corpus_dir / name
        if path.exists():
            h.update(name.encode())
            h.update(path.read_bytes())
    return h.hexdigest()


# ── Manifest (provenance for the adapter library) ─────────────────────────────


def write_manifest(out_dir: Path, args, pairs_n: int, counts: dict, digest: str,
                   produced: dict) -> Path:
    created_ns = time.time_ns()
    adapter_id = hashlib.sha256(
        f"{args.base}|{digest}|{args.rank}|{args.alpha}|{args.epochs}".encode()
    ).hexdigest()[:16]
    manifest = {
        "schema": "anima-trainer-manifest/v1",
        "run": {
            "dry_run": args.dry_run,
            "base_model": args.base,
            "corpus_dir": str(args.corpus),
            "corpus_sha256": digest,
            "pairs_total": pairs_n,
            "pairs_per_format": counts,
            "hyperparams": {
                "rank": args.rank, "alpha": args.alpha, "epochs": args.epochs,
                "learning_rate": args.lr, "max_seq_len": args.max_seq,
            },
        },
        "produced": produced,
        # Mirrors finetune::AdapterArtifact for E8 S8.4.8 ingestion.
        "adapter_artifact": {
            "adapter_id": f"sleep-{adapter_id}",
            "description": f"sleep-phase QLoRA over {pairs_n} compiled pairs",
            "format": "baked_gguf" if produced.get("gguf") else "lora_adapter",
            "weights_digest": produced.get("weights_digest", ""),
            "provenance": {
                "base_model": args.base,
                "method": {"kind": "q_lora", "rank": args.rank, "alpha": args.alpha},
                "source_job": f"trainer/sleep_phase.py@{created_ns}",
                "created_at_ns": created_ns,
            },
        },
    }
    out_dir.mkdir(parents=True, exist_ok=True)
    path = out_dir / "manifest.json"
    path.write_text(json.dumps(manifest, indent=2) + "\n", encoding="utf-8")
    return path


def write_modelfile(out_dir: Path, gguf_rel: str, base: str) -> Path:
    modelfile = out_dir / "Modelfile"
    modelfile.write_text(
        f"# Sleep-phase adapted variant of {base}.\n"
        f"# Register with:  ollama create anima-adapted -f {modelfile}\n"
        f"FROM ./{gguf_rel}\n",
        encoding="utf-8",
    )
    return modelfile


# ── Live training (imports deferred so --dry-run never needs the GPU stack) ──


def train(args, pairs: list[dict]) -> dict:
    import torch  # noqa: PLC0415

    if not torch.cuda.is_available() and not args.cpu:
        print("✗ CUDA is not available (pass --cpu to force a CPU smoke run)", file=sys.stderr)
        raise SystemExit(2)

    from unsloth import FastLanguageModel  # noqa: PLC0415
    from datasets import Dataset  # noqa: PLC0415
    from trl import SFTConfig, SFTTrainer  # noqa: PLC0415

    print(f"→ loading base model {args.base} (4-bit)")
    model, tokenizer = FastLanguageModel.from_pretrained(
        model_name=args.base,
        max_seq_length=args.max_seq,
        load_in_4bit=True,
    )
    model = FastLanguageModel.get_peft_model(
        model,
        r=args.rank,
        lora_alpha=args.alpha,
        lora_dropout=0.0,
        target_modules=["q_proj", "k_proj", "v_proj", "o_proj",
                        "gate_proj", "up_proj", "down_proj"],
        use_gradient_checkpointing="unsloth",
        random_state=3407,
    )

    eos = tokenizer.eos_token or ""

    def to_text(p: dict) -> str:
        # Prefer the model's own chat template; fall back to Alpaca framing.
        try:
            return tokenizer.apply_chat_template(
                [{"role": "user", "content": p["prompt"]},
                 {"role": "assistant", "content": p["response"]}],
                tokenize=False,
            )
        except Exception:
            return (f"### Instruction:\n{p['prompt']}\n\n"
                    f"### Response:\n{p['response']}{eos}")

    dataset = Dataset.from_list([{"text": to_text(p)} for p in pairs])

    trainer = SFTTrainer(
        model=model,
        tokenizer=tokenizer,
        train_dataset=dataset,
        args=SFTConfig(
            dataset_text_field="text",
            max_seq_length=args.max_seq,
            per_device_train_batch_size=args.batch,
            gradient_accumulation_steps=4,
            num_train_epochs=args.epochs,
            learning_rate=args.lr,
            logging_steps=5,
            output_dir=str(args.out / "checkpoints"),
            optim="adamw_8bit",
            seed=3407,
            report_to="none",
        ),
    )
    print(f"→ training on {len(pairs)} pairs ({args.epochs} epoch(s))")
    trainer.train()

    produced: dict = {}
    adapter_dir = args.out / "adapter"
    model.save_pretrained(str(adapter_dir))
    tokenizer.save_pretrained(str(adapter_dir))
    produced["adapter_dir"] = str(adapter_dir)
    print(f"✓ LoRA adapter saved → {adapter_dir}")

    if args.gguf:
        gguf_dir = args.out / "gguf"
        print("→ merging + exporting GGUF (q4_k_m)")
        model.save_pretrained_gguf(str(gguf_dir), tokenizer, quantization_method="q4_k_m")
        ggufs = sorted(gguf_dir.glob("*.gguf"))
        if ggufs:
            produced["gguf"] = str(ggufs[0])
            produced["weights_digest"] = "sha256:" + hashlib.sha256(
                ggufs[0].read_bytes()
            ).hexdigest()
            modelfile = write_modelfile(args.out, ggufs[0].relative_to(args.out).as_posix(),
                                        args.base)
            produced["modelfile"] = str(modelfile)
            print(f"✓ GGUF exported → {ggufs[0]}")
            print(f"  register with:  docker compose exec ollama "
                  f"ollama create anima-adapted -f {modelfile}")
    if "weights_digest" not in produced:
        digest = hashlib.sha256()
        for f in sorted(adapter_dir.rglob("*")):
            if f.is_file():
                digest.update(f.read_bytes())
        produced["weights_digest"] = "sha256:" + digest.hexdigest()
    return produced


# ── Entry point ───────────────────────────────────────────────────────────────


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--corpus", type=Path,
                    default=Path(os.environ.get("TRAINER_CORPUS_DIR",
                                                "/anima-data/training_corpus")),
                    help="directory holding the sleep-phase JSONL corpus")
    ap.add_argument("--base", default=os.environ.get(
        "TRAINER_BASE_MODEL", "unsloth/gemma-4-E2B-it-unsloth-bnb-4bit"))
    ap.add_argument("--out", type=Path, default=None,
                    help="output dir (default /models/adapters/<utc-stamp>)")
    ap.add_argument("--dry-run", action="store_true",
                    help="validate the corpus + write a manifest; no GPU stack needed")
    ap.add_argument("--gguf", action="store_true",
                    help="also merge + export a Q4_K_M GGUF and a Modelfile")
    ap.add_argument("--cpu", action="store_true", help="permit a (slow) CPU smoke run")
    ap.add_argument("--min-pairs", type=int, default=4,
                    help="refuse to train on fewer unique pairs than this")
    ap.add_argument("--cot-in-response", action="store_true", default=True,
                    help="include chain_of_thought text in the training target")
    ap.add_argument("--rank", type=int, default=16)
    ap.add_argument("--alpha", type=int, default=32)
    ap.add_argument("--epochs", type=float, default=1.0)
    ap.add_argument("--lr", type=float, default=2e-4)
    ap.add_argument("--max-seq", type=int, default=2048)
    ap.add_argument("--batch", type=int, default=2)
    args = ap.parse_args()

    if args.out is None:
        stamp = time.strftime("%Y%m%dT%H%M%SZ", time.gmtime())
        args.out = Path("/models/adapters") / stamp

    print("anima-trainer: sleep-phase fine-tune")
    print(f"  corpus : {args.corpus}")
    print(f"  base   : {args.base}")
    print(f"  out    : {args.out}")

    if not args.corpus.is_dir():
        print(f"✗ corpus directory not found: {args.corpus}\n"
              f"  (the agent writes it during the PolicyCompilation sleep phase —\n"
              f"   has at least one task completed and a sleep cycle run?)",
              file=sys.stderr)
        return 1

    pairs, counts = load_corpus(args.corpus, args.cot_in_response)
    digest = corpus_digest(args.corpus)
    for name, n in counts.items():
        print(f"  {name:24s} {n} record(s)")
    print(f"  unique pairs           {len(pairs)}")
    print(f"  corpus sha256          {digest[:16]}…")

    if len(pairs) < args.min_pairs:
        print(f"✗ only {len(pairs)} unique pair(s) — below --min-pairs={args.min_pairs}; "
              f"let the agent live a little longer.", file=sys.stderr)
        return 1

    if args.dry_run:
        manifest = write_manifest(args.out, args, len(pairs), counts, digest,
                                  produced={"dry_run": True})
        print(f"✓ dry-run OK — manifest → {manifest}")
        return 0

    produced = train(args, pairs)
    manifest = write_manifest(args.out, args, len(pairs), counts, digest, produced)
    print(f"✓ run complete — manifest → {manifest}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
