# Helpers for the chat-panel Sway scenarios (sourced by run.sh).
#
# Everything that touches a display goes to the private headless Sway this
# file starts ($W), never the user's: every display command below sets
# WAYLAND_DISPLAY=$W on its own command line, and Sway is stopped through its
# own IPC socket.

ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/../../../.." && pwd)
CASSETTES=$ROOT/crates/app/tests/cassettes
TARGET=${CARGO_TARGET_DIR:-$ROOT/target}
OUT=${SCENARIO_OUT:-${TMPDIR:-/tmp}/scriptorium-scenarios}
SHOTS=$OUT/shots
# Private copies of the binaries, so a concurrent cargo build sharing the
# target folder can't swap them mid-run (kept beside the target, not in a tmpfs).
BIN=$TARGET/scenarios-bin
APP=$BIN/scriptorium-app-scen
CLI=$BIN/scriptorium-scen
VPOINTER=$BIN/vpointer-scen
CLOCK=2026-11-15T09:30:00Z
export XDG_RUNTIME_DIR=${XDG_RUNTIME_DIR:-/run/user/$(id -u)}

SCEN=""      # the scenario running
WORK=""      # its scratch folder
FAILS=()     # its failed checks
NOTES=()     # its notes (known gaps, skipped parts)

log() { printf '  %s\n' "$*"; }

# ---- the compositor ------------------------------------------------------------

sway_start() {
    local conf=$OUT/sway.conf before pid
    printf 'output HEADLESS-1 resolution %s\ndefault_border none\nfocus_follows_mouse no\n' "${1:-1500x1000}" > "$conf"
    before=$(ls "$XDG_RUNTIME_DIR" | grep -E '^wayland-[0-9]+$' | sort)
    env -u WAYLAND_DISPLAY -u SWAYSOCK -u DISPLAY WLR_BACKENDS=headless WLR_LIBINPUT_NO_DEVICES=1 \
        sway -c "$conf" > "$OUT/sway.log" 2>&1 &
    SWAY_PID=$!
    for _ in $(seq 50); do
        W=$(comm -13 <(echo "$before") <(ls "$XDG_RUNTIME_DIR" | grep -E '^wayland-[0-9]+$' | sort) | head -1)
        SOCK=$XDG_RUNTIME_DIR/sway-ipc.$(id -u).$SWAY_PID.sock
        [ -n "$W" ] && [ -S "$SOCK" ] && break
        sleep 0.1
    done
    if [ -z "$W" ] || [ ! -S "$SOCK" ]; then
        echo "could not start the headless Sway (see $OUT/sway.log)" >&2
        exit 1
    fi
    echo "headless Sway: WAYLAND_DISPLAY=$W"
}

sway_stop() {
    [ -n "${SOCK:-}" ] && [ -S "$SOCK" ] && SWAYSOCK=$SOCK swaymsg exit > /dev/null 2>&1
    [ -n "${SWAY_PID:-}" ] && wait "$SWAY_PID" 2> /dev/null
}

resolution() { SWAYSOCK=$SOCK swaymsg output HEADLESS-1 resolution "$1" > /dev/null; sleep 0.8; }

# ---- input and screenshots -----------------------------------------------------

# Each wtype run drops its first key, so every run leads with a Shift press.
# key: wtype arguments (`key -M ctrl -k l -m ctrl`); type: literal text.
key() { WAYLAND_DISPLAY=$W wtype -k Shift_L -s 120 "$@"; sleep "${KEY_PAUSE:-0.5}"; }
type_text() { WAYLAND_DISPLAY=$W wtype -k Shift_L -d 15 "$1"; sleep 0.4; }
ctrl() { key -M ctrl -k "$1" -m ctrl; }
click() { WAYLAND_DISPLAY=$W "$VPOINTER" click "$@"; sleep 0.5; }
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
        printf '{"version": 1, "disclosures": ["api.anthropic.com\\u001f%s"]}\n' "$WORK/film.fdx" \
            > "$WORK/config/scriptorium/seen.json"
    fi
}

# The environment both programs run with: offline, deterministic, nothing of
# the user's (config, state, keyring: the CLI reads SCRIPTORIUM_KEYRING, the
# app SCRIPTORIUM_TEST_KEYS).
run_env() {
    env -u DISPLAY -u SCRIPTORIUM_REPLAY -u SCRIPTORIUM_WORK_KEY \
        HOME="$WORK" XDG_CONFIG_HOME="$WORK/config" XDG_STATE_HOME="$WORK/state" \
        SCRIPTORIUM_KEYRING=off SCRIPTORIUM_TEST_KEYS=env SCRIPTORIUM_ANTHROPIC_KEY=test \
        SCRIPTORIUM_TEST_CLOCK=$CLOCK SCRIPTORIUM_TEST_TIME_SCALE=${TIME_SCALE:-0.01} "$@"
}

