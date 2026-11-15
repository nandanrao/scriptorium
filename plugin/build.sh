#!/bin/sh
# Builds the Scriptorium plugin for Claude: the MCP server (self-contained: it
# links the CLI library) goes in plugin/bin, where .mcp.json expects it. The
# `scriptorium` CLI is copied too, for people and agents who use it directly.
# Prototype packaging: one platform, the one you build on.
#
# Needs Rust (cargo). Install it with rustup (https://rustup.rs):
#   curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
#
# Then install the plugin from this folder (the one holding
# .claude-plugin/plugin.json):
#   Claude Code:              claude --plugin-dir /path/to/plugin
#   Cowork / Claude Desktop:  add the plugin from this folder in the app's
#                             plugin settings.
set -eu
here=$(cd "$(dirname "$0")" && pwd)
if ! command -v cargo >/dev/null 2>&1; then
    echo "cargo not found: install Rust with rustup (https://rustup.rs), then run this again" >&2
    exit 1
fi
cd "$here/.."
cargo build --release -p scriptorium-cli -p scriptorium-mcp
mkdir -p "$here/bin"
cp target/release/scriptorium target/release/scriptorium-mcp "$here/bin/"
echo "plugin ready: $here"
echo "  Claude Code:             claude --plugin-dir $here"
echo "  Cowork / Claude Desktop: add the plugin from the folder $here"
