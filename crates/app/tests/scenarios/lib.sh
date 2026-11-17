# Helpers for the chat-panel Sway scenarios (sourced by run.sh).
#
# Everything that touches a display goes to the private headless Sway this
# file starts ($W), never the user's: every display command below sets
# WAYLAND_DISPLAY=$W on its own command line, and Sway is stopped through its
# own IPC socket.

ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/../../../.." && pwd)
CASSETTES=$ROOT/crates/app/tests/cassettes
TARGET=${CARGO_TARGET_DIR:-$ROOT/target}
# Each run gets its own folder unless SCENARIO_OUT names one, so two runs at
# once never share files.
OUT=${SCENARIO_OUT:-$(mktemp -d "${TMPDIR:-/tmp}/scriptorium-scenarios.XXXXXX")}
mkdir -p "$OUT"
SHOTS=$OUT/shots
# Private copies of the binaries, so a concurrent cargo build sharing the
# target folder can't swap them mid-run (kept beside the target, not in a
# tmpfs). Per run: named after the run's folder.
BIN=$TARGET/scenarios-bin/$(basename "$OUT")
APP=$BIN/scriptorium-app-scen
CLI=$BIN/scriptorium-scen
VPOINTER=$BIN/vpointer-scen
MCP_LINK=$BIN/mcp-link-scen
CLOCK=2026-11-15T09:30:00Z

# CI=1: a slower machine (software rendering): every wait is longer.
if [ -n "${CI:-}" ]; then WAIT_SCALE=${WAIT_SCALE:-3} KEY_PAUSE=${KEY_PAUSE:-1.0}; fi
WAIT_SCALE=${WAIT_SCALE:-1}

# ImageMagick 7 (`magick`) or 6 (`convert`, `identify`).
if command -v magick > /dev/null; then
    im() { magick "$@"; }
    im_width() { magick identify -format %w "$1"; }
else
    im() { convert "$@"; }
    im_width() { identify -format %w "$1"; }
fi

# ---- places and colours ----------------------------------------------------------
#
# Coordinates are in pixels of the 1500×1000 headless output (a grim shot's
# pixels). They hold while the layout they come from doesn't change; when a
# scenario fails to click something, compare its shots with these.

# The File menu (Linux dropdown, menus.rs): its button, then its items.
FILE_MENU="27 18"
MENU_UNDO="41 158"        # Undo, the 4th item (after Open…, Add review…, Write .fdx)
MENU_ACCOUNTS="66 283"    # AI accounts…, after About Scriptorium
# The accounts sheet (agent_panel/accounts.rs, a 440 px column on the right):
SHEET_CUSTOM="1395 249"   # "Add an account": Custom, with Work listed above
SHEET_BASE_URL="1280 259" # the Custom form's fields, top to bottom
SHEET_MODEL="1280 321"
SHEET_MORE="1130 465"     # "More options"
SHEET_ADVANCED="1120 629" # "Advanced", once More options is open
# The composer's text field: this far left of the window's right edge, and
# down from the top (agent_panel/composer.rs, at the foot of the panel).
COMPOSER_FROM_RIGHT=200
COMPOSER_Y=880 # its text line before the first reply (a usage line then lifts it ~20 px)
# theme.rs colours, as "R G B": ACCENT (0x5b3fa3: links, the sheet's Save)
# and WARN (0xa2361f: error rows and their buttons).
ACCENT_RGB="91 63 163"
WARN_RGB="162 54 31"
# The part of a shot the probes read: the right-hand column (panel or sheet).
PROBE=$OUT/probe.png
PROBE_LEFT=0
PROBE_WIDTH=340
PROBE_TOP=76
PROBE_HEIGHT=800

SCEN=""      # the scenario running
WORK=""      # its scratch folder
FAILS=()     # its failed checks
VISUAL=()    # what only its shots show (listed in the summary)

log() { printf '  %s\n' "$*"; }
# visual DESCRIPTION: something to check by eye in this scenario's shots.
visual() { VISUAL+=("$1"); }

# ---- the compositor ------------------------------------------------------------

# A private runtime folder (short: socket paths are limited to 108 bytes), so
# the compositor's sockets can't meet the user's, and nothing assumes
# /run/user/$UID exists.
RUNTIME=""

