#!/usr/bin/env bash
# The chat panel's end-to-end scenarios (docs/plans/chat-panel.md, "Headless-Sway
# test plan", S1–S14), replayed from cassettes in a private headless Sway, and
# S15: an outside agent through `scriptorium mcp` with the app opening and
# quitting (docs/plans/agent-connect.md, G3; crates/cli/tests/mcp_link.rs).
#
#   crates/app/tests/scenarios/run.sh              # build, then every scenario
#   crates/app/tests/scenarios/run.sh S5 S11       # only these
#   NO_BUILD=1 crates/app/tests/scenarios/run.sh   # use the binaries already built
#
#   CI=1 crates/app/tests/scenarios/run.sh         # a slow machine: 3× the waits,
#                                                  # software rendering (pixman)
#
# Never touches the user's display, config, keyring or network: Sway runs
# in a private XDG_RUNTIME_DIR, each run in its own folder ($SCENARIO_OUT, or
# a new mktemp -d), each scenario in a folder of its own there, with replies
# from crates/app/tests/cassettes (SCRIPTORIUM_REPLAY), a fixed clock and
# sequential ids (SCRIPTORIUM_TEST_CLOCK) and SCRIPTORIUM_KEYRING=off.
# Screenshots go to $OUT/shots/<scenario>-<step>.png, the app's log and
# files to $OUT/work/<scenario>/. Exit status: 0 when every scenario passes,
# 1 when one fails, 2 when it couldn't run.
#
# Checks are on what the programs leave behind (thread files, review files,
# the session, the app's log and exit code, the CLI's --json output). What
# only the screen shows (focus rings, the sidebar rail, a countdown) is
# listed per scenario as "visual items unverified": look at those shots.
# The places clicked and the colours looked for are named at the top of
# lib.sh. Cassettes come from make_cassettes.py beside this file.
#
# Packages (Debian/Ubuntu names; a CI image needs all of them): sway, wtype,
# grim, jq, imagemagick (7's `magick` or 6's `convert`), dbus (for
# dbus-run-session), util-linux (setsid), mesa-vulkan-drivers (lavapipe:
# GPUI draws with Vulkan, software is enough), fonts-dejavu-core, and a Rust
# toolchain. CI=1 sets WLR_RENDERER=pixman for Sway. There is no CI job yet.
#
# A new scenario: a function named like the others (fresh, app_start, keys,
# check …, visual …, app_quit — see lib.sh), added to ALL. To drive one by
# hand, source this file (it then only defines the functions), then
# sway_start.

set -u
. "$(dirname "${BASH_SOURCE[0]}")/lib.sh"

ALL=(S1 S2 S3 S4 S5 S6 S7 S8 S9 S10 S11 S12 S13 S14 S15)

# ---- S1 toggle ---------------------------------------------------------------------

