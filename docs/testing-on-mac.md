# Trying Scriptorium on your Mac

Thank you for testing this. Scriptorium is an early version, so things may
be rough. Everything you notice helps, including the small things.

## What's new in 0.6.0

0.6.0 is about connecting Claude (or another AI app) to Scriptorium. See
section 7.

- **One button to connect Claude.** **Scriptorium › Connect an AI agent…**
  has a **Connect** button each for Claude Desktop, Claude Code and Codex. It
  sets up that app for you, keeping its other settings. For Claude Desktop,
  Claude then has to quit and open again; Scriptorium can do that for you.
- **Claude sees what you see.** While Scriptorium is open, Claude works on
  the script you have open: ask it "What am I looking at in Scriptorium?" and
  it names the script, the scene and the line you're on. Its suggestions
  appear on the cards as it makes them.
- **It still works with the app closed.** Claude then works on the files,
  and Scriptorium shows the changes the next time you open the script.
- **One download.** There is no plugin zip any more: everything is inside
  `Scriptorium.app`. If you used the plugin, see "If you used the plugin
  before" in section 7.
- **Install command-line tool…** (in the Scriptorium menu) adds the
  `scriptorium` command to Terminal, if you want it. Claude doesn't need it.
- **Move the app to Applications before you open it** (section 1). It
  matters more now, because Claude is pointed at the app where it is.

The checklist for this version is at the end of section 7, "Please try
(0.6.0)".

## What's new in 0.5.0

0.5.0 brings two big things: translating with Claude, and the AI chat. (The
chat was planned as 0.4.0, but 0.4.0 was never released, so it is new to you
too.)

**Translating.** See section 7, "Translating with Claude".

- **A version of your script in another language.** Claude (connected as in
  section 7, or the chat in the app) or **File › New version…** (`Cmd+T`) makes
  a second Final Draft file next to yours, `MY SCRIPT.es.fdx`, lined up line
  by line with the English. Claude fills it with suggestions you decide as
  usual; each card shows the English line.
- **The English on the left.** Open the Spanish and the English is lined up
  on the left, row by row, with a coloured bar per row saying whether it's
  translated, in sync, or changed since. **Swap** (at the top) opens the
  other file.
- **Write .fdx saves the translation itself.** For a version Scriptorium
  made, `Cmd+S` writes that file (`MY SCRIPT.es.fdx`), not a
  `(scriptorium)` copy. Your original is still never changed.

**The AI chat.** See section 9.

- Press `Cmd+L` to open a chat column at the right. You talk to an agent
  (Claude, GPT and others) about your script, and the changes it proposes
  appear as suggestions on a review card for that conversation, to decide as
  usual. Nothing in your script changes until you accept it.
- **You bring your own API key.** Section 9 says how to get one, set it up
  and answer the Mac's Keychain questions.
- **Agents only write into review files in the script's folder.** A review
  you attached from another folder, or one that doesn't start with
  `SCRIPT REVIEW`, is read-only for the AI: it can read it but adds its
  suggestions to its own review instead.
- **The AI's "write the script" replaces `MY SCRIPT (scriptorium).fdx`
  without warning** (it never touches your original or any other `.fdx`),
  so don't keep editing that file: copy it to another name first.

Please do section 9 on a copy of your script.

## What's new in 0.3.0 (still worth checking)

- **Full editing.** `Return` splits a line in two, `Backspace` at the start
  of a line joins it to the one above, and `Tab` changes a line's type
  (Action, Character, Dialogue and so on). You can select several lines and
  delete them, and add new lines.
- **Paste guesses line types.** Text pasted from another app is split into
  lines and Scriptorium guesses each line's type; `Tab` fixes a wrong guess.
- **Copy keeps types inside Scriptorium.** Copying and pasting within
  Scriptorium keeps each line's type. Copying to Final Draft or another app
  gives plain text only.
- **Finish a suggestion's rewrite faster.** While rewriting a suggestion's
  line (`E`), press `Cmd+Return` to save it (`Esc` puts it back).
- **Bigger suggestions.** A suggestion can now change several lines at once:
  cut lines, add lines, or rework a whole passage. The card shows what
  happens: red struck-through text goes, green text is new. The script view
  keeps its dots.