sway_start() {
    local conf=$OUT/sway.conf
    RUNTIME=$(mktemp -d /tmp/scrt.XXXXXX) && chmod 700 "$RUNTIME"
    export XDG_RUNTIME_DIR=$RUNTIME
    # Sway tells its children which display and IPC socket are its own.
    printf 'output HEADLESS-1 resolution %s\ndefault_border none\nfocus_follows_mouse no\nexec sh -c %s\n' \
        "${1:-1500x1000}" "'echo \"\$WAYLAND_DISPLAY \$SWAYSOCK\" > $RUNTIME/display'" > "$conf"
    env -u WAYLAND_DISPLAY -u SWAYSOCK -u DISPLAY WLR_BACKENDS=headless WLR_LIBINPUT_NO_DEVICES=1 \
        ${CI:+WLR_RENDERER=pixman} sway -c "$conf" > "$OUT/sway.log" 2>&1 &
    SWAY_PID=$!
    wait_for 10 test -s "$RUNTIME/display"
    read -r W SOCK < "$RUNTIME/display" 2> /dev/null
    if [ -z "${W:-}" ] || [ ! -S "${SOCK:-}" ]; then
        echo "could not start the headless Sway (see $OUT/sway.log)" >&2
        exit 2
    fi
    echo "headless Sway: WAYLAND_DISPLAY=$W in $RUNTIME"
}

sway_stop() {
    [ -n "${SOCK:-}" ] && [ -S "$SOCK" ] && SWAYSOCK=$SOCK swaymsg exit > /dev/null 2>&1
    [ -n "${SWAY_PID:-}" ] && wait "$SWAY_PID" 2> /dev/null
    [ -n "$RUNTIME" ] && rm -rf "$RUNTIME"
}

resolution() { SWAYSOCK=$SOCK swaymsg output HEADLESS-1 resolution "$1" > /dev/null; sleep 0.8; }

# ---- input and screenshots -----------------------------------------------------

# Each wtype run drops its first key, so every run leads with a Shift press.
# key: wtype arguments (`key -M ctrl -k l -m ctrl`); type: literal text.
key() { WAYLAND_DISPLAY=$W wtype -k Shift_L -s 120 "$@"; sleep "${KEY_PAUSE:-0.5}"; }
type_text() { WAYLAND_DISPLAY=$W wtype -k Shift_L -d 15 "$1"; sleep "${KEY_PAUSE:-0.4}"; }
ctrl() { key -M ctrl -k "$1" -m ctrl; }
# click X Y [button]: also takes one of the "X Y" places above, unquoted.
click() { WAYLAND_DISPLAY=$W "$VPOINTER" click "$@"; sleep "${KEY_PAUSE:-0.5}"; }
shot() { WAYLAND_DISPLAY=$W grim "$SHOTS/$SCEN-$1.png"; }

# ---- a scenario's folder -------------------------------------------------------

SCRIPT_FDX='<?xml version="1.0" encoding="UTF-8" standalone="no" ?>
<FinalDraft DocumentType="Script" Template="No" Version="5">
  <Content>
    <Paragraph Type="Scene Heading"><Text>EXT. FARM HOUSE - NIGHT</Text></Paragraph>
    <Paragraph Type="Action"><Text>Laura smiles.</Text></Paragraph>
    <Paragraph Type="Character"><Text>CESAR</Text></Paragraph>
    <Paragraph Type="Dialogue"><Text>I am go to the town tomorrow.</Text></Paragraph>
    <Paragraph Type="Scene Heading"><Text>INT. KITCHEN - DAY</Text></Paragraph>
    <Paragraph Type="Action"><Text>Laura pours coffee for two.</Text></Paragraph>
    <Paragraph Type="Character"><Text>LAURA</Text></Paragraph>
    <Paragraph Type="Dialogue"><Text>You will miss the bus.</Text></Paragraph>
  </Content>
</FinalDraft>
'
ACCOUNTS_JSON='{"version": 1, "default": "work",
  "accounts": [{"id": "work", "name": "Work", "provider": "anthropic", "model": "claude-opus-5-5"}]}'

# fresh [name]: a new folder with film.fdx, one account, the panel open and
# the disclosure for film.fdx already accepted (pass `nodisclosure` to keep
# the first-use card).
fresh() {
    WORK=$OUT/work/$SCEN${1:+-$1}
    rm -rf "$WORK"
    mkdir -p "$WORK/config/scriptorium" "$WORK/state"
    printf '%s' "$SCRIPT_FDX" > "$WORK/film.fdx"
    printf '%s\n' "$ACCOUNTS_JSON" > "$WORK/config/scriptorium/accounts.json"
    echo open > "$WORK/config/scriptorium/agent-panel"
    if [ "${2:-}" != nodisclosure ]; then
        printf '{"version": 1, "disclosures": ["https://api.anthropic.com:443\\u001f%s"]}\n' "$WORK/film.fdx" \
            > "$WORK/config/scriptorium/seen.json"
    fi
}

