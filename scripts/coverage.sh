#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'EOF'
Usage: scripts/coverage.sh [--install]

Generate local HTML coverage reports using cargo-llvm-cov.

Outputs:
  ./coverage/index.html

Prereqs:
  - rustup + a Rust toolchain installed
  - rustup component: llvm-tools-preview
  - cargo subcommand: cargo-llvm-cov

Options:
  --install   Install missing prerequisites (llvm-tools-preview + cargo-llvm-cov)
EOF
}

INSTALL=0
while [[ $# -gt 0 ]]; do
  case "$1" in
    --install) INSTALL=1; shift ;;
    -h|--help) usage; exit 0 ;;
    *) echo "Unknown arg: $1" >&2; usage; exit 2 ;;
  esac
done

if ! command -v cargo >/dev/null 2>&1; then
  echo "Missing required command: cargo. Install Rust and ensure cargo is on PATH." >&2
  exit 1
fi

if ! command -v rustup >/dev/null 2>&1; then
  echo "Missing required command: rustup. Install rustup (https://rustup.rs/)." >&2
  exit 1
fi

if ! rustup component list --installed | grep -q '^llvm-tools-preview'; then
  if [[ "$INSTALL" -eq 1 ]]; then
    echo "Installing rustup component: llvm-tools-preview"
    rustup component add llvm-tools-preview
  else
    echo "Missing rustup component llvm-tools-preview." >&2
    echo "Run: rustup component add llvm-tools-preview (or rerun with --install)" >&2
    exit 1
  fi
fi

if ! cargo llvm-cov --version >/dev/null 2>&1; then
  if [[ "$INSTALL" -eq 1 ]]; then
    echo "Installing cargo subcommand: cargo-llvm-cov"
    cargo install cargo-llvm-cov
  else
    echo "Missing cargo subcommand cargo-llvm-cov." >&2
    echo "Run: cargo install cargo-llvm-cov (or rerun with --install)" >&2
    exit 1
  fi
fi

mkdir -p coverage
echo "Generating HTML coverage report into ./coverage/"
cargo llvm-cov --workspace --html --output-dir coverage
echo "Coverage report: $(pwd)/coverage/index.html"