- **Lines you changed under a suggestion.** If you removed or split a line
  that a suggestion was about, the card says "line removed", "line
  changed" or "line cut" (a suggestion you had decided whose lines were later
  removed).

Because 0.3.0 can now add and remove lines, please do the checks in
section 8 before relying on it.

## 1. Download and install

1. Double-click `Scriptorium-<version>-mac.zip` to unzip it. You'll get
   `Scriptorium.app`.
2. **Before you open it,** drag `Scriptorium.app` into your **Applications**
   folder. Please don't open it from Downloads first: macOS then runs it from
   a hidden temporary place, and Claude can't be connected to it (the
   Connect window asks you to move it first, and changes nothing).
3. Open it from Applications the first time. macOS will be cautious, because
   the app isn't registered with Apple yet. It's safe. This happens once for
   each new version you download.

**Updating to a newer version:** quit Scriptorium, drag the new
`Scriptorium.app` into Applications and choose **Replace**. Keep the same
name and place: Claude stays connected to it.

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

Then open Scriptorium as usual. The same line is the fix if Claude later
can't start Scriptorium (section 7).

## 2. Open your script (and reviews)

1. **File › Open…** and choose your Final Draft script (the `.fdx` file).
2. If someone gave you a review file (a `.review.txt`), use
   **File › Add review…** to add it, or drag it onto the window. You can add
   more than one. Reviews made by Claude (section 7) appear by themselves, so
   you don't need this for those.