S1() {
    fresh
    echo closed > "$WORK/config/scriptorium/agent-panel"
    cli suggest notes.review.txt film.fdx --para 1 --after "Laura grins." --why sharper
    cli reviews film.fdx add notes.review.txt
    resolution 1500x1000
    # With a review attached the app opens on it, the card focused.
    app_start || return
    shot 01-review-closed
    ctrl l
    check "Ctrl+L opens the panel (remembered)" wait_for 10 file_has config/scriptorium/agent-panel open
    type_text "a draft"
    shot 02-review-1500-rail-typed
    # Closing on Ctrl+L happens only from the composer: so it had the focus.
    ctrl l
    check "Ctrl+L again (from the composer) closes it" wait_for 10 file_has config/scriptorium/agent-panel closed
    key -k a
    check "focus went back to the card (A accepts)" \
        wait_for 5 file_re film.scriptorium.json '"review/[^/"]*/0","from":null,"to":"accepted'
    key -k u
    ctrl 0
    ctrl l
    check "opens in the script view" wait_for 10 file_has config/scriptorium/agent-panel open
    shot 03-script-1500
    ctrl l
    check "closes in the script view" wait_for 10 file_has config/scriptorium/agent-panel closed
    type_text "Q"
    check "focus went back to the editor (a key edits the script)" \
        wait_for 5 file_has film.scriptorium.json '"inserted":[{"text":"Q"'
    ctrl z
    ctrl 1
    resolution 1700x1000
    ctrl l
    shot 04-review-1700-no-rail
    ctrl l
    # Narrow windows, review view, panel open (agent_panel::columns): at
    # 1100 the panel and card narrow beside the page at a readable zoom; at
    # 1070 the card is at its narrowest; at 900 the page gives way to the
    # card, except while rewriting (E), when the card gives way instead; at
    # the minimum (768) the same, the status line ending in "…". Nothing is
    # cut off (Send and × show).
    ctrl 1
    resolution 1100x1000
    ctrl l
    check "the panel is open at 1100 px" wait_for 10 file_has config/scriptorium/agent-panel open
    shot 05-review-1100-panel
    resolution 1070x1000
    shot 06-review-1070-panel
    resolution 900x1000
    shot 07-review-900-panel
    # Rewriting at 900 px: Ctrl+L from the composer closes the panel and
    # focuses the card (checked above), E starts the rewrite, Ctrl+L opens
    # the panel again with the rewrite going on.
    ctrl l
    check "the panel closes at 900 px" wait_for 10 file_has config/scriptorium/agent-panel closed
    key -k e
    ctrl l
    check "and opens again" wait_for 10 file_has config/scriptorium/agent-panel open
    shot 08-review-900-rewriting
    # Esc from the composer goes back to the rewrite; Esc again puts it back.
    key -k Escape
    key -k Escape
    resolution 768x1000
    shot 09-review-768-panel
    resolution 1500x1000
    visual "02: the sidebar is a rail at 1500 px in the review view; the composer shows its focus ring"
    visual "04: no rail at 1700 px"
    visual "05–09: nothing cut off (Send, ×, the card's buttons); the page readable or hidden with a note on the card; 08: rewriting keeps the page, the card gives way"
    app_quit 0
}

# ---- S2 click focus -----------------------------------------------------------------

S2() {
    fresh
    app_start "$CASSETTES/s02_click_focus.jsonl" || return
    shot 01-editor-focused
    click_composer
    type_text "Who is Laura"
    shot 02-clicked-typed
    key -k Return
    check "the message went from the clicked composer" wait_run 1
    check "thread t0001 holds it" jq_is "$(thread t0001)" '.thread.messages[0].blocks[0].text == "Who is Laura"'
    shot 03-answered
    app_quit 0
}

# ---- S3 disclosure ------------------------------------------------------------------

S3() {
    fresh "" nodisclosure
    app_start "$CASSETTES/s03_disclosure.jsonl" || return
    ctrl l
    type_text "Who is Laura"
    key -k Return
    check "Enter shows the disclosure instead of sending" log_has "agent composer: disclosure first"
    check "nothing is sent yet" test ! -e "$WORK/film.threads/t0001.json"
    shot 01-card-draft-intact
    # The card answers only by its buttons: Tab to Not now, Tab to Agree and send.
    key -k Tab
    key -k Tab
    shot 02-agree-focused
    key -k Return
    check "Agree and send sends the draft" wait_run 1
    check "the draft as typed is the first message" jq_is "$(thread t0001)" '.thread.messages[0].blocks[0].text == "Who is Laura"'
    check "the disclosure is remembered" file_has config/scriptorium/seen.json film.fdx
    focus_composer
    type_text "And Cesar"
    key -k Return
    check "a second send goes straight out" wait_run 2
    check "only one disclosure card" test "$(applog | grep -c 'disclosure first')" = 1
    shot 03-second-sent
    app_quit 0
}

# ---- S4 streaming -------------------------------------------------------------------