# The environment both programs run with: offline, deterministic, nothing of
# the user's (config, state, keyring).
run_env() {
    env -u DISPLAY -u SCRIPTORIUM_REPLAY -u SCRIPTORIUM_WORK_KEY \
        HOME="$WORK" XDG_CONFIG_HOME="$WORK/config" XDG_STATE_HOME="$WORK/state" \
        SCRIPTORIUM_KEYRING=off SCRIPTORIUM_ANTHROPIC_KEY=test \
        SCRIPTORIUM_TEST_CLOCK=$CLOCK SCRIPTORIUM_TEST_TIME_SCALE=${TIME_SCALE:-0.01} "$@"
}

# app_start [cassette] [VAR=value...]: the app on $WORK/film.fdx (or $APP_ARGS),
# in a process group of its own (its id in app.pid) so it can be stopped
# without touching any other program.
app_start() {
    local cassette=${1:-}
    shift || true
    rm -f "$WORK/app.exit" "$WORK/app.pid"
    (
        cd "$WORK" || exit
        run_env WAYLAND_DISPLAY="$W" ${cassette:+SCRIPTORIUM_REPLAY="$cassette"} "$@" \
            setsid -w sh -c 'echo $$ > app.pid; exec "$@"' sh dbus-run-session -- "$APP" ${APP_ARGS:-film.fdx} \
            > "$WORK/app.log" 2>&1
        echo $? > "$WORK/app.exit"
    ) &
    wait_for 20 app_window || { FAILS+=("the app's window never appeared"); return 1; }
    # Up once the script is open (logged), and drawn a moment later.
    wait_for 10 log_has "INFO opened "
    sleep 0.8
}

# app_kill: stops this scenario's app (its process group), if it runs.
app_kill() {
    local pid
    pid=$(cat "$WORK/app.pid" 2> /dev/null) || return 0
    if app_running && [ -n "$pid" ]; then
        kill -TERM -- "-$pid" 2> /dev/null
        wait_for 5 test -f "$WORK/app.exit"
    fi
}

app_window() { SWAYSOCK=$SOCK swaymsg -t get_tree 2> /dev/null | grep -q '"pid": [0-9]'; }

app_running() { [ ! -f "$WORK/app.exit" ]; }

# app_quit [expected exit code]: Ctrl+Q, then check the exit code (3 means
# the cassette had exchanges left over).
app_quit() {
    local want=${1:-0} code
    app_running || { check_exit "$want"; return; }
    key -k Escape
    ctrl q
    if ! wait_for 15 test -f "$WORK/app.exit"; then
        FAILS+=("the app didn't quit on Ctrl+Q")
        app_kill
    fi
    check_exit "$want"
}

check_exit() {
    local code
    code=$(cat "$WORK/app.exit" 2> /dev/null)
    if [ "$code" != "$1" ]; then
        local why=""
        [ "$code" = 3 ] && why=" (cassette exchanges left over)"
        FAILS+=("app exit code $code, expected $1$why: $(grep -m2 -i 'cassette\|exchange\|transport' "$WORK/app.log" | tr '\n' ' ')")
    fi
    if grep -q PANIC "$WORK/state/scriptorium/scriptorium.log" 2> /dev/null; then
        FAILS+=("the app panicked: $(grep -m1 -A2 PANIC "$WORK/state/scriptorium/scriptorium.log" | tr '\n' ' ')")
    fi
}

# cli ARGS...: the CLI in $WORK with the same environment; stdout in $CLI_OUT,
# stderr in $CLI_ERR, exit code in $CLI_CODE. CASSETTE=... replays.
cli() {
    CLI_OUT=$(cd "$WORK" && run_env ${CASSETTE:+SCRIPTORIUM_REPLAY="$CASSETTE"} "$CLI" "$@" 2> "$WORK/cli.err" < /dev/null)
    CLI_CODE=$?
    CLI_ERR=$(cat "$WORK/cli.err")
}

# ---- waiting and checking ------------------------------------------------------

# wait_for SECONDS CMD...: until CMD succeeds (polled every 0.2 s; SECONDS
# times WAIT_SCALE).
wait_for() {
    local n=$(($1 * 5 * WAIT_SCALE))
    shift
    for _ in $(seq "$n"); do
        "$@" && return 0
        sleep 0.2
    done
    return 1
}

# check DESCRIPTION CMD...: records a failure if CMD fails.
check() {
    local what=$1
    shift
    if "$@"; then log "ok   $what"; else
        log "FAIL $what"
        FAILS+=("$what")
    fi
}

applog() { cat "$WORK/state/scriptorium/scriptorium.log" 2> /dev/null; }
log_has() { applog | grep -q -- "$1"; }
file_has() { grep -qF -- "$2" "$WORK/$1" 2> /dev/null; }
# jq_is FILE FILTER: the filter yields true.
jq_is() { jq -e "$2" "$WORK/$1" > /dev/null 2>&1; }
thread() { echo "film.threads/$1.json"; }