The sidebar switches between your plain script and each review. A review
lists its suggested changes, one card at a time. Click in the script to edit
the script yourself (change text, split or join lines, add or remove lines,
change a line's type); press `Esc` to go back to the card.

Next time you open the script, its reviews come back by themselves.

## 3. The keys

Anywhere in the app:

| Key | What it does |
|---|---|
| `Cmd+O` | Open a script |
| `Cmd+Shift+O` | Add a review |
| `Cmd+S` | Write the new `.fdx` (see below) |
| `Cmd+T` | Make a version of the script (**File › New version…**, section 7) |
| `Cmd+L` | Open or close the AI chat (section 9) |
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
| `E` | Rewrite the line yourself (`Cmd+Return` saves, `Esc` puts it back) |
| `J` / `K` (or down / up arrow) | Next / previous change |
| `U` / `Shift+U` | Undo / redo a decision |

While editing the script: `Return` splits a line, `Backspace` at the start of
a line joins it to the one above, `Tab` changes the line's type. Lines inside
dual dialogue can be edited, but not split, joined or removed.

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

For connecting Claude, see "Please try (0.6.0)" at the end of section 7.

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

## 7. Connecting Claude (or another AI agent)

Optional. Once connected, Claude (or another AI app) can work on your script:
a review pass, tighter dialogue, a translation. What it proposes arrives as
suggestions on a review card, and you decide them in Scriptorium as usual.
Nothing in your script changes until you accept it. Everything needed is
inside `Scriptorium.app`; there is nothing else to download.

Work on a copy of your script the first time.

### Claude Desktop

1. Open Scriptorium (from Applications, see section 1).
2. In the menu bar, choose **Scriptorium › Connect an AI agent…**. A panel
   opens on the right with a row each for **Claude Desktop**, **Claude
   Code** and **Codex**, each saying "Connected" or "Not connected".
3. Click **Connect** next to Claude Desktop.
   - **If Claude is closed,** the panel says "Connected Claude Desktop. Open
     Claude (or reopen it) to use Scriptorium." Open Claude.
   - **If Claude is open,** the panel says "Claude is open and would undo
     this. Quit and reopen it for you?" Click **Quit and reopen Claude for
     me**. Claude closes and opens again by itself. The first time, macOS
     may ask whether Scriptorium may control Claude: click **OK**. (Or quit
     Claude yourself with `Cmd+Q` while in Claude, then click **Connect**
     again and open Claude.)
   - The row now says **Connected**. Claude's other settings are kept, and
     Scriptorium keeps a copy of the file it changed, just in case.
4. In Claude, check that it's there: click the tools button under the
   message box (a **+** or a sliders icon, depending on your version of
   Claude) and look for **scriptorium** in the list. It should be switched
   on.
5. The first time Claude uses Scriptorium in a chat, it asks your
   permission. Choose **Allow always** (or **Allow once** if you prefer to
   be asked each time).

**If macOS blocks it.** When you click Connect, Scriptorium first checks
that Claude will be able to start it. If macOS is blocking it, the panel
says "macOS is blocking Scriptorium's helper…" and shows a **Copy the
Terminal command** button. Click it, open **Terminal** (Applications ›
Utilities), paste with `Cmd+V`, press Return, and click **Connect** again.
It's the same `xattr` line as in section 1. If Claude ever says it can't
start or reach "scriptorium", run that line too, then quit and reopen
Claude.

### Claude Code

Click **Connect** next to Claude Code. Then start a **new** `claude`
session in Terminal (sessions already running don't see it) and type `/mcp`:
`scriptorium` should be listed as connected.

### Codex

Click **Connect** next to Codex. Then start a new Codex session (the
ChatGPT desktop app and the Codex command share this setting).

### Other AI apps

Any app that can use "MCP servers" can connect too. At the bottom of the
panel, under **Other agents**, click **Copy setup command**. In the other
app's settings, add an MCP server of the "stdio" kind and paste the
command.

**To undo any of this,** open the panel again and click **Disconnect** on
that row.

**If you move or rename Scriptorium,** open it once from its new place. It
puts Codex and Claude Code right by itself, and the status bar says, for
example, "Updated Codex's settings to this copy of Scriptorium." For Claude
Desktop, if Claude is closed it's fixed the same way; if Claude is open, the
status bar says "Claude's Scriptorium setting points at a copy of
Scriptorium that was moved." with a **Fix (quits and reopens Claude)**
button.

### Asking Claude

Open your script in Scriptorium, then in Claude ask:

> What am I looking at in Scriptorium?

Claude asks Scriptorium and answers with the script that's open, the scene,
and the line you're on (or the suggestion card you're looking at). This is
the easiest way to check that everything is connected.

**With Scriptorium open (live).** Ask for things in plain words, for
example:

- "Do a dialogue pass on this scene."
- "Suggest changes to MARA's lines so she sounds less formal."
- "List the suggestions waiting on this script."

The suggestions appear in the sidebar while Claude works, as a review named
after the pass (for example "dialogue-pass"). Press `Cmd+1` (or click it)
and decide them as usual. You don't need **Add review**, and you don't need
to tell Claude where the script is: it works on the one you have open. If
you ask Claude to undo, it undoes the last change made to the script,
whoever made it, including your own typing.

**With Scriptorium closed (files).** Claude still works: it changes the
files next to your script, and the next time you open the script in
Scriptorium the suggestions are there. Claude needs to know which script.
It uses the one from earlier in the conversation, or asks you. To give it
the script's location: in Finder, click the `.fdx` file once, hold
**Option**, right-click (or Control-click) it and choose **Copy "MY
SCRIPT.fdx" as Pathname**, then paste that into Claude. (In Claude Code
you can also drag the file into the window.)

Only the script open in Scriptorium is worked on live. If you ask about
another script, Claude works on its files, and you see the result when you
open it.

If Claude says "Scriptorium is open but a different version", you have two
copies of Scriptorium (for example an old one in Downloads). Quit, delete
the old copy, open the one in Applications and click **Connect** again.

To share a script with someone, keep the script, its `*.review.txt` files and
`<script>.scriptorium.json` together in one folder. You both need the same
Scriptorium version. Don't send the `.scriptorium.set-aside-*.json` or
`.scriptorium.lock` files.

### If you used the plugin before

Versions up to 0.5.0 came with a separate Claude Code plugin. It's no longer
needed, and it shouldn't be used with 0.6.0:

1. Stop starting Claude with `claude --plugin-dir …`. Start it with plain
   `claude`.
2. Delete the `scriptorium-plugin` folder (wherever you put it, for example
   in Documents) and the old `Scriptorium-plugin-…-mac.zip`.
3. If you added that folder in Claude Desktop's or Cowork's plugin settings,
   remove it there.
