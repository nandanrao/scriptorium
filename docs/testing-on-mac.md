# Trying Scriptorium on your Mac

Thank you for testing this. Scriptorium is an early version, so things may
be rough. Everything you notice helps, including the small things.

## 1. Download and install

1. Double-click `Scriptorium-<version>-mac.zip` to unzip it. You'll get
   `Scriptorium.app`.
2. Drag `Scriptorium.app` into your **Applications** folder.
3. Open it the first time. macOS will be cautious, because the app isn't
   registered with Apple yet. It's safe. This only happens once.

**On macOS 13 or 14 (Ventura, Sonoma):** in Applications, right-click
(or Control-click) Scriptorium and choose **Open**. Then click **Open** again
in the box that appears.

**On macOS 15 (Sequoia):** double-click Scriptorium. macOS will say it can't
open it. Click **Done**. Then go to **System Settings › Privacy & Security**,
scroll down, and click **Open Anyway** next to Scriptorium. Confirm with your
password.

**If macOS says the app "is damaged"** (it isn't), open **Terminal**
(in Applications › Utilities), paste this line, and press Return:

```
xattr -dr com.apple.quarantine /Applications/Scriptorium.app
```

Then open Scriptorium as usual.

## 2. Open your script (and reviews)

1. **File › Open…** and choose your Final Draft script (the `.fdx` file).
2. If someone gave you a review file (a `.review.txt`), use
   **File › Add review…** to add it, or drag it onto the window. You can add
   more than one. Reviews made by Claude (section 7) appear by themselves, so
   you don't need this for those.
3. You can also drag files onto the window.

The sidebar switches between your plain script and each review. A review
lists its suggested changes, one card at a time. Click in the script to edit
the text of a line yourself; press `Esc` to go back to the card.

Next time you open the script, its reviews come back by themselves.

## 3. The keys

Anywhere in the app:

| Key | What it does |
|---|---|
| `Cmd+O` | Open a script |
| `Cmd+Shift+O` | Add a review |
| `Cmd+S` | Write the new `.fdx` (see below) |
| `Cmd+0` | Show the plain script |
| `Cmd+1`, `Cmd+2`, `Cmd+3` | Show the first, second, third review |
| `Cmd+Shift+]` / `Cmd+Shift+[` | Next / previous view |
| `Cmd+Z` / `Cmd+Shift+Z` | Undo / redo |
| `Cmd+Q` | Quit (your work is saved) |

On a review card:

| Key | What it does |
|---|---|
| `A` or `Return` | Accept the suggestion |
| `1` `2` `3` | Pick option 1, 2 or 3 |
| `Tab` / `Shift+Tab` | Next / previous option |
| `R` | Keep mine |
| `E` | Rewrite the line yourself (`Return` saves, `Esc` puts it back) |
| `J` / `K` (or down / up arrow) | Next / previous change |
| `U` / `Shift+U` | Undo / redo a decision |

While editing text, the usual Mac keys work: `Option+←/→` jumps a word,
`Cmd+←/→` goes to the start or end of the line, `Cmd+↑/↓` to the start or end
of the script, and `Cmd+A`, `Cmd+C`, `Cmd+X`, `Cmd+V` as always.

## 4. What gets written, and where

Your original script is **never changed**.

Next to your script, in the same folder, Scriptorium writes:

- `MY SCRIPT.scriptorium.json`: your work so far. It's saved as you go, so
  you can quit any time and pick up where you left off.
- `MY SCRIPT (scriptorium).fdx`: your script with your decisions applied.
  This is only written when you press `Cmd+S` (or **File › Write .fdx**).
  Open it in Final Draft like any other script.

- `MY SCRIPT.scriptorium.set-aside-<date-time>.json`: only appears if
  Scriptorium couldn't use or save over your session file (for example,
  another program changed it). It is a safety copy of your work. You can
  delete these files when you no longer need them.
- `MY SCRIPT.scriptorium.lock`: an empty file that keeps Scriptorium and
  Claude (or the `scriptorium` command) from saving your work at the same
  moment. Leave it; it can be deleted when nothing is running.

(With your own script's name in place of `MY SCRIPT`.)

## 5. What to try, and what to tell us

Please try these, and tell us what worked and what felt wrong:

- [ ] **The menu bar and Cmd shortcuts.** Do the menus show up at the top of
      the screen? Do the keys above work?
- [ ] **Accented letters.** Press `Option+e`, let go, then press `e`, to type é. Try ñ
      (`Option+n`, then `n`).
- [ ] **Sharpness.** Does the text look crisp, like other Mac apps?
- [ ] **Trackpad scrolling.** Smooth? Too fast or too slow?
- [ ] **Opening files.** Do the Open and Add review windows behave normally?
- [ ] **iCloud Drive.** Try a script that lives in iCloud Drive.
- [ ] **Quit and reopen.** Make a few decisions, quit, reopen the same script.
      Is everything where you left it?

Anything else is welcome too: confusing words, things you expected to work,
moments you got stuck. A screenshot (`Cmd+Shift+4`) helps a lot.

## 6. The log file

If something goes wrong, Scriptorium notes it in a small text file:

```
~/Library/Logs/Scriptorium/scriptorium.log
```

To find it: in Finder, choose **Go › Go to Folder…** (`Cmd+Shift+G`), paste
`~/Library/Logs/Scriptorium`, and press Return. Attach `scriptorium.log` to
an email or message, along with what you were doing when it happened.

## 7. Using Scriptorium with Claude Code (plugin)

Optional. The plugin lets Claude propose changes to your script, which you
then decide on in the app. You need
[Claude Code](https://docs.claude.com/en/docs/claude-code/setup) installed
first; follow its install page, then come back.

1. Download `Scriptorium-plugin-<version>-mac.zip` from the same release and
   double-click it to unzip. You get a folder called `scriptorium-plugin`.
2. Move that folder somewhere permanent, such as your **Documents** folder.
   Do this before anything else; if you move it later, the steps below stop
   working.
3. macOS blocks the tools inside until you clear it, once. Open **Terminal**
   and type this, including the space at the end (don't press Return yet):
   `xattr -dr com.apple.quarantine `
   Then drag the `scriptorium-plugin` folder from Finder into the Terminal
   window (it types the folder's path for you) and press Return.
4. To start Claude with the plugin, in Terminal type this, again with the
   space at the end: `claude --plugin-dir `
   Drag the `scriptorium-plugin` folder into the window and press Return.
   You need to start Claude this way **every time** you launch it.
   Type `/mcp` and check that `scriptorium` is listed as connected.
5. Tell Claude which script to work on. Drag the `.fdx` file from Finder into
   the Claude chat; that pastes its path.
6. Ask for things in plain words, for example:
   - "Review scene 12 of this script and suggest tighter dialogue."
   - "Suggest changes to MARA's lines so she sounds less formal."
   - "List the suggestions waiting on the script."

Where the suggestions show up: in Scriptorium, in the left sidebar, as a
review named after the pass (for example "dialogue-pass"). Press `Cmd+1` to
open it (or click it). You don't need **Add review**. If the script is
already open in the app, it picks up Claude's changes while you watch;
otherwise open the script and they're there.

To share a script with someone, keep the script, its `*.review.txt` files and
`<script>.scriptorium.json` together in one folder. You both need the same
Scriptorium version. Don't send the `.scriptorium.set-aside-*.json` or
`.scriptorium.lock` files.

Claude Desktop and Cowork: when your version supports plugins, add the
`scriptorium-plugin` folder in its plugin settings.

---

*Optional, for the curious: the app also comes with a command-line tool for
AI agents to work on the same script; you can ignore it.*