S4() {
    fresh
    # Real time: the stream is meant to be slow.
    TIME_SCALE=1 app_start "$CASSETTES/s04_streaming.jsonl" || return
    ctrl l
    type_text "Summarise the script"
    key -k Return
    sleep 1
    shot 01-mid-stream
    check "the run ends" wait_run 1 30
    shot 02-end
    local want
    want=$(grep -o '"text": "[^"]*"' "$CASSETTES/s04_streaming.jsonl" | sed 's/"text": "//; s/"$//' | tr -d '\n')
    check "the thread file holds the whole streamed text" \
        jq_is "$(thread t0001)" ".thread.messages[1].blocks[0].text == \"$want\""
    visual "01: the reply mid-stream; 02: the end reads as the thread file"
    app_quit 0
}

# ---- S5 tool rows → review ----------------------------------------------------------

S5() {
    fresh
    app_start "$CASSETTES/s05_tool_rows.jsonl" || return
    ctrl l
    type_text "Tighten the farm house scene"
    key -k Return
    check "the run ends" wait_run 1
    local review="Tighten the farm house scene.review.txt"
    check "the thread's own review: $review" file_has "$review" "Id: thread-t0001"
    check "with both suggestions" test "$(grep -c '^AFTER:' "$WORK/$review")" = 2
    check "the thread records it" jq_is "$(thread t0001)" ".review_path == \"$review\""
    check "the session attaches it (the sidebar lists it)" \
        wait_for 5 jq_is film.scriptorium.json '[.reviews[].id] | index("thread-t0001") != null'
    shot 01-rows
    click_link
    shot 02-link-card
    key -k a
    check "A on the card accepts the first suggestion" \
        wait_for 5 file_has film.scriptorium.json '"review/thread-t0001/0","from":null,"to":"accepted'
    shot 03-accepted
    app_quit 0
}

# ---- S6 detach and re-attach -----------------------------------------------------

S6() {
    fresh
    app_start "$CASSETTES/s06_reattach.jsonl" || return
    send "Tighten the smile" 1
    local review="Tighten the smile.review.txt"
    check "the first round attaches the thread's review" \
        wait_for 5 jq_is film.scriptorium.json '[.reviews[].id] | index("thread-t0001") != null'
    # Detached by another program (the CLI); the app's watcher picks it up.
    cli reviews film.fdx remove "$review"
    check "scriptorium reviews remove detaches it (exit $CLI_CODE)" test "$CLI_CODE" = 0
    check "the session no longer lists it" \
        wait_for 5 jq_is film.scriptorium.json '[.reviews[].id] | index("thread-t0001") == null'
    sleep 1.5
    shot 01-detached
    send "One more" 2
    check "the same file gets the second suggestion" test "$(grep -c '^AFTER:' "$WORK/$review")" = 2
    check "no second file (… 2.review.txt)" test "$(ls "$WORK" | grep -c 'review.txt$')" = 1
    check "it is attached again" \
        wait_for 5 jq_is film.scriptorium.json '[.reviews[] | select(.id == "thread-t0001") | .path] == ["'"$review"'"]'
    shot 02-reattached
    app_quit 0
}

# ---- S7 stop ------------------------------------------------------------------------

S7() {
    fresh
    TIME_SCALE=1 app_start "$CASSETTES/s07_stop.jsonl" || return
    ctrl l
    type_text "Write a long answer"
    key -k Return
    sleep 2.5
    shot 01-mid-stream
    key -M ctrl -k period -m ctrl
    check "Ctrl+. stops the run" wait_run 1
    check "it ended Stopped" log_has "agent: run ended: Stopped"
    shot 02-stopped
    check "the partial answer is kept" jq_is "$(thread t0001)" \
        '.thread.messages[1].blocks[0].text | startswith("This is a very long answer.") and length < 1680'
    type_text "Shorter"
    key -k Return
    check "the next send works" wait_run 2
    check "and is answered" jq_is "$(thread t0001)" '.thread.messages[-1].blocks[0].text == "Short answer."'
    shot 03-after-stop
    app_quit 0
}

# ---- S8 retry and errors ------------------------------------------------------------