4. Connect Claude Code (and Claude Desktop, if you use it) as above.

Your scripts, reviews and decisions are unaffected.

### Translating with Claude (0.5.0)

Claude makes a translation of your script as a second Final Draft file, lined
up with the original line by line, and proposes each translated line as a
suggestion. You decide them in Scriptorium as usual. Work on a copy of your
script the first time.

**Before you start.**

- **Keep the project folder out of iCloud.** Put the script in a plain
  folder that iCloud doesn't sync, for example a new folder in your home
  folder (in Finder, **Go › Home**), not on the Desktop or in Documents if
  "Desktop & Documents Folders" is turned on in iCloud settings. Not in
  Dropbox either. Work on it on **one computer**.
- **Connect Claude first** (above), and check it answers "What am I
  looking at in Scriptorium?".

**Your first day.**

1. Optional but worth it: in the script's folder, write a short brief for
   Claude in a text file named `brief.es.md` (for Spanish; `brief.fr.md` for
   French and so on): who the audience is, which Spanish (Spain, Mexico,
   Argentina…), formal or informal address, how to write scene headings
   (keep INT./EXT.? translate DAY/NIGHT?), names not to translate. If you
   have agreed translations for words that come back often, list them in a
   file named `glossary.md` in the same folder. Claude reads both when they
   are there. Without a brief, Claude makes its own choices and tells you
   what it chose for scene headings.
2. Open the English script in Scriptorium and ask Claude:
   "Make a Spanish version of this script and translate scene 1."
   (With Scriptorium closed, give Claude the script's location as described
   under "Asking Claude".)
3. Claude makes, next to your script:
   - `MY SCRIPT.es.fdx`: the Spanish version. Scene headings and character
     names are copied from the English (Claude still suggests a translation
     for each scene heading); every other line starts empty.
   - `MY SCRIPT.es.links.txt`: how the lines of the two files line up.
     Leave it alone.
   - `Translation from English.review.txt`: Claude's suggestions.
4. Open `MY SCRIPT.es.fdx` in Scriptorium (**File › Open…**). The English
   is lined up on the left, the Spanish on the right (see "What you see"
   below). The review "Translation from English" is in the sidebar; press
   `Cmd+1`. Each card shows the English line and Claude's Spanish. Accept,
   pick another option, keep the empty line, or rewrite it, as with any
   review. Claude gives one option when the line is obvious and two or
   three, with a short note, when there's a real choice.
5. Keep `MY SCRIPT.es.fdx` open in Scriptorium and ask Claude for more
   ("Translate scenes 2 to 5"). The new suggestions appear while you watch.
   It skips lines that already have a suggestion, so asking twice doesn't
   duplicate anything. With Scriptorium closed, Claude works on the files,
   and the suggestions are there when you open the Spanish.

**Making the version in Scriptorium instead.** Open the English script and
choose **File › New version…** (`Cmd+T`). Type the language (`es` for
Spanish, `fr` for French, `pt-BR` for Brazilian Portuguese…) and the
original's language (`en`), then press **Create and open**. Scriptorium
makes `MY SCRIPT.es.fdx` and opens it with the English on the left. Its lines
are empty (headings and names copied) until someone translates them: ask
Claude (connected as above), or the chat in the app (below). A script has one version for now; New version… on a script that
already has one says so.

**What you see.**

- **Two columns.** The English on the left (grey, read only), the Spanish on
  the right, each English line beside its Spanish line. A thin coloured bar
  between them says how each row stands: not yet translated, in sync,
  changed in the Spanish, changed in the English, or both.
- **The sidebar** has a section "Lined up on the left": `◧ English` with how
  many lines are left to translate under it, and a small legend of the
  colours (hover over it for the full words). Click **Nothing** to hide the
  English; click `◧ English` to bring it back.
- **At the top:** "English | **Español**" (the open file in bold) and
  **Swap**, which opens the English with the Spanish lined up on the left.
  Swap again to come back.
- **A narrow window** (for example with the chat open on a laptop): the
  English gives way first, and a strip says "English hidden: widen the
  window or close the chat (`Cmd+L`)." Close the chat or make the window
  wider to see it again.