# app_start [cassette] [VAR=value...]: the app on $WORK/film.fdx (or $APP_ARGS).
app_start() {
    local cassette=${1:-}
    shift || true
    rm -f "$WORK/app.exit"
    (
        cd "$WORK" || exit
        run_env WAYLAND_DISPLAY="$W" ${cassette:+SCRIPTORIUM_REPLAY="$cassette"} "$@" \
            dbus-run-session -- "$APP" ${APP_ARGS:-film.fdx} > "$WORK/app.log" 2>&1
        echo $? > "$WORK/app.exit"
    ) &
    wait_for 20 app_window || { FAILS+=("the app's window never appeared"); return 1; }
    sleep 1.5
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
        pkill -f "^$APP" 2> /dev/null
        wait_for 5 test -f "$WORK/app.exit"
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

# wait_for SECONDS CMD...: until CMD succeeds (polled every 0.2 s).
wait_for() {
    local n=$(($1 * 5))
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

# link_ys: the y of each line of link-coloured text in the panel (the
# accent purple of a tool row's review link), top to bottom, read off a
# fresh shot (the composer's focus ring, a long straight line, is skipped). Rows sit wherever the conversation puts them, so scenarios
# find them instead of hard-coding places.
link_ys() {
    local f=$OUT/.probe.png w
    WAYLAND_DISPLAY=$W grim "$f"
    w=$(magick identify -format %w "$f")
    magick "$f" -crop "340x800+$((w - 350))+76" -depth 8 txt:- | awk '
        NR > 1 { split($1, xy, "[,:]"); s = $2; gsub(/[()]/, "", s); split(s, c, ",")
                 if ((c[1]-93)^2 + (c[2]-66)^2 + (c[3]-164)^2 < 1600) n[xy[2] + 76]++ }
        END { for (y in n) if (n[y] >= 10 && n[y] < 150) print y }' | sort -n |
        awk 'NR == 1 { s = $1; p = $1; next } $1 > p + 4 { print int((s + p) / 2); s = $1 } { p = $1 }
             END { if (NR) print int((s + p) / 2) }'
}

# click_link [n]: clicks the n-th link line in the panel (default: the last).
click_link() {
    local ys y w
    ys=$(link_ys)
    if [ -n "${1:-}" ]; then y=$(echo "$ys" | sed -n "${1}p"); else y=$(echo "$ys" | tail -1); fi
    [ -n "$y" ] || { FAILS+=("no review link in the panel to click"); return 1; }
    w=$(magick identify -format %w "$OUT/.probe.png")
    click $((w - 150)) "$y"
}
file_re() { grep -qE -- "$2" "$WORK/$1" 2> /dev/null; }

# button_center R G B: "x y" of the last button in the right-hand column (the
# panel, the accounts sheet) drawn with a border or fill of that colour (an
# error row's Retry: WARN; the sheet's Save: ACCENT), found as the last
# horizontal run of 30–200 pixels of the colour, read off a fresh shot.
button_center() {
    local f=$OUT/.probe.png w left
    WAYLAND_DISPLAY=$W grim "$f"
    w=$(magick identify -format %w "$f")
    left=$((w - 350))
    magick "$f" -crop "340x800+$left+76" -depth 8 txt:- | awk -v R="$1" -v G="$2" -v B="$3" -v L="$left" '
        NR > 1 { split($1, xy, "[,:]"); s = $2; gsub(/[()]/, "", s); split(s, c, ",")
                 on[xy[2] + 76, xy[1] + 0] = ((c[1]-R)^2 + (c[2]-G)^2 + (c[3]-B)^2 < 900) }
        END {
            for (y = 76; y < 876; y++) {
                run = 0; best = 0; bx = 0
                for (x = 0; x < 340; x++) {
                    if (on[y, x]) { run++; if (run > best) { best = run; bx = x - run + 1 } } else run = 0
                }
                if (best >= 30 && best <= 200) { if (!top || y > last + 40) { top = y; tx = bx; tw = best } last = y }
            }
            if (top) printf "%d %d\n", L + tx + tw / 2, (top + last) / 2
        }'
}

# click_retry: the last error row's button (Retry).
click_retry() {
    local at
    at=$(button_center 162 54 31)
    [ -n "$at" ] || { FAILS+=("no error-row button to click"); return 1; }
    # shellcheck disable=SC2086
    click $at
}
