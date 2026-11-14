#!/bin/sh
# Builds the server and the CLI it runs, and puts both in plugin/bin, where
# .mcp.json expects them. Prototype packaging: one platform, the one you build on.
set -eu
here=$(cd "$(dirname "$0")" && pwd)
cd "$here/.."
cargo build --release -p scriptorium-cli -p scriptorium-mcp
mkdir -p "$here/bin"
cp target/release/scriptorium target/release/scriptorium-mcp "$here/bin/"
echo "plugin ready: $here (try: claude --plugin-dir $here)"