- Opening the English on its own, the status bar says
  "Lined up with MY SCRIPT.es.fdx: open it (Swap, in the toolbar) to
  translate."

**How to work.**

- **Decide in Scriptorium first, polish in Final Draft after.** When you are
  ready, **close `MY SCRIPT.es.fdx` in Final Draft** if it is open there,
  then press `Cmd+S` (**File › Write .fdx**) in Scriptorium. That saves the
  Spanish file itself (no `(scriptorium)` copy), and the status bar says
  "Wrote MY SCRIPT.es.fdx. Undo starts again from here." Then open it in
  Final Draft to polish, and reopen it in Scriptorium whenever you like:
  Scriptorium finds the lines again.
- **When Write .fdx says "Not written".** The status bar says why:
  - "…changed since you opened it. Close it in Final Draft and reopen it
    here.": Final Draft saved the Spanish in the meantime. Close it there,
    open it again in Scriptorium (your decisions are kept), and press
    `Cmd+S` again.
  - "Decide or rewrite #4 and #9 first.": you split or joined the lines
    of those suggestions. Decide them (or undo the split) first.
  - "The assistant is still working on this script…": wait for the chat to
    finish, or stop it (`Cmd+.`).
- Decisions are never lost, but **lines you type straight into the Spanish
  in Scriptorium** (not through a card) are lost if Final Draft saves the
  file before you press Write .fdx. Write first, then go to Final Draft.
- **Mark in sync.** When you fix a Spanish line yourself (in Final Draft,
  or typing it in Scriptorium and writing it), its row reads "Español
  changed". If it's right, click in that line and choose **Edit › Mark in
  sync**: the row is in sync again. **Edit › Mark not in sync** takes it
  back (the row then asks for a new translation). The same works when the
  English was reworded but the Spanish still fits. A line you typed in
  Scriptorium and haven't written yet can't be marked: press `Cmd+S` first.
- **The title page and scene summaries stay in English**, and the English
  script notes aren't copied. That's on purpose: change the title page (and
  the summaries, if you use them) in Final Draft. Same for "(CONT'D)" and
  "(MORE)": set them once in Final Draft's Mores and Continueds settings.
- **When the English changes** (you edit it in Final Draft): ask Claude to
  "update the Spanish". Lines whose English changed get new suggestions in
  the same review, "Translation from English"; the earlier suggestion on such
  a line was for the old English, so it no longer shows, and what you decided
  then stays in the Spanish text until you pick the new one.
- **A new English line** has no Spanish line to fill yet; Claude lists them.
  For each, add an empty line of the same type (Action, Dialogue…) in the
  Spanish in Final Draft at that place, save, close, and ask Claude again: it
  fills it. (A later version will do this for you.)
- Never edit `MY SCRIPT.es.links.txt` or the review files by hand.
- **The AI chat in Scriptorium** (`Cmd+L`) can make the version too: on the
  English, ask "Make a Spanish version of this script". It makes the file and
  tells you to open it; a conversation stays on the script it was started
  on, so press **Swap** (or open `MY SCRIPT.es.fdx`), start a new
  conversation there (`Cmd+N` in the chat), and ask it to translate.

**Please try, on a copy** (screenshot what looks wrong):

- [ ] With the English open in Scriptorium, ask Claude "Make a Spanish
      version of this script and translate scene 1". `MY SCRIPT.es.fdx` and the review
      appear.
- [ ] Before deciding anything, open `MY SCRIPT.es.fdx` in **Final Draft**:
      no warning; headings and names in English; every other line empty
      with the right type. Save it there unchanged and close it.
- [ ] Open it in Scriptorium: the English on the left, lined up. Accept a
      few lines, keep one empty, rewrite one. Do the colours match the
      legend?
- [ ] With the chat open (`Cmd+L`) on your laptop: does the English give way
      with the "English hidden" strip? Close the chat: does it come back?
- [ ] Press **Swap** and back.
- [ ] Press `Cmd+S`, then open `MY SCRIPT.es.fdx` in Final Draft: the
      Spanish lines you accepted, with the right indents; the rest empty.
