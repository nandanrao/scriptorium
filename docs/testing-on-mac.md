# Trying Scriptorium on your Mac

Thank you for testing this. Scriptorium is an early version, so things may
be rough. Everything you notice helps, including the small things.

## What's new in 0.5.0

- **Translating with Claude.** Ask Claude (through the plugin) to make a
  Spanish (or other language) version of your script. It makes a second
  Final Draft file next to yours, `MY SCRIPT.es.fdx`, lined up line by line
  with the English, and fills it with suggestions you decide in Scriptorium
  as usual, with the English line shown on each card. See section 7,
  "Translating with Claude".
- **Write .fdx saves the translation itself.** For a version Scriptorium
  made, `Cmd+S` writes that file (`MY SCRIPT.es.fdx`), not a
  `(scriptorium)` copy. Your original is still never changed.
- **The AI chat** (`Cmd+L`, below under 0.4.0) ships in this same release, so
  if you haven't tried it yet, it's new to you too.

## What's new in 0.4.0

- **AI chat.** Press `Cmd+L` to open a chat column at the right. You talk
  to an agent (Claude, GPT and others) about your script, and the changes it
  proposes appear as suggestions on a review card for that conversation, to
  decide as usual. Nothing in your script changes until you accept it.
- **You bring your own API key.** See section 9 for getting one, setting it
  up and the Mac's Keychain questions.
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
- **Update the plugin.** If you had an earlier plugin, delete the old
  `scriptorium-plugin` folder and put the new one from
  `Scriptorium-plugin-0.5.0-mac.zip` in the same place. Run the `xattr`
  step (step 3 above) again on the new folder, start Claude with
  `claude --plugin-dir ` and the folder (step 4), and check with `/mcp` that
  `scriptorium` is connected. Install the new app too: both must be 0.5.0.

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
2. Drag the English `.fdx` into the Claude chat and ask:
   "Make a Spanish version of this script and translate scene 1."
3. Claude makes, next to your script:
   - `MY SCRIPT.es.fdx`: the Spanish version. Scene headings and character
     names are copied from the English (Claude still suggests a translation
     for each scene heading); every other line starts empty.
   - `MY SCRIPT.es.links.txt`: how the lines of the two files line up.
     Leave it alone.
   - `Translation from English.review.txt`: Claude's suggestions.
4. Open `MY SCRIPT.es.fdx` in Scriptorium (**File › Open…**). The review
   "Translation from English" is in the sidebar; press `Cmd+1`. Each card
   shows the English line and Claude's Spanish. Accept, pick another option,
   keep the empty line, or rewrite it, as with any review. Claude gives one
   option when the line is obvious and two or three, with a short note, when
   there's a real choice.
5. Ask Claude for more ("Translate scenes 2 to 5"). It skips lines that
   already have a suggestion, so asking twice doesn't duplicate anything. If
   Scriptorium is open, the new suggestions appear while you watch.

**How to work.**

- **Decide in Scriptorium first, polish in Final Draft after.** When you are
  ready, **close `MY SCRIPT.es.fdx` in Final Draft** if it is open there,
  then press `Cmd+S` (**Write .fdx**) in Scriptorium. That saves the Spanish
  file itself (no `(scriptorium)` copy). Undo starts again from there. Then
  open it in Final Draft to polish, and reopen it in Scriptorium whenever you
  like: Scriptorium finds the lines again.
- Decisions are never lost, but **lines you type straight into the Spanish
  in Scriptorium** (not through a card) are lost if Final Draft saves the
  file before you press Write .fdx. Write first, then go to Final Draft.
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
- **The AI chat in Scriptorium** (`Cmd+L`) can make the version too, but a
  conversation stays on the script it was started on: open
  `MY SCRIPT.es.fdx` and ask there to translate.

**Sharing.** Keep these together in one folder: both scripts, the
`.links.txt` file, the `.review.txt` files and the `.scriptorium.json` files.

**Starting over.** Quit Scriptorium, then delete `MY SCRIPT.es.fdx`,
`MY SCRIPT.es.links.txt`, `MY SCRIPT.es.scriptorium.json` and the
`Translation from English.review.txt` file (and, if there,
`MY SCRIPT.es.scriptorium.lock`, `MY SCRIPT.es.links.lock` and any
`MY SCRIPT.es.scriptorium.set-aside-*.json`). Your English script is untouched; ask Claude again.

**From Terminal** (optional): the plugin folder's `bin/scriptorium` does the
same without Claude. `scriptorium version new "MY SCRIPT.fdx" --lang es
--from en` makes the version (`--from en` names the original as English, so
the review and labels say "English"); `scriptorium rows "MY SCRIPT.es.fdx"
--scene 1` shows each line next to its English and what it needs.

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

*Optional, for the curious: the app also comes with a command-line tool for
AI agents to work on the same script; you can ignore it.*

## 9. Trying the AI chat (0.4.0)

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
      open? Press `Cmd+N`: does a new, empty conversation start?
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
