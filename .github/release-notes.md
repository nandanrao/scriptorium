Unsigned (ad-hoc) macOS build, macOS 13 or later. Move Scriptorium to Applications before you open it the first time. First launch: see docs/testing-on-mac.md (it covers the `xattr` quarantine step). Work on a copy of your script.

**New: connect an AI agent.** Scriptorium › Connect an AI agent… has a button each for Claude Desktop, Claude Code and Codex. It sets up that agent to work with Scriptorium, keeping your other settings (with a backup). For Claude Desktop, quit and reopen Claude afterwards. For other agents, use Copy setup command.

**Live edits while the app is open.** When Scriptorium is open, the agent works on the script you have open: its suggestions show up on the cards as it makes them, and you decide them as usual. When the app is closed, the agent still works on the files, and Scriptorium shows the changes the next time you open it.

**Install command-line tool…** (in the Scriptorium menu) adds the `scriptorium` command to your Terminal. You don't need it for agents; it's there if you want to use Scriptorium from the command line.

**One download.** The separate Claude Code plugin zip is gone: everything is in the app. If you used the plugin, stop starting Claude Code with `--plugin-dir`, delete the `scriptorium-plugin` folder, and use Connect an AI agent… › Claude Code instead.

Sharing a folder needs the same Scriptorium version on every Mac.
