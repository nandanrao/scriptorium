---
name: script-review
description: Reviewing or revising a screenplay (Final Draft .fdx) with Scriptorium — proposing changes for the writer to decide, a dialogue or grammar pass, checking a review's progress, or applying decisions. Use whenever the user asks for fixes, changes or a pass on their script.
---

# Reviewing a screenplay with Scriptorium

The writer decides; you propose. Work through the `scriptorium` tools and never
edit the .fdx file yourself (Scriptorium keeps Final Draft's formatting byte
for byte).

Before starting, make sure you have the script's path; if you don't, ask the
writer for it, and pass it as an absolute path. If the writer asks for
feedback, notes or opinions rather than changes, answer in chat and don't
propose anything.

## Running a pass

1. Get the shape of the script, then read what the pass needs: one scene at a
   time for focused work, filtered paragraph lists to find lines across the
   script, the whole script (it comes in parts) only when the pass covers all
   of it. Reads always show the script as it is now, including the writer's
   decisions, so read again rather than trusting what you read earlier.
2. Send the pass in one batch, into one review file named for it, e.g.
   `dialogue-pass.review.txt`. Always give `review_file`; it lives next to the
   script and is created and attached for you. It must be a `.review.txt` in
   the script's folder (or a folder inside it), or a review already attached;
   other paths are refused. Check the result for lines that
   failed, and fix or drop them.
3. Tell the writer briefly what you proposed, without repeating the proposals,
   and that they can open the script in the Scriptorium app to decide; it
   picks up your changes as they happen.

Change what the pass is about and nothing else; keep the writer's voice.

Use the smallest unit the writer can judge on its own: one line when one line
changes (`paragraph_id`, options as strings). Group lines that only make sense
together (a cue and its dialogue, an exchange, a beat) into one suggestion with
`paragraph_ids` (every id of the lines, consecutive); the writer takes it whole, and
each option is a list of `{kind, text}` lines. Use `insert_after` to add lines;
an option `[]` cuts them. At most 3 options and 40 lines a side; a line is
in at most one suggestion per review.

## When the writer asks you to decide or edit

Only then decide suggestions or edit lines yourself; refer to a suggestion by
its stable `"e<entry>"` if others may have been added since you listed them.
Everything can be undone, and the app sees it immediately. `write_script`
writes the revised script as a new `<name> (scriptorium).fdx` next to the
original, replacing an earlier one of that name; the original is never
touched.