S8() {
    fresh
    # Waits at 0.3×: the 10 s retry-after shows as a countdown for 3 s.
    TIME_SCALE=0.3 app_start "$CASSETTES/s08_errors.jsonl" || return
    ctrl l
    type_text "First"
    key -k Return
    sleep 1.2
    shot 01-countdown
    check "529, then the retry is answered" wait_run 1
    check "…with the reply" jq_is "$(thread t0001)" '.thread.messages[1].blocks[0].text == "First answer."'
    type_text "Second"
    key -k Return
    check "401 ends the run as an auth error" wait_run 2
    check "…kind Auth" log_has "run ended: Error(Api { status: Some(401), kind: Auth"
    shot 02-auth
    type_text "Third"
    key -k Return
    check "500 through three retries ends the run" wait_run 3 20
    check "…kind Other" log_has "run ended: Error(Api { status: Some(500), kind: Other"
    shot 03-other
    click_retry
    check "Retry: a malformed line ends it as a parse error" wait_run 4
    check "…Parse" log_has "run ended: Error(Parse"
    shot 04-parse
    click_retry
    check "Retry: offline through the retries" wait_run 5 20
    check "…kind Offline" log_has "run ended: Error(Api { status: None, kind: Offline"
    # The log line is written just before the panel redraws: wait for the row.
    check "the offline error row shows" wait_for 5 has_button "$WARN_RGB"
    shot 05-offline
    click_retry
    check "Retry succeeds" wait_run 6
    check "…with the reply" jq_is "$(thread t0001)" '.thread.messages[-1].blocks[0].text == "Third answer, after Retry."'
    shot 06-retried
    # A thread file that can't be read: listed, greyed.
    printf 'not a thread' > "$WORK/film.threads/t0002.json"
    ctrl t
    shot 07-list-unreadable
    key -k Escape
    visual "01: the retry countdown"
    visual "02–05: the auth, server, parse and offline error rows"
    visual "06: \"No reply\" under \"Second\""
    visual "07: the unreadable thread greyed in the list"
    app_quit 0
}

# ---- S9 whole-script confirm --------------------------------------------------------

S9() {
    fresh small
    app_start "$CASSETTES/s09_whole_small.jsonl" || return
    send "Read it all" 1
    check "a short script: read_script runs without asking" sh -c "! grep -q 'agent: asks' '$WORK/state/scriptorium/scriptorium.log'"
    check "and the agent got the script" jq_is "$(thread t0001)" \
        '[.thread.messages[].blocks[] | select(.type == "tool_result") | .content] | any(contains("Laura smiles."))'
    shot 01-small
    app_quit 0

    fresh big
    big_script > "$WORK/film.fdx"
    app_start "$CASSETTES/s09_whole_big.jsonl" || return
    ctrl l
    type_text "Read it all"
    key -k Return
    check "a long script: the app asks" wait_for 10 log_has "agent: asks"
    shot 02-asks
    # The card's buttons follow the field: Tab to Not now.
    key -k Tab
    key -k Return
    check "Not now: the run goes on and ends" wait_run 1
    check "the answer is stored" jq_is "$(thread t0001)" '.whole_script == false'
    shot 03-not-now
    type_text "Try again"
    key -k Return
    check "the second read_script ends too" wait_run 2
    check "without asking again" test "$(applog | grep -c 'agent: asks')" = 1
    shot 04-second
    app_quit 0
}

# A script well over the whole-script threshold (20k tokens).
big_script() {
    printf '<?xml version="1.0" encoding="UTF-8" standalone="no" ?>\n<FinalDraft DocumentType="Script" Template="No" Version="5">\n  <Content>\n'
    for i in $(seq 300); do
        printf '    <Paragraph Type="Scene Heading"><Text>INT. ROOM %d - DAY</Text></Paragraph>\n' "$i"
        printf '    <Paragraph Type="Action"><Text>Laura smiles. She crosses the room, opens the window and looks out at the fields for a long while, thinking of %d things.</Text></Paragraph>\n' "$i"
        printf '    <Paragraph Type="Character"><Text>CESAR</Text></Paragraph>\n'
        printf '    <Paragraph Type="Dialogue"><Text>I am go to the town tomorrow, and the day after, and the day after that one too.</Text></Paragraph>\n'
    done
    printf '  </Content>\n</FinalDraft>\n'
}

