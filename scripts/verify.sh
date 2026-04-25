#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

cargo fmt --all -- --check
cargo test --workspace

npm --prefix frontend ci
npm --prefix frontend run check
npm --prefix frontend test
npm --prefix frontend run build

npm --prefix desktop ci
npm --prefix desktop test
