"""Identity memory — stable, human-readable facts about the agent and user.

The identity store is a JSON file on disk, loaded fresh at the start of each
cortex invocation.  Edits are written atomically (write-to-tmp, rename) so a
crash cannot leave a partial file.

Schema (all fields are optional)
~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~
::

    {
        "user": {
            "name":        "Alice",
            "preferences": {"language": "en", "verbosity": "brief"},
            "timezone":    "Europe/London"
        },
        "agent": {
            "name":        "Anima",
            "version":     "0.1.0",
            "capabilities": ["search", "code", "math"]
        },
        "policies": {
            "max_tool_calls_per_invocation": 10,
            "allow_network":                 false
        },
        "notes": []
    }

Usage
~~~~~
::

    im = IdentityMemory(Path("~/.anima/identity.json"))
    im.load()
    print(im.get("user.name"))
    im.set("user.name", "Bob")
    im.save()
"""

from __future__ import annotations

import json
import os
from pathlib import Path
from typing import Any


class IdentityMemory:
    """Thin wrapper around the identity JSON file."""

    def __init__(self, path: Path) -> None:
        self._path = path
        self._data: dict[str, Any] = {}

    # ── Persistence ───────────────────────────────────────────────────────────

    def load(self) -> None:
        """Load the identity file from disk, ignoring a missing file."""
        try:
            raw = self._path.read_text(encoding="utf-8")
            self._data = json.loads(raw)
        except FileNotFoundError:
            self._data = {}
        except json.JSONDecodeError as exc:
            raise ValueError(
                f"Corrupt identity file at {self._path}: {exc}"
            ) from exc

    def save(self) -> None:
        """Atomically and durably write the current data to disk.

        Uses a process-unique temp name (so concurrent savers cannot rename a
        torn file into place) and fsyncs both the temp file and the parent
        directory before/after the rename, so a crash or power loss cannot
        leave a partial or empty ``identity.json`` (L2).
        """
        parent = self._path.parent
        parent.mkdir(parents=True, exist_ok=True)
        tmp = self._path.with_name(f"{self._path.name}.{os.getpid()}.tmp")
        payload = json.dumps(self._data, indent=2, ensure_ascii=False)
        try:
            fd = os.open(tmp, os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o600)
            try:
                os.write(fd, payload.encode("utf-8"))
                os.fsync(fd)
            finally:
                os.close(fd)
            os.replace(tmp, self._path)
            # Persist the directory entry created by the rename.
            dir_fd = os.open(parent, os.O_RDONLY)
            try:
                os.fsync(dir_fd)
            finally:
                os.close(dir_fd)
        except BaseException:
            try:
                os.unlink(tmp)
            except OSError:
                pass
            raise

    # ── Key-value access (dot-separated paths) ────────────────────────────────

    def get(self, key: str, default: Any = None) -> Any:
        """Return the value at *key* (``"a.b.c"`` → nested lookup)."""
        parts = key.split(".")
        node: Any = self._data
        for part in parts:
            if not isinstance(node, dict):
                return default
            node = node.get(part, default)
            if node is default:
                return default
        return node

    def set(self, key: str, value: Any) -> None:
        """Set the value at *key*, creating intermediate dicts as needed."""
        parts = key.split(".")
        node = self._data
        for part in parts[:-1]:
            node = node.setdefault(part, {})
        node[parts[-1]] = value

    # ── Serialisation (for IPC) ───────────────────────────────────────────────

    def as_dict(self) -> dict[str, Any]:
        """Return a deep copy suitable for JSON serialisation."""
        return json.loads(json.dumps(self._data))

    @classmethod
    def from_dict(cls, data: dict[str, Any], path: Path | None = None) -> "IdentityMemory":
        """Construct an in-memory instance from a plain dict (tests / IPC)."""
        inst = cls(path or Path("/dev/null"))
        inst._data = data
        return inst