# ---- S10 accounts sheet -------------------------------------------------------------

S10() {
    fresh
    resolution 1500x1000
    app_start || return
    click $FILE_MENU
    click $MENU_ACCOUNTS
    shot 01-sheet
    click $SHEET_CUSTOM
    click $SHEET_BASE_URL
    type_text "http://localhost:1234/v1"
    click $SHEET_MODEL
    type_text "qwen3"
    click $SHEET_MORE
    click $SHEET_ADVANCED # where a key can come from instead of the keychain
    shot 02-form-env-notice
    # Save: the last filled accent button (it moves as the notices change).
    click_button "$ACCENT_RGB" "the sheet's Save"
    check "the account is saved to the config in scratch" wait_for 5 jq_is config/scriptorium/accounts.json \
        '.accounts[] | select(.id == "custom") | .provider == "custom" and .base_url == "http://localhost:1234/v1" and .model == "qwen3"'
    check "with no key in it" sh -c "! grep -qi 'key' '$WORK/config/scriptorium/accounts.json'"
    cli accounts list --json
    echo "$CLI_OUT" > "$WORK/accounts.json"
    check "the CLI lists it" jq_is accounts.json '[.. | objects | select(.id? == "custom")] | length > 0'
    shot 03-saved
    key -k Escape
    visual "02: the env-var notice under Advanced (SCRIPTORIUM_CUSTOM_KEY)"
    visual "03: the saved account in the list"
    app_quit 0
}

# ---- S11 CLI/app parity -------------------------------------------------------------

S11() {
    fresh cli
    local cli_work=$WORK review="Tighten the farm house scene.review.txt"
    CASSETTE=$CASSETTES/s11_parity.jsonl cli chat film.fdx --json "Tighten the farm house scene"
    echo "$CLI_OUT" > "$WORK/chat.json"
    check "scriptorium chat finishes (exit $CLI_CODE)" test "$CLI_CODE" = 0
    check "its JSON: finished, thread t0001, suggestions 1 and 2 in thread-t0001" jq_is chat.json \
        '.outcome == "finished" and .thread == "t0001" and .review_path == "'"$review"'"
         and .added == [{"review_id": "thread-t0001", "numbers": [1, 2]}] and .error == null'

    fresh app
    local app_work=$WORK
    app_start "$CASSETTES/s11_parity.jsonl" || return
    send "Tighten the farm house scene" 1
    wait_for 5 jq_is film.scriptorium.json '[.reviews[].id] | index("thread-t0001") != null'
    shot 01-app
    app_quit 0

    check "identical review files" cmp -s "$cli_work/$review" "$app_work/$review"
    # Thread files: byte for byte (the test clock fixes times and ids); if not,
    # the difference, with times taken out, is in s11-thread.diff.
    if ! cmp -s "$cli_work/film.threads/t0001.json" "$app_work/film.threads/t0001.json"; then
        diff <(jq -S 'del(.created, .updated)' "$cli_work/film.threads/t0001.json") \
            <(jq -S 'del(.created, .updated)' "$app_work/film.threads/t0001.json") > "$OUT/work/S11-thread.diff"
        FAILS+=("thread files differ (CLI vs app): see $OUT/work/S11-thread.diff")
    else log "ok   identical thread files"; fi
    check "the sessions attach the same review" test \
        "$(jq -c '[.reviews[] | {path, id, text_hash}]' "$cli_work/film.scriptorium.json")" = \
        "$(jq -c '[.reviews[] | {path, id, text_hash}]' "$app_work/film.scriptorium.json")"
}

# ---- S12 the CLI alongside ---------------------------------------------------------