- [ ] In Final Draft, type a Spanish line into one of the empty lines, save,
      close. Reopen it in Scriptorium: is that line there, and does its row
      read changed? Use **Edit › Mark in sync** on it.
- [ ] **File › New version…** (`Cmd+T`) on another copy of the script: does
      the sheet make the version and open it?

**Sharing.** Keep these together in one folder: both scripts, the
`.links.txt` file, the `.review.txt` files and the `.scriptorium.json` files.

**Starting over.** Quit Scriptorium, then delete `MY SCRIPT.es.fdx`,
`MY SCRIPT.es.links.txt`, `MY SCRIPT.es.scriptorium.json` and the
`Translation from English.review.txt` file (and, if there,
`MY SCRIPT.es.scriptorium.lock`, `MY SCRIPT.es.links.lock` and any
`MY SCRIPT.es.scriptorium.set-aside-*.json`). Your English script is untouched; ask Claude again.

**From Terminal** (optional): the `scriptorium` command (see below) does the
same without Claude. `scriptorium version new "MY SCRIPT.fdx" --lang es
--from en` makes the version (`--from en` names the original as English, so
the review and labels say "English"); `scriptorium rows "MY SCRIPT.es.fdx"
--scene 1` shows each line next to its English and what it needs;
`scriptorium links "MY SCRIPT.es.fdx" sync 37` marks row 37 in sync (as
**Edit › Mark in sync** does; `unsync` takes it back).

### The `scriptorium` command (optional)

You don't need this for Claude or any other AI app. It's for people who like
Terminal.

1. Choose **Scriptorium › Install command-line tool…**.
2. On most Macs, macOS asks for your Mac's password once (on a Mac with
   Homebrew it usually doesn't). Type it and click **OK**.
3. The status bar says "Installed. Open a new Terminal window and type
   scriptorium --help."
4. Open a **new** Terminal window and type `scriptorium --help`.

If you cancel the password box, nothing is changed. If you move Scriptorium
later and `scriptorium` says "no such file", choose the menu item again.

### Please try (0.6.0)

On a copy of a script. Screenshot anything that looks wrong, and note your
macOS version (Apple menu › About This Mac).

- [ ] **Install.** A fresh download, moved to Applications, then opened
      (section 1). Nothing about agents or Terminal is asked when it
      starts. Choose **Scriptorium › Install command-line tool…**: one
      password box at most. In a new Terminal window, `scriptorium --help`
      prints the help.
- [ ] **Claude Desktop.** **Connect an AI agent…** › **Connect** on Claude
      Desktop, then quit and reopen Claude. "scriptorium" is in Claude's
      tools list. Open a script in Scriptorium, click a line, and ask
      Claude "What am I looking at in Scriptorium?": it names the script
      and that line. Ask for a dialogue pass on that scene: suggestions
      appear on the card within a second or so.
- [ ] **App closed.** Quit Scriptorium and ask Claude for another scene. It
      works on the files (it asks for the script's location, or uses the
      one from before). Open the script in Scriptorium: the suggestions are
      there.
- [ ] **Renamed app.** Quit Scriptorium, rename it to "Scriptorium 2" in
      Applications, and open it. The status bar says it updated Claude
      Desktop's settings (or, with Claude open, offers **Fix (quits and
      reopens Claude)**). Restart Claude: it still connects. Then try it
      once more from a folder with a space in its name (for example a new
      folder "My Apps" in your home folder). Rename it back afterwards.
- [ ] **Claude Code.** **Connect** on Claude Code, then in a new `claude`
      session type `/mcp`: `scriptorium` is connected. Type `/` and look
      for Scriptorium's **review_pass** in the list: does it start a review
      pass?
- [ ] **Codex.** **Connect** on Codex, then in a new `codex` session ask
      "what's open in Scriptorium?": it answers with your script. With
      Scriptorium closed, ask it to review a script in a folder outside the
      one Codex was started in: the review file appears next to that
      script.
- [ ] **Two at once.** Claude Desktop and Codex both making suggestions at
      the same time (ask them for different passes): both reviews fill up.
- [ ] **From Downloads.** Put a fresh copy in Downloads and open it from
      there without moving it. **Connect an AI agent…** says to move it to
      Applications first, and nothing is changed. (Delete that copy
      afterwards.)
