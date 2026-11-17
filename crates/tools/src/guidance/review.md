# Reviewing a screenplay with Scriptorium

The writer decides; you propose. Work through the Scriptorium tools and never
edit the .fdx file yourself (Scriptorium keeps Final Draft's formatting byte
for byte).

Which script: call where_is_writer first. If the writer has the script open in
Scriptorium, leave out `script` and the tools work on the open one, live: the
writer sees each change as you make it. For any other script, pass its absolute
path as `script`; if you don't know it, ask the writer for it, or to open the
script in Scriptorium.

If the writer asks for feedback, notes or opinions rather than changes, answer
in chat and don't propose anything. To translate a script into another
language, call guide('translate') instead.

## Running a pass

1. Get the shape of the script (script_info), then read what the pass needs:
   one scene at a time (read_scene) for focused work, filtered paragraph lists
   (list_paragraphs) to find lines across the script, the whole script
   (read_script, which comes in parts) only when the pass covers all of it.
   where_is_writer tells you the scene the writer is looking at, when they say
   "this scene". Reads always show the script as it is now, including the
   writer's decisions and typing, so read again rather than trusting what you
   read earlier.
2. Check list_suggestions for what is already proposed, to avoid duplicates.
3. Send the pass in one suggest_changes batch, into one review file named for
   it, e.g. `dialogue-pass.review.txt`. Always give `review_file`, and keep the
   same one for the whole pass; it lives next to the script and is created and
   attached for you. It must be a `.review.txt` in the script's folder (or a
   folder inside it); other paths are refused. A file there that doesn't start
   with `SCRIPT REVIEW` must be attached by the writer first. Check the result
   for lines that failed, and fix or drop them.
4. Tell the writer briefly what you proposed, without repeating the proposals.
   The suggestions are in the app's review sidebar for them to decide; if the
   app isn't open, they see them when they open the script.

Change what the pass is about and nothing else; keep the writer's voice,
dialect, tone and formatting. Screenplay conventions are not errors (present
tense in action, fragments, (O.S.), (V.O.), (CONT'D)), and letter case in
headings and cues doesn't show, so a case-only change is refused.

Use the smallest unit the writer can judge on its own: one line when one line
changes (`paragraph_id`, options as strings). Group lines that only make sense
together (a cue and its dialogue, an exchange, a beat) into one suggestion with
`paragraph_ids` (every id of the lines, consecutive); the writer takes it
whole, and each option is a list of `{kind, text}` lines. Use `insert_after` to
add lines; an option `[]` cuts them. At most 3 options and 40 lines a side; a
line is in at most one suggestion per review. Give one option when the change
is obvious, two or three only for a real choice, with a short `why` saying how
they differ.

## When the writer asks you to decide or edit

Only then decide suggestions (decide_suggestion) or edit lines yourself
(edit_paragraph); refer to a suggestion by its stable `"e<entry>"` if others
may have been added since you listed them. Everything can be undone, and the
app shows it at once. undo undoes the last step, whoever made it, so it can
undo the writer's own typing: use it only for a change you just made.
write_script writes the revised script as a new `<name> (scriptorium).fdx`
next to the original, replacing an earlier one of that name; the original is
never touched.
