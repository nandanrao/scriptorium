# Trying Scriptorium on your Mac

Thank you for testing this. Scriptorium is an early version, so things may
be rough. Everything you notice helps, including the small things.

## 1. Download and install

1. Double-click `Scriptorium-0.1.0-mac.zip` to unzip it. You'll get
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

## 2. Open your script and add a review

1. **File › Open…** and choose your Final Draft script (the `.fdx` file).
2. **File › Add review…** and choose the review file (the `.review.txt`).
   You can add more than one.
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

---

*Optional, for the curious: the app also comes with a command-line tool for
AI agents to work on the same script; you can ignore it.*
