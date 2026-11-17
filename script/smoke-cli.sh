#!/usr/bin/env bash
# Smoke-test a built `scriptorium` CLI: `--version`, then an MCP
# `initialize` + `tools/list` round trip over `scriptorium mcp`.
#
#   script/smoke-cli.sh target/bundle/Scriptorium.app/Contents/MacOS/scriptorium
#
# Portable to macOS bash 3.2 and BSD tools. Needs no app running and no
# script: these two calls never touch the files or the app.
set -euo pipefail

BIN=${1:?usage: smoke-cli.sh path/to/scriptorium}
[[ -x "$BIN" ]] || { echo "smoke: $BIN is not an executable" >&2; exit 1; }

fail() { echo "smoke: $*" >&2; exit 1; }

version=$("$BIN" --version | head -n 1) || fail "\`scriptorium --version\` failed"
case "$version" in
  "scriptorium "[0-9]*) echo "smoke: $version" ;;
  *) fail "unexpected --version output: $version" ;;
esac

# One JSON-RPC message per line; the notification gets no reply. stdin ends
# after the last line, so the server exits on its own.
out=$(printf '%s\n' \
  '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"smoke","version":"0"}}}' \
  '{"jsonrpc":"2.0","method":"notifications/initialized"}' \
  '{"jsonrpc":"2.0","id":2,"method":"tools/list"}' \
  | "$BIN" mcp) || fail "\`scriptorium mcp\` exited with an error"

lines=$(printf '%s\n' "$out" | grep -c . || true)
[[ "$lines" == 2 ]] || fail "expected 2 replies from \`scriptorium mcp\`, got $lines:
$out"
init=$(printf '%s\n' "$out" | sed -n 1p)
list=$(printf '%s\n' "$out" | sed -n 2p)

for want in '"id":1' '"result":{' '"protocolVersion":"2025-06-18"' '"name":"scriptorium"'; do
  case "$init" in *"$want"*) ;; *) fail "initialize reply lacks $want: $init" ;; esac
done
for want in '"id":2' '"result":{"tools":[' '"name":"suggest_changes"' '"name":"where_is_writer"'; do
  case "$list" in *"$want"*) ;; *) fail "tools/list reply lacks $want: $(printf '%s' "$list" | cut -c1-400)" ;; esac
done

echo "smoke: \`scriptorium mcp\` answered initialize and tools/list"
