# Translating a screenplay with Scriptorium

The writer decides; you propose. A translation is a **version**: a second
`.fdx` next to the original, lined up with it row by row through a links file.
You fill it with suggestions in a review file, and the writer accepts, keeps or
rewrites each one in the Scriptorium app. Work only through the Scriptorium
tools: never edit either `.fdx` or the `.links.txt` file yourself.

Which script: call where_is_writer first. If the writer has the script open in
Scriptorium, leave out `script` to work on it; otherwise pass its absolute path,
and if you don't know it, ask the writer for it, or to open it in
Scriptorium.

## 1. The version

1. Call list_versions on the script. It says whether a version is lined up
   with it, in which language, and whether this script is the original or the
   version: `is_version` marks the version (the file that is translated and
   gets suggestions), and the top-level `version` is its absolute path. Note
   the original's `label` (e.g. `English`): it names the review below.
2. If the script is already the version in the language asked for, use it as
   it is.
3. Otherwise call new_version on the original with `language` (the code of the
   language to translate into, e.g. `es`) and `original_language` (the
   original's code, e.g. `en`; tell from the script). It makes the version
   (scene headings and character names copied, every other line blank) or, if
   it already exists, finds it ("already made"). Either way the result has a
   line `The version: <absolute path>`.
4. **From then on, pass that absolute path as `script` in every call**, even
   while the original is open in the app. Suggestions go on the version, never
   on the original.
5. A script has one version for now. If list_versions shows a version in
   another language, tell the writer and stop; don't make a second one.

## 2. The brief and the glossary

Before translating, look in the script's folder for these files and read them
with your own file tools if you have them (Scriptorium doesn't read them); if
you can't read files, ask the writer whether there is a brief:

- `brief.<lang>.md` (e.g. `brief.es.md`): the writer's instructions for this
  language (register, region, how to treat INT./EXT. and times of day, names,
  what not to translate).
- `glossary.md`: terms with their agreed translations.

Follow them over your own preferences. If there is no brief, translate scene
headings' places and times of day, keep the INT./EXT. form usual for the
language, and say so in your report so the writer can correct it.

## 3. Translating, a scene at a time

For each scene asked for (or every scene, in order, if asked for the whole
script):

1. read_rows with `scene` (a number, 1 = the first, or part of its heading)
   and **without** `needs`, so you see the whole scene: every row, its
   original text (`other`), the version's text (`this`), its status and its
   `suggestions`. Translate with the scene's full context, not line by line.
   For a long scene, page with `from_row` and `limit`.
2. Work on the rows with `needs: true`. **Skip any row that already has a
   suggestion of any status** (its `suggestions` list isn't empty: pending,
   accepted, kept, rewritten): the writer has it in hand. A kept row stays as
   the writer left it.
3. Scene headings are copied too, so their rows show the original's text,
   but they read `blank` and need a suggestion like any other line (follow
   the brief for INT./EXT. and times of day). Character cues are copied and
   count as in sync. Still suggest on a cue row when the cue is generic
   ("WOMAN", "POLICEMAN") or carries an extension typed in the cue ("(V.O.)",
   "(O.S.)") that the language writes differently. Never add "(CONT'D)" or
   "(MORE)": Final Draft adds them.
4. Send the scene in one suggest_changes batch, one item per row, naming the
   row's lines on the version's side (its `this` ids from read_rows):
   `{"row": 12, "paragraph_id": 345, "options": ["..."], "why": "..."}`.
   `row` is optional (the row is found from the lines) but keeps you honest.
   An item must cover **all** of its row's `this` lines and nothing else:
   when the row has several, give them all as `paragraph_ids` and each option
   as a list of `{kind, text}` lines. To change the number of lines, use such
   a list option (it may have more or fewer lines); never use `insert_after`
   on a version.
5. Always give `review_file`: `Translation from <label>.review.txt`, with the
   original's `label` as list_versions shows it, e.g.
   `Translation from English.review.txt`. Use the same file for every scene
   and every later session.
6. Check the result for items that failed, and fix or drop them.

**Options.** Give one option when the translation is obvious. Give two or
three only for a real choice (formality, an idiom, a pun, length), with a
short `why` saying how they differ ("1 keeps her defiance; 2 is softer").
Keep the writer's tone, rhythm and line lengths; translate what is there,
don't improve it.

## 4. Updating after the original changed

Rows whose status reads, say, "English changed" or "both changed"
(`other_changed`, `both_changed`; the label uses the original's language) and
that `needs` get a new suggestion, in the **same** review as before. The
earlier suggestion on such a row was for the old original line, so it isn't
found any more and doesn't block the new one. A row whose decided suggestion
still matches the original is refused with "already decided ... nothing to
update": skip it.

## 5. Report to the writer

Tell the writer briefly what you proposed (how many rows, which scenes),
without repeating the translations, and:

- Rows that read "nothing on this side" (a line new in the original, or a
  version line the writer removed): list them with their original text. For a
  new line, the writer adds an empty line of that type in the version in Final
  Draft at that place, saves, and asks you again; you then fill it. If they
  removed it on purpose, nothing to do.
- Rows noted "can't be translated here" (End of Act, Lyrics and other special
  types): the writer changes them in Final Draft.
- The first time: the title page and scene summaries stay in the original's
  language by design (and the original's script notes aren't copied); the
  writer changes them in Final Draft.
- Next steps: open the version in the Scriptorium app; the review is in the
  sidebar. Decide there first. Close the version in Final Draft before
  **Write .fdx**, which saves the version file itself; polish in Final Draft
  only after that.

## Don'ts

- Never edit the `.fdx` files or the `.links.txt` file by hand, or with any
  tool but these.
- Don't call write_script on the version; the writer saves it from the app
  with Write .fdx (the tool refuses without an `output`).
- Don't decide or edit suggestions unless the writer asks you to.