S12() {
    fresh
    printf '// Nothing may be sent.\n' > "$WORK/none.jsonl"
    TIME_SCALE=1 app_start "$CASSETTES/s12_app_slow.jsonl" || return
    # 1. scriptorium chat on a new thread while the app is open.
    CASSETTE=$CASSETTES/s12_cli_new.jsonl cli chat film.fdx --new --json "Fix the grammar"
    echo "$CLI_OUT" > "$WORK/cli-new.json"
    check "scriptorium chat --new finishes beside the app (exit $CLI_CODE)" jq_is cli-new.json \
        '.outcome == "finished" and .thread == "t0001" and .added == [{"review_id": "thread-t0001", "numbers": [1]}]'
    sleep 2.5 # the app's watcher looks every second
    check "the app keeps the CLI's review attached" jq_is film.scriptorium.json '[.reviews[].id] | index("thread-t0001") != null'
    shot 01-sidebar-shows-cli-review
    ctrl l
    ctrl t
    shot 02-list-shows-cli-thread
    key -k Escape
    # 2. The app runs a new conversation slowly; the CLI is refused on it.
    ctrl l
    type_text "Think slowly"
    key -k Return
    check "the app's run starts" wait_for 5 test -e "$WORK/film.threads/t0002.json"
    sleep 0.5
    local t0=$SECONDS
    CASSETTE=$WORK/none.jsonl cli chat film.fdx --thread t0002 --json "Me too"
    echo "$CLI_OUT" > "$WORK/cli-busy.json"
    check "the CLI is refused on the thread the app is running (exit $CLI_CODE, after $((SECONDS - t0)) s)" \
        test "$CLI_CODE" = 3 -a $((SECONDS - t0)) -ge 2
    check "its JSON says busy" jq_is cli-busy.json '.outcome == "busy"'
    shot 03-app-running
    check "the app's run ends" wait_run 1 20
    # 3. The CLI runs t0001 slowly; the panel opens it read-only and won't send.
    (cd "$WORK" && run_env TIME_SCALE=1 SCRIPTORIUM_TEST_TIME_SCALE=1 SCRIPTORIUM_REPLAY="$CASSETTES/s12_cli_slow.jsonl" \
        "$CLI" chat film.fdx --thread t0001 --json "Take your time" > cli-slow.json 2> cli-slow.err
        echo $? > cli-slow.exit) &
    sleep 1.5
    ctrl t
    shot 04-list-cli-running
    key -k Down
    key -k Return
    shot 05-opened-read-only
    type_text "Can I"
    key -k Return
    check "the panel won't run the thread the CLI is running" test "$(applog | grep -c 'agent: run on')" = 1
    check "the CLI's run finishes" wait_for 20 test -f "$WORK/cli-slow.exit"
    check "…exit 0" test "$(cat "$WORK/cli-slow.exit" 2> /dev/null)" = 0
    check "nothing typed in the read-only panel reached the thread" sh -c "! grep -q 'Can I' '$WORK/film.threads/t0001.json'"
    visual "01: the sidebar lists the CLI's review"
    visual "02/04: the thread list (\"In use elsewhere\" in 04)"
    visual "05: the CLI's thread read-only, its note shown once"
    app_quit 0
}

# ---- S13 undo -----------------------------------------------------------------------

S13() {
    fresh
    app_start "$CASSETTES/s05_tool_rows.jsonl" || return
    send "Tighten the farm house scene" 1
    local review="Tighten the farm house scene.review.txt"
    cp "$WORK/$review" "$WORK/review.before"
    click_link
    key -k a
    check "A accepts (the card has the focus)" \
        wait_for 5 file_has film.scriptorium.json '"review/thread-t0001/0","from":null,"to":"accepted'
    sleep 1
    cp "$WORK/film.scriptorium.json" "$WORK/session.accepted"
    ctrl l
    type_text "hello there"
    shot 01-typed
    ctrl z
    shot 02-ctrl-z
    sleep 1.5
    check "Ctrl+Z in the composer leaves the script's decisions alone" cmp -s "$WORK/film.scriptorium.json" "$WORK/session.accepted"
    type_text "more text"
    click $FILE_MENU
    click $MENU_UNDO
    check "File › Undo with the composer focused goes to the composer" log_has "agent panel: menu undo in the composer"
    shot 03-menu-undo-composer
    sleep 1.5
    check "…and leaves the decisions alone" cmp -s "$WORK/film.scriptorium.json" "$WORK/session.accepted"
    key -k Escape # focus back to the card
    click $FILE_MENU
    click $MENU_UNDO
    check "File › Undo with the card focused undoes the accept" \
        wait_for 5 sh -c "! cmp -s '$WORK/film.scriptorium.json' '$WORK/session.accepted'"
    shot 04-menu-undo-card
    check "the agent's suggestions remain" cmp -s "$WORK/$review" "$WORK/review.before"
    check "the thread's review is still attached" jq_is film.scriptorium.json '[.reviews[].id] | index("thread-t0001") != null'
    visual "02: Ctrl+Z emptied the composer"
    visual "03: the menu undo emptied it too"
    visual "04: the accept undone on the card"
    app_quit 0
}