- [ ] **"Open Anyway" only.** On a fresh download, use only **Open Anyway**
      (or right-click › Open), not the `xattr` line. Does Claude Desktop
      still start Scriptorium? Does Connect say macOS is blocking it? Tell
      us which macOS version (13, 14 or 15).
- [ ] **Quit and reopen for me.** With Claude Desktop running and a chat
      open, click **Connect** (after a **Disconnect**). Use **Quit and reopen
      Claude for me**. Claude comes back with "scriptorium" listed, and
      your other connectors and Claude's own settings are as before.
- [ ] **Quitting in the middle.** Ask Claude for something, quit
      Scriptorium, then ask Claude for something else. The second request
      works on the files, with no error saying a change "may have been
      applied".

**Only if we ask (for whoever is comfortable in Terminal): the Claude
Desktop extension.** This decides how a later version connects Claude
Desktop.

1. In **Connect an AI agent…**, click **Disconnect** on Claude Desktop and
   let it quit and reopen Claude.
2. In Terminal, run:
   `/Applications/Scriptorium.app/Contents/MacOS/scriptorium connect claude-desktop --mcpb`
   Does Claude show an install screen? Click **Install**. Are Scriptorium's
   tools listed, and does "What am I looking at in Scriptorium?" work?
3. In Claude's **Settings › Extensions**, is there an on/off switch for
   Scriptorium?
4. When we send you a newer build, replace the app in Applications without
   installing the extension again. Open the new Scriptorium, quit and reopen
   Claude, and ask "What am I looking at in Scriptorium?". If it answers,
   the extension runs the new build; if it says "Scriptorium is open but a
   different version", it doesn't.
5. Copy the app into a folder with a space in its name (for example
   "My Apps" in your home folder) and repeat step 2 with that copy's path in
   quotes, for example
   `"$HOME/My Apps/Scriptorium.app/Contents/MacOS/scriptorium" connect claude-desktop --mcpb`.
6. Afterwards, remove the extension in **Settings › Extensions**, delete the
   copy, and **Connect** Claude Desktop again from the panel.

## 8. Checking files in Final Draft (please do this before relying on 0.3.0)

0.3.0 can add, remove, split and join lines, so the `.fdx` it writes needs
checking in Final Draft. **Work on a copy of a script, never your only copy.**
Take a screenshot of each result.

1. Open the copy in Scriptorium and save (`Cmd+S`) without changing anything.
   Open the new `(scriptorium).fdx` in Final Draft. It should look identical
   to the original.
2. Back in Scriptorium, on the copy, do each of these:
   - add a new scene (a scene heading, an action line, a character, a line of
     dialogue);
   - delete a whole scene, including its heading;
   - split an action paragraph (put the cursor in the middle, press `Return`);
   - join two dialogue lines (`Backspace` at the start of the second);
   - change an action line to dialogue (`Tab`);
   - put a word in italics in one of the new lines.

   Save (`Cmd+S`).
3. Open the saved `.fdx` in Final Draft. Look for:
   - new lines have the right font, indents and capitals;
   - scene numbers run on with no gaps and no repeats;
   - page count and page breaks look right;
   - the italic word is still italic;
   - the title page, headers and Navigator are fine;
   - inserted lines have the same indents and spacing as other lines of the
     same type;
   - with revision colours turned on, the new and edited text looks normal
     (not odd colours or marks);
   - an Action line you changed to Character now shows in capitals with the
     Character indent;
   - a numbered Scene Heading you changed to Action has lost its scene number
     and summary; an Action you changed to a Scene Heading gets a number from
     Final Draft;
   - a new empty line opens cleanly: no error, no stray characters;
   - if you have a script saved with Windows line endings (skip if unsure; ask whoever sent
     it), it should come back the same after step 1, and Final
     Draft should open it after step 2.
4. Save in Final Draft, then open that file in Scriptorium. The text should
   match and there should be no warning.
5. Copy three lines from Final Draft and paste them into Scriptorium; note any
   line that gets the wrong type. Then copy three lines from Scriptorium and
   paste them into Final Draft; note what you see (plain text is expected).
