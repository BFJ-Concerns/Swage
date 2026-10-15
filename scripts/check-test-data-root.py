#!/usr/bin/env python3
"""Run real test consumers and detect writes outside their declared roots."""

import argparse
import hashlib
import os
from pathlib import Path
import sqlite3
import subprocess
import tempfile


def snapshot(root):
    entries = {}
    for path in sorted(root.rglob("*")):
        name = str(path.relative_to(root))
        if path.is_symlink():
            entries[name] = ("symlink", os.readlink(path))
        elif path.is_dir():
            entries[name] = ("directory",)
        else:
            entries[name] = ("file", hashlib.sha256(path.read_bytes()).hexdigest())
    return entries


def tripwire():
    # This is the only access to the real operator data permitted by this check.
    result = subprocess.run(
        ["bash", "-c", "stat -c '%n %Y %s' /home/ben/.swage/swage.db*"],
        capture_output=True, text=True, check=False,
    )
    print(result.stdout, end="", flush=True)
    if result.returncode:
        print(result.stderr, end="", flush=True)
        raise RuntimeError("operator database tripwire unavailable")
    return result.stdout


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--scratch-parent", required=True, type=Path)
    parser.add_argument("--ambient-root", action="store_true")
    parser.add_argument("command", nargs=argparse.REMAINDER)
    args = parser.parse_args()
    command = args.command
    if command[:1] == ["--"]:
        command = command[1:]
    if not command:
        parser.error("a test command is required after --")
    args.scratch_parent.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix="data-root-", dir=args.scratch_parent) as directory:
        root = Path(directory)
        env = os.environ.copy()
        for key in ("DISPLAY", "WAYLAND_DISPLAY", "SWAGE_HOME"):
            env.pop(key, None)
        env["CARGO_HOME"] = "/home/ben/.cargo"
        env["RUSTUP_HOME"] = "/home/ben/.rustup"
        env["HOME"] = str(root / "operator")
        protected = [root / "operator" / ".swage"]
        for key in ("XDG_RUNTIME_DIR", "XDG_CONFIG_HOME", "XDG_DATA_HOME", "XDG_STATE_HOME"):
            path = root / key.lower()
            env[key] = str(path)
            protected.append(path)
        if args.ambient_root:
            protected.append(root / "ambient")
            env["SWAGE_HOME"] = str(protected[-1])
        for path in protected:
            path.mkdir(parents=True)
            path.chmod(0o700)
            (path / "sentinel").write_bytes(b"operator state\x00must survive\xff\n")
        with sqlite3.connect(protected[0] / "swage.db") as database:
            database.execute("CREATE TABLE isolation_sentinel (value TEXT)")
            database.execute("INSERT INTO isolation_sentinel VALUES ('untouched')")
        before = {str(path): snapshot(path) for path in protected}
        print(f"sentinel tree: {root}", flush=True)
        print(f"SWAGE_HOME: {env.get('SWAGE_HOME', '<unset>')}", flush=True)
        print("operator tripwire before:", flush=True)
        original_tripwire = tripwire()
        try:
            result = subprocess.run(command, env=env, check=False)
        finally:
            print("operator tripwire after:", flush=True)
            final_tripwire = tripwire()
            after = {str(path): snapshot(path) for path in protected}
            changed = before != after or original_tripwire != final_tripwire
            for path in protected:
                old, new = before[str(path)], after[str(path)]
                for entry in sorted(old.keys() | new.keys()):
                    if old.get(entry) != new.get(entry):
                        print(f"CHANGED: {path / entry}", flush=True)
            print("sentinel comparison: " + ("FAIL" if changed else "PASS"), flush=True)
        return 1 if changed else result.returncode


if __name__ == "__main__":
    raise SystemExit(main())
