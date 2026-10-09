#!/usr/bin/env sh
# Build (if needed) and start the CUEd desktop daemon with the UI open.
set -e
cd "$(dirname "$0")/.."
cargo build --release -p cuedd
exec ./target/release/cuedd --open "$@"
