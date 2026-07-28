import? "Justfile.local"

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

clippy:
    cargo run -p xtask -- clippy

test:
    cargo run -p xtask -- test

test-lib package *filter:
    cargo run -p xtask -- test-lib {{ package }} {{ filter }}

test-integration package target *filter:
    cargo run -p xtask -- test-integration {{ package }} {{ target }} {{ filter }}

verify:
    cargo run -p xtask -- verify

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
