#!/usr/bin/env python3
"""Select Rust workspace packages whose tests should run for a changed file set."""

from __future__ import annotations

import argparse
import json
import os
import subprocess
import sys
from pathlib import Path


FULL_RUN_PATHS = {
    ".github/workflows/ci.yml",
    "Cargo.lock",
    "Cargo.toml",
    "scripts/detect-rust-test-packages.py",
}


def run_json(command: list[str]) -> dict:
    result = subprocess.run(command, check=True, capture_output=True, text=True)
    return json.loads(result.stdout)


def changed_files(base: str, head: str) -> list[str]:
    result = subprocess.run(
        ["git", "diff", "--name-only", f"{base}...{head}"],
        check=True,
        capture_output=True,
        text=True,
    )
    return [line.strip() for line in result.stdout.splitlines() if line.strip()]


def workspace_packages(metadata: dict, repo_root: Path) -> dict[str, dict]:
    members = set(metadata["workspace_members"])
    packages = {}
    for package in metadata["packages"]:
        if package["id"] not in members:
            continue
        manifest_path = Path(package["manifest_path"]).resolve()
        packages[package["name"]] = {
            "id": package["id"],
            "name": package["name"],
            "root": manifest_path.parent.relative_to(repo_root).as_posix(),
            "dependencies": [
                dependency["name"]
                for dependency in package["dependencies"]
                if dependency.get("path") is not None
            ],
        }
    return packages


def reverse_dependency_closure(packages: dict[str, dict], roots: set[str]) -> set[str]:
    reverse_deps: dict[str, set[str]] = {name: set() for name in packages}
    for package_name, package in packages.items():
        for dependency_name in package["dependencies"]:
            if dependency_name in reverse_deps:
                reverse_deps[dependency_name].add(package_name)

    affected = set(roots)
    pending = list(roots)
    while pending:
        package_name = pending.pop()
        for dependent in reverse_deps.get(package_name, set()):
            if dependent not in affected:
                affected.add(dependent)
                pending.append(dependent)
    return affected


def detect_packages(files: list[str], packages: dict[str, dict]) -> tuple[bool, list[str], str]:
    if not files:
        return False, [], "no changed files"

    package_by_root = {package["root"]: name for name, package in packages.items()}
    directly_changed: set[str] = set()

    for file_path in files:
        if file_path in FULL_RUN_PATHS:
            return True, sorted(packages), f"{file_path} changed"

        if file_path.startswith(".github/workflows/") or file_path.startswith("scripts/"):
            return True, sorted(packages), f"{file_path} may affect CI"

        if file_path.endswith(".rs") and not file_path.startswith("crates/"):
            return True, sorted(packages), f"{file_path} is Rust outside a known crate"

        parts = Path(file_path).parts
        if len(parts) >= 2 and parts[0] == "crates":
            crate_root = f"{parts[0]}/{parts[1]}"
            package_name = package_by_root.get(crate_root)
            if package_name:
                directly_changed.add(package_name)

    if not directly_changed:
        return False, [], "no Rust package paths changed"

    affected = reverse_dependency_closure(packages, directly_changed)
    return False, sorted(affected), "affected workspace package closure"


def write_github_output(run_full: bool, packages: list[str], reason: str) -> None:
    output_path = os.environ.get("GITHUB_OUTPUT")
    if not output_path:
        return

    matrix = {"package": packages}
    with open(output_path, "a", encoding="utf-8") as output:
        output.write(f"run_full={'true' if run_full else 'false'}\n")
        output.write(f"has_packages={'true' if packages else 'false'}\n")
        output.write(f"packages={json.dumps(packages)}\n")
        output.write(f"matrix={json.dumps(matrix)}\n")
        output.write(f"reason={reason}\n")


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--base", help="Base git revision for diff detection")
    parser.add_argument("--head", help="Head git revision for diff detection")
    parser.add_argument(
        "--changed-file",
        action="append",
        default=[],
        help="Changed file path. May be supplied more than once.",
    )
    args = parser.parse_args()

    if args.changed_file:
        files = args.changed_file
    elif args.base and args.head:
        files = changed_files(args.base, args.head)
    else:
        parser.error("provide --base and --head, or at least one --changed-file")

    repo_root = Path.cwd().resolve()
    metadata = run_json(["cargo", "metadata", "--no-deps", "--format-version", "1"])
    packages = workspace_packages(metadata, repo_root)
    run_full, selected_packages, reason = detect_packages(files, packages)

    write_github_output(run_full, selected_packages, reason)

    print(
        json.dumps(
            {
                "changed_files": files,
                "run_full": run_full,
                "packages": selected_packages,
                "reason": reason,
            },
            indent=2,
        )
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
