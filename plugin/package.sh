#!/bin/sh
# Assembles the Scriptorium plugin folder: package.sh <out-dir> <bin-dir>
# Creates <out-dir>/scriptorium-plugin/ holding .claude-plugin/plugin.json,
# .mcp.json, skills/ and bin/{scriptorium,scriptorium-mcp} (copied from
# <bin-dir>). Used by the release workflow; also fine locally, e.g.
#   plugin/package.sh /tmp/pkg target/release
set -eu
if [ $# -ne 2 ]; then
    echo "usage: $0 <out-dir> <bin-dir>" >&2
    exit 2
fi
here=$(cd "$(dirname "$0")" && pwd)
bindir=$2
for b in scriptorium scriptorium-mcp; do
    [ -f "$bindir/$b" ] || { echo "missing $bindir/$b" >&2; exit 1; }
done
dest=$1/scriptorium-plugin
rm -rf "$dest"
mkdir -p "$dest/bin"
cp -R "$here/.claude-plugin" "$here/skills" "$dest/"
cp "$here/.mcp.json" "$dest/"
cp "$bindir/scriptorium" "$bindir/scriptorium-mcp" "$dest/bin/"
chmod +x "$dest/bin/scriptorium" "$dest/bin/scriptorium-mcp"
echo "plugin assembled: $dest"