# ---- S14 rewrite in progress --------------------------------------------------------

S14() {
    fresh
    cli suggest notes.review.txt film.fdx --para 3 --after "I am going to the town." --why grammar
    cli reviews film.fdx add notes.review.txt
    app_start "$CASSETTES/s14_rewriting.jsonl" || return
    # The app opens on the review, its card on line 3: E starts rewriting it.
    key -k e
    shot 01-rewriting
    ctrl l
    type_text "Fix the grammar"
    key -k Return
    check "the run ends" wait_run 1
    local review="Fix the grammar.review.txt"
    check "the proposal on the line being rewritten is refused" jq_is "$(thread t0001)" \
        '[.thread.messages[].blocks[] | select(.type == "tool_result") | .content] | any(contains("editing this line"))'
    check "the other one is added" file_has "$review" "PARA: 7"
    check "line 3 isn't in the thread's review" sh -c "! grep -q '^PARA: 3' '$WORK/$review'"
    shot 02-refused
    app_quit 0
}

# ---- S15 an outside agent and the app ---------------------------------------------

# The connector (`scriptorium mcp`) driven as an agent drives it, while the
# app opens, quits and opens again: crates/cli/tests/mcp_link.rs, run in this
# Sway. It starts and stops the app itself, with its own HOME and
# XDG_RUNTIME_DIR (so the agent socket is private); its checks print as
# "ok"/"FAIL" lines, kept in $WORK/mcp_link.log.
S15() {
    fresh
    env -u DISPLAY -u WAYLAND_DISPLAY -u SWAYSOCK \
        SCRIPTORIUM_E2E_WAYLAND="$RUNTIME/$W" SCRIPTORIUM_E2E_APP="$APP" SCRIPTORIUM_E2E_CLI="$CLI" \
        SCRIPTORIUM_E2E_OUT="$WORK/e2e" SCRIPTORIUM_E2E_SHOTS="$SHOTS/S15-" WAIT_SCALE="$WAIT_SCALE" \
        "$MCP_LINK" --exact the_connector_follows_the_app_as_it_opens_and_quits --nocapture \
        > "$WORK/mcp_link.log" 2>&1
    local code=$? line
    grep -E '^  (ok|FAIL) ' "$WORK/mcp_link.log" | cut -c1-160 | while IFS= read -r line; do log "${line#  }"; done
    while IFS= read -r line; do FAILS+=("${line#  FAIL }"); done < <(grep -E '^  FAIL ' "$WORK/mcp_link.log" | cut -c1-200)
    if [ "$code" != 0 ] && ! grep -q '^  FAIL ' "$WORK/mcp_link.log"; then
        FAILS+=("mcp_link exited $code (see $WORK/mcp_link.log)")
    fi
    visual "01: the review finished, no card; 02: the agent's suggestion #2 on the card, the status bar saying an AI agent added it"
    visual "03: reopened on the review, the card on #2, three suggestions in the sidebar"
}

# ---- the run ------------------------------------------------------------------------

# send TEXT N: focus the composer, send TEXT and wait for the N-th run to end.
send() {
    focus_composer
    type_text "$1"
    key -k Return
    check "\"$1\" sent and answered" wait_run "$2"
}