# probe: a fresh shot of the screen into $PROBE, and the left edge of the
# right-hand column in it ($PROBE_LEFT).
probe() {
    WAYLAND_DISPLAY=$W grim "$PROBE"
    PROBE_LEFT=$(($(im_width "$PROBE") - PROBE_WIDTH - 10))
}

# probe_failed WHAT: records a failure to find WHAT, keeping the shot it was
# looked for in.
probe_failed() {
    local kept=$SHOTS/$SCEN-probe-$(date +%s%N).png
    cp "$PROBE" "$kept"
    FAILS+=("couldn't find $1 on screen (looked in $kept)")
}

# probe_pixels: "x y r g b" for each pixel of the right-hand column.
probe_pixels() {
    im "$PROBE" -crop "${PROBE_WIDTH}x${PROBE_HEIGHT}+$PROBE_LEFT+$PROBE_TOP" -depth 8 txt:- |
        awk -v T="$PROBE_TOP" 'NR > 1 { split($1, xy, "[,:]"); s = $2; gsub(/[()]/, "", s); split(s, c, ",")
                                       print xy[1], xy[2] + T, c[1], c[2], c[3] }'
}

# link_ys: the y of each line of link-coloured text (ACCENT) in the panel,
# top to bottom, in the last probe (the composer's focus ring, a long
# straight line, is skipped). Rows sit wherever the conversation puts them,
# so scenarios find them instead of hard-coding places.
link_ys() {
    # shellcheck disable=SC2086
    set -- $ACCENT_RGB
    probe_pixels | awk -v R="$1" -v G="$2" -v B="$3" '
        ($3-R)^2 + ($4-G)^2 + ($5-B)^2 < 1600 { n[$2]++ }
        END { for (y in n) if (n[y] >= 10 && n[y] < 150) print y }' | sort -n |
        awk 'NR == 1 { s = $1; p = $1; next } $1 > p + 4 { print int((s + p) / 2); s = $1 } { p = $1 }
             END { if (NR) print int((s + p) / 2) }'
}

# click_link [n]: clicks the n-th link line in the panel (default: the last),
# on its right half (the review's name).
click_link() {
    local ys y
    probe
    ys=$(link_ys)
    if [ -n "${1:-}" ]; then y=$(echo "$ys" | sed -n "${1}p"); else y=$(echo "$ys" | tail -1); fi
    [ -n "$y" ] || { probe_failed "a review link in the panel"; return 1; }
    click $((PROBE_LEFT + PROBE_WIDTH - 140)) "$y"
}

file_re() { grep -qE -- "$2" "$WORK/$1" 2> /dev/null; }

# button_center "R G B": "x y" of the last button in the right-hand column
# (the panel, the accounts sheet) drawn with a border or fill of that colour
# (an error row's Retry: WARN_RGB; the sheet's Save: ACCENT_RGB), found as
# the last horizontal run of 30–200 pixels of the colour, in the last probe.
button_center() {
    # shellcheck disable=SC2086
    set -- $1
    probe_pixels | awk -v R="$1" -v G="$2" -v B="$3" -v L="$PROBE_LEFT" -v T="$PROBE_TOP" -v H="$PROBE_HEIGHT" '
        { on[$2, $1] = (($3-R)^2 + ($4-G)^2 + ($5-B)^2 < 900); if ($1 > maxx) maxx = $1 }
        END {
            for (y = T; y < T + H; y++) {
                run = 0; best = 0; bx = 0
                for (x = 0; x <= maxx; x++) {
                    if (on[y, x]) { run++; if (run > best) { best = run; bx = x - run + 1 } } else run = 0
                }
                if (best >= 30 && best <= 200) { if (!top || y > last + 40) { top = y; tx = bx; tw = best } last = y }
            }
            if (top) printf "%d %d\n", L + tx + tw / 2, (top + last) / 2
        }'
}

# has_button "R G B": a fresh probe shows such a button.
has_button() {
    probe
    [ -n "$(button_center "$1")" ]
}

# click_button "R G B" WHAT: clicks the last such button.
click_button() {
    local at
    probe
    at=$(button_center "$1")
    [ -n "$at" ] || { probe_failed "$2"; return 1; }
    # shellcheck disable=SC2086
    click $at
}

# click_retry: the last error row's button (Retry).
click_retry() { click_button "$WARN_RGB" "an error row's button"; }

# focus_composer: Escape (focus back to the script), then Ctrl+L (to the
# composer): lands in the composer from anywhere the panel is open.
focus_composer() {
    key -k Escape
    ctrl l
}

# click_composer: the composer's text field, at the foot of the panel, as it
# sits before the first reply.
click_composer() {
    local w
    w=$(SWAYSOCK=$SOCK swaymsg -t get_outputs | jq '.[0].current_mode.width')
    click $((w - COMPOSER_FROM_RIGHT)) "$COMPOSER_Y"
}
