---
name: script-review
description: Reviewing or revising a screenplay (Final Draft .fdx) with Scriptorium — proposing line changes for the writer to decide, a dialogue or grammar pass, checking a review's progress, or applying decisions. Use whenever the user asks for notes, fixes or a pass on their script.
---

# Reviewing a screenplay with Scriptorium

The writer decides; you propose. Scriptorium shows each suggestion inside the
script, and the writer accepts one of your options, keeps their line, or
rewrites it. Work through the `scriptorium` tools; never edit the .fdx file
yourself (Scriptorium preserves Final Draft's formatting byte for byte).

## A pass on the script

1. `script_info` for the shape of the script, then `list_paragraphs` filtered
   by scene, speaker, kind or text. Don't page through the whole script unless
   the pass needs it.
2. For each line worth changing, `suggest_change` with the paragraph id, one to
   three replacement options (whole line, best first) and a short `why`. Put a
   pass in one review file named for it, e.g. `dialogue-pass.review.txt`, next
   to the script.
3. `attach_reviews` with that file once, so it appears in the app.
4. Tell the writer how many suggestions you made and that they're waiting in
   Scriptorium. Don't decide them yourself.

Keep suggestions to one paragraph each, and keep the writer's voice: change
what the pass is about and nothing else. Character names (Character elements)
are shown in capitals; a change that only alters letter case there is refused
as invisible.

## When the writer asks you to decide or edit

`list_suggestions` (with `pending_only`) shows what's left, numbered from 1.
`decide_suggestion` accepts an option, keeps the line, or rewrites it.
`edit_paragraph` changes a line directly. Everything is undoable with `undo`,
and the app sees it immediately. `write_script` writes the revised script as a
new .fdx; the original is never touched.