6. If you have a script with dual dialogue: editing the text inside it should
   work, and trying to delete a line inside it should be refused with a clear
   message.

Note anything that looks different and send a screenshot.

---

*Optional, for the curious: the app also comes with a `scriptorium` command
for Terminal (section 7, "The `scriptorium` command"); you can ignore it.*

## 9. Trying the AI chat

The chat needs an API key: a password that lets Scriptorium use an AI
service on your account. It costs a little money per message, charged by the
provider (not by us). A few chat messages typically cost cents; the chat
shows the cost as you go. Work on a copy of your script.

**Getting a key (pick one provider).**

- Anthropic (Claude): console.anthropic.com, sign in, add a few dollars of
  credit under Billing, then **API keys › Create key**.
- OpenAI: platform.openai.com, sign in, add credit under Billing, then
  **API keys › Create new secret key**.
- OpenRouter: openrouter.ai, sign in, add credit, then **Keys › Create key**.
  One key reaches many models.

Copy the key when it is shown (you can't see it again). Keep it private.

**Adding it to Scriptorium.**

1. Open Scriptorium and your script. In the menu bar choose
   **Scriptorium › AI accounts…**.
2. Add an account, choose the provider, paste the key and save. Press
   **Check**: it should say the key works.
3. macOS asks whether Scriptorium may use the Keychain (it may ask for your
   Mac's password). Choose **Always Allow**, not "Allow". This app is
   ad-hoc signed, so macOS treats each new version as a different app and
   **may ask again after every update**, and sometimes more than once in a
   row. Choose **Always Allow** each time. If you press "Deny", the key
   isn't saved: open AI accounts and paste it again.
4. The command-line tool and the app are signed separately, so macOS sees
   them as two programs. When one reads a key the other saved, the Keychain
   may ask again; choose **Always Allow** there too.

**If the Keychain keeps misbehaving (fallback).** Quit Scriptorium. In
Terminal run `launchctl setenv SCRIPTORIUM_ANTHROPIC_KEY ` followed by your key
(use `SCRIPTORIUM_OPENAI_KEY` or `SCRIPTORIUM_OPENROUTER_KEY` for the others),
then open Scriptorium again. Scriptorium checks that variable first, before
the Keychain, so while it is set it is the key used. This lasts until you restart the Mac; to clear it sooner run
`launchctl unsetenv SCRIPTORIUM_ANTHROPIC_KEY`. Anyone who can read your
Terminal history can see the key, so use the Keychain when you can.

**Trying the chat.**

- [ ] Press `Cmd+L`. Does the chat column open at the right? Does the
      composer have the cursor? `Cmd+L` again closes it; your draft stays.
- [ ] Type a question ("Who is the most active character in scene 3?")
      and press Return. A card appears first explaining what is sent to the
      provider. Read it, then press **Agree and send**. (**Not now** keeps
      your draft.)
- [ ] Watch the reply stream in. Do the "tool rows" (what the agent read)
      make sense?
- [ ] Ask for a change ("Make MARA's lines in scene 3 shorter").
      Suggestions appear in the sidebar as a review named after the chat.
      Open it, accept one, rewrite one, undo one.
- [ ] With the chat focused, press `Cmd+T`: does the list of conversations
      open? (Outside the chat, `Cmd+T` is **File › New version…**.) Press `Cmd+N`: does a new, empty conversation start?
- [ ] Press `Cmd+.` while it is working: it should stop, and your
      conversation stays.
- [ ] Quit and reopen: is the conversation still in the thread list? (It is
      saved next to your script in a folder named `MY SCRIPT.threads`.)
- [ ] Ask something that needs the whole script ("Summarise the whole
      script"). It should ask first, with a cost estimate.
- [ ] Give a wrong key on purpose: is the error understandable?

Please tell us the cost shown for what you tried, how long replies took,
and anything the agent suggested that was wrong or odd. Chat conversations
contain pieces of your script, so don't send us the `.threads` folder unless
you are happy for us to see them.

Where things live: accounts in
`~/Library/Application Support/scriptorium/accounts.json` (no keys in it),
keys in the Keychain (open Keychain Access and search "scriptorium").