# wait_run N [seconds]: until the app's log shows N runs ended.
wait_run() { wait_for "${2:-15}" eval "[ \$(applog | grep -c 'agent: run ended') -ge $1 ]"; }


main() {
    local picked=("$@")
    [ ${#picked[@]} -eq 0 ] && picked=("${ALL[@]}")
    for s in "${picked[@]}"; do
        declare -F "$s" > /dev/null || { echo "no scenario $s (have: ${ALL[*]})" >&2; exit 2; }
    done
    for t in sway swaymsg wtype grim jq dbus-run-session setsid; do
        command -v "$t" > /dev/null || { echo "needs $t" >&2; exit 2; }
    done
    command -v magick > /dev/null || command -v convert > /dev/null || { echo "needs ImageMagick" >&2; exit 2; }
    mkdir -p "$SHOTS" "$OUT/work" "$BIN"
    rm -f "$SHOTS"/*.png
    echo "scenario output: $OUT"
    if [ -z "${NO_BUILD:-}" ]; then
        echo "building (CARGO_TARGET_DIR=$TARGET)…"
        # Two cargo calls, not one: built together, cargo unifies features
        # and the CLI would get what only the app's dependencies turn on
        # (serde_json's preserve_order, from GPUI), hiding S11's differences.
        (cd "$ROOT" && CARGO_TARGET_DIR=$TARGET cargo build -q -p scriptorium-cli --bins) || exit 2
        cp "$TARGET/debug/scriptorium" "$CLI.new" || exit 2
        (cd "$ROOT" && CARGO_TARGET_DIR=$TARGET cargo build -q -p scriptorium-app --bins --example vpointer) || exit 2
        mv "$CLI.new" "$CLI"
        # S15's driver: the CLI's end-to-end test, built on its own as the
        # CLI is.
        local test_exe
        test_exe=$(cd "$ROOT" && CARGO_TARGET_DIR=$TARGET cargo test -q -p scriptorium-cli --test mcp_link --no-run \
            --message-format=json | jq -r 'select(.target.name == "mcp_link" and .executable != null) | .executable') || exit 2
        cp "$test_exe" "$MCP_LINK" || exit 2
    else
        cp "$TARGET/debug/scriptorium" "$CLI" || exit 2
        # The newest build of the test (its file names end in a hash).
        cp "$(ls -t "$TARGET"/debug/deps/mcp_link-* | grep -v '\.d$' | head -1)" "$MCP_LINK" || exit 2
    fi
    cp "$TARGET/debug/scriptorium-app" "$APP" && cp "$TARGET/debug/examples/vpointer" "$VPOINTER" || exit 2
    # Before anything starts: whatever happens, the compositor, the app and
    # the binaries' copies go.
    trap 'app_kill; sway_stop; rm -rf "$BIN"' EXIT
    sway_start 1500x1000
    local results=() failed=0 unverified=0
    for s in "${picked[@]}"; do
        SCEN=$s FAILS=() VISUAL=() WORK=""
        echo "$s"
        "$s"
        # Never leave an app behind for the next scenario.
        [ -n "$WORK" ] && app_kill
        local n=${#VISUAL[@]} also=""
        [ "$n" -gt 0 ] && also=" ($n visual item$([ "$n" = 1 ] || echo s) unverified)"
        unverified=$((unverified + n))
        if [ ${#FAILS[@]} -eq 0 ]; then
            results+=("PASS  $s$also")
        else
            failed=1
            results+=("FAIL  $s$also")
            for f in "${FAILS[@]}"; do results+=("        - $f"); done
        fi
        for v in "${VISUAL[@]}"; do results+=("        look: $v"); done
    done
    echo
    echo "==== chat panel scenarios ===="
    printf '%s\n' "${results[@]}"
    echo "shots in $SHOTS; $unverified visual item(s) to check by eye"
    exit $failed
}

# Sourced (to drive a scenario step by step), it only defines the functions.
[ "${BASH_SOURCE[0]}" = "$0" ] && main "$@"
