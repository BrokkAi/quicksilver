#!/usr/bin/env bash
# Run once on a fresh Ubuntu 24.04 benchmark VM.
set -euo pipefail

sudo apt-get update -q
sudo env DEBIAN_FRONTEND=noninteractive apt-get install -yq \
  build-essential clang pkg-config git python3 rsync jq time tmux
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs \
  | sh -s -- -y --default-toolchain 1.99.1 --profile minimal -c llvm-tools
source "$HOME/.cargo/env"
cargo install --locked cargo-nextest@0.9.140
cargo install --locked cargo-mutants@27.1.0
rustc -vV
cargo nextest --version
cargo mutants --version
nproc; free -g; df -h "$HOME"
