import? "Justfile.local"

# Default the test stage to cargo-nextest (parallel scheduling, flake
# retries per .config/nextest.toml); set SWAGE_TEST_RUNNER=cargo on a
# machine without cargo-nextest installed.
export SWAGE_TEST_RUNNER := env("SWAGE_TEST_RUNNER", "nextest")

default:
    @just --list

list:
    @just --list

setup:
    cargo run -p xtask -- setup

build-dev:
    cargo run -p xtask -- build-dev

build-dev-embedded:
    cargo run -p xtask -- build-dev --embed-frontend

dev gateway_port="3000" vite_port="5173" vite_host="0.0.0.0":
    cargo run -p xtask -- dev --gateway-port {{ gateway_port }} --vite-port {{ vite_port }} --vite-host {{ vite_host }}

fmt:
    cargo run -p xtask -- fmt

fmt-check:
    cargo run -p xtask -- fmt-check

check:
    cargo run -p xtask -- check

# Fast, pre-test static gate: local-state, format, workspace/frontend,
# frontend design-token, and clippy checks.
check-gate:
    cargo run -p xtask -- check-gate

clippy:
    cargo run -p xtask -- clippy

test:
    cargo run -p xtask -- test

test-e2e:
    cargo run -p xtask -- build-dev --embed-frontend
    cd frontend && PLAYWRIGHT_HTML_OPEN=never npm run test:e2e

test-lib package *filter:
    cargo run -p xtask -- test-lib {{ package }} {{ filter }}

test-integration package target *filter:
    cargo run -p xtask -- test-integration {{ package }} {{ target }} {{ filter }}

# CI runs this same recipe, so the local gate and CI cannot drift apart.
# The full gate: check-gate, tests, projection freshness, and desktop smoke.
verify:
    cargo run -p xtask -- verify

# Alias for `verify`, kept for muscle memory.
ci:
    cargo run -p xtask -- verify

sweep:
    cargo run -p xtask -- sweep

release:
    cargo run -p xtask -- release

release-force:
    cargo run -p xtask -- release --force-frontend

install prefix="${HOME}/.local/bin":
    cargo run -p xtask -- install --prefix {{ prefix }}

install-lan prefix="${HOME}/.local/bin":
    cargo run -p xtask -- install --prefix {{ prefix }} --lan

install-hooks:
    git config core.hooksPath .githooks
