# S16 "apps" (docs/plans/apps.md, A11): the app hosts the example apps on a
# test port, an agent opens one through `scriptorium mcp`, curl walks the
# server's checks and API, and (when firefox and geckodriver are installed) a
# headless Firefox loads the pages. Sourced by run.sh after lib.sh; uses its
# helpers (fresh, app_start, cli, check, …).
#
# Nothing reaches the user's display or browser: the app runs in the private
# Sway; SCRIPTORIUM_TEST_OPEN_URL writes the URLs it would open to a file;
# SCRIPTORIUM_TEST_APPS_PORT keeps it off 28417 (a real copy may serve
# there); Firefox runs -headless with DISPLAY and WAYLAND_DISPLAY unset, a
# fresh profile and HOME in the scenario's folder. The one page that names
# an internet address (csp-probe) is blocked by the CSP before any load.

APPS_PORT=39417
GECKO_PORT=39418
BASE=http://127.0.0.1:$APPS_PORT

# hcurl ARGS...: curl at the apps server, never through a proxy; prints the
# status code only (the body to $WORK/body, the head to $WORK/head).
hcurl() {
    curl -s --noproxy '*' --max-time 10 -o "$WORK/body" -D "$WORK/head" -w '%{http_code}' "$@"
}
# jcurl ARGS...: as hcurl, with the cookie and this server's Origin.
jcurl() { hcurl -b "$WORK/jar" -H "Origin: $BASE" "$@"; }
head_has() { grep -qi -- "$1" "$WORK/head"; }
body_has() { grep -qF -- "$1" "$WORK/body"; }
is() { [ "$1" = "$2" ]; }

# mcp OUT CALL...: one `scriptorium mcp` session: initialize, then each CALL
# ("tool_name {json args}") as tools/call with ids 2, 3, …; the replies, one
# JSON per line, in $WORK/OUT. mcp_text OUT ID: a reply's text.
mcp() {
    local out=$WORK/$1 id=2 c
    shift
    {
        echo '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"s16","version":"0"}}}'
        echo '{"jsonrpc":"2.0","method":"notifications/initialized"}'
        for c in "$@"; do
            jq -cn --argjson id "$id" --arg name "${c%% *}" --argjson args "${c#* }" \
                '{jsonrpc: "2.0", id: $id, method: "tools/call", params: {name: $name, arguments: $args}}'
            id=$((id + 1))
        done
    } | (cd / && run_env "$CLI" mcp) > "$out" 2> "$out.err"
}
mcp_text() { jq -r --argjson id "$2" 'select(.id == $id) | .result.content[0].text' "$WORK/$1"; }

# ---- the browser (WebDriver over curl) ------------------------------------------

wd() { # wd METHOD PATH [JSON]: a WebDriver command; the answer's .value on stdout
    curl -s --noproxy '*' --max-time 30 -X "$1" -H 'Content-Type: application/json' \
        ${3:+--data "$3"} "http://127.0.0.1:$GECKO_PORT$2" | jq -c '.value'
}
wd_go() { wd POST "/session/$WDS/url" "$(jq -cn --arg u "$1" '{url: $u}')" > /dev/null; }
# wd_js SCRIPT: runs SCRIPT in the page; its return value as text.
wd_js() { wd POST "/session/$WDS/execute/sync" "$(jq -cn --arg s "$1" '{script: $s, args: []}')" | jq -r '.'; }
page_text() { wd_js 'return document.body ? document.body.innerText : ""'; }
# page_says TEXT: the page shows TEXT (what it shows is kept in $WORK/page.txt).
page_says() { page_text > "$WORK/page.txt"; grep -qF -- "$1" "$WORK/page.txt"; }

at_url() { [ "$(wd GET "/session/$WDS/url" | jq -r .)" = "$1" ]; }
# mark_page PATH: the page at PATH gets a marker (window.s16marker) that is
# still there a second later.
mark_page() {
    [ "$(wd_js 'window.s16marker = 1; return location.pathname')" = "$1" ] || return 1
    sleep 1
    [ "$(wd_js 'return window.s16marker === 1 && document.readyState === "complete"')" = true ]
}
# page_back: a new page (no marker) showing the first scene and no overlay.
page_back() {
    [ "$(wd_js 'return window.s16marker === undefined && document.readyState === "complete"')" = true ] &&
        page_says "EXT. FARM HOUSE - NIGHT" && ! grep -qF "Scriptorium" "$WORK/page.txt"
}
# browser_shot NAME: the page as Firefox draws it, into the shots.
browser_shot() { sleep 0.5; wd GET "/session/$WDS/screenshot" | jq -r . | base64 -d > "$SHOTS/$SCEN-$1.png" 2> /dev/null; }
# app_listed NAME REGEX: a fresh list_apps (through the app) shows REGEX
# among app NAME's lines.
app_listed() {
    mcp list_apps.jsonl 'list_apps {}'
    mcp_text list_apps.jsonl 2 | awk -v n="- $1:" 'index($0, n) == 1 { on = 1; print; next } /^- / { on = 0 } on' |
        grep -q -- "$2"
}

browser_start() {
    mkdir -p "$WORK/ff-home" "$WORK/ff-profiles"
    env -u DISPLAY -u WAYLAND_DISPLAY -u SWAYSOCK HOME="$WORK/ff-home" MOZ_HEADLESS=1 \
        geckodriver --port "$GECKO_PORT" --profile-root "$WORK/ff-profiles" > "$WORK/geckodriver.log" 2>&1 &
    GECKO_PID=$!
    wait_for 10 curl -s --noproxy '*' "http://127.0.0.1:$GECKO_PORT/status" -o /dev/null || return 1
    WDS=$(curl -s --noproxy '*' --max-time 60 -X POST -H 'Content-Type: application/json' \
        --data '{"capabilities":{"alwaysMatch":{"browserName":"firefox","moz:firefoxOptions":{"args":["-headless"]}}}}' \
        "http://127.0.0.1:$GECKO_PORT/session" | jq -r '.value.sessionId // empty')
    [ -n "$WDS" ]
}

browser_stop() {
    [ -n "${WDS:-}" ] && wd DELETE "/session/$WDS" > /dev/null
    [ -n "${GECKO_PID:-}" ] && kill "$GECKO_PID" 2> /dev/null && wait "$GECKO_PID" 2> /dev/null
    WDS="" GECKO_PID=""
}

# ---- S16 apps ----------------------------------------------------------------------

S16() {
    fresh
    mkdir -p "$WORK/apps"
    cp -r "$ROOT/crates/apps/examples/scene-list" "$ROOT/crates/apps/examples/characters-by-scene" "$WORK/apps/"
    local opened=$WORK/opened
    rm -f "$opened"

    # 1. The CLI lists the copies; `tool list_apps` names the same folder.
    cli apps film.fdx
    check "scriptorium apps exits 0" is "$CLI_CODE" 0
    check "it lists both examples" sh -c 'echo "$1" | grep -q "^- scene-list: Scene list" && echo "$1" | grep -q "^- characters-by-scene:"' _ "$CLI_OUT"
    local folder
    folder=$(echo "$CLI_OUT" | sed -n '1s/^[0-9]* apps in \(.*\):$/\1/p')
    check "it names the absolute apps folder ($folder)" is "$folder" "$WORK/apps/"
    cli tool film.fdx list_apps
    check "scriptorium tool list_apps names the same folder" is "$(echo "$CLI_OUT" | sed -n '1s/^[0-9]* apps in \(.*\):$/\1/p')" "$folder"

    # 2. The app, serving apps on the test port; an agent opens scene-list.
    app_start "" SCRIPTORIUM_TEST_APPS_PORT=$APPS_PORT SCRIPTORIUM_TEST_OPEN_URL="$opened" || return
    check "the apps server answers" wait_for 10 sh -c "curl -s --noproxy '*' -o /dev/null -w '%{http_code}' -H 'Host: 127.0.0.1:$APPS_PORT' $BASE/api/status | grep -q 401"
    mcp mcp1.jsonl 'list_apps {}' 'open_app {"name": "scene-list"}'
    check "list_apps (through the app) names the folder" sh -c 'echo "$1" | grep -qF "apps in $2"' _ "$(mcp_text mcp1.jsonl 2)" "$folder"
    check "open_app answers it opened" is "$(mcp_text mcp1.jsonl 3)" "Opened Scene list in the writer's browser."
    check "one URL went to the browser (the test hook)" is "$(wc -l < "$opened" 2> /dev/null)" 1
    local url key token
    url=$(head -1 "$opened" 2> /dev/null)
    key=$(echo "$url" | sed -n 's|.*&to=/p/\([0-9a-f]*\)/scene-list/$|\1|p')
    token=$(echo "$url" | sed -n 's|.*/open?t=\([0-9a-f]*\)&.*|\1|p')
    check "it is the opening link ($url)" sh -c "[ -n '$key' ] && [ -n '$token' ] && [ '${url%%/open*}' = '$BASE' ]"
    check "open_app's answer doesn't hold the token" sh -c "! grep -q '$token' '$WORK/mcp1.jsonl'"
    local page=$BASE/p/$key/scene-list/ api=$BASE/api/p/$key/a/scene-list

    # 3. Without the cookie: 401.
    check "an app page without the cookie: 401" is "$(hcurl "$page")" 401
    check "/ without the cookie: 401" is "$(hcurl "$BASE/")" 401
    check "the 401 page says to use the Apps menu" body_has "Apps menu"
    check "a wrong token at /open: 401" is "$(hcurl "$BASE/open?t=0000&to=/p/$key/scene-list/")" 401

    # 4. The opening link: the cookie, then the page.
    check "the opening link: 200" is "$(hcurl -c "$WORK/jar" "$url")" 200
    check "Set-Cookie is HttpOnly and SameSite=Lax" sh -c "grep -i '^set-cookie: scriptorium=' '$WORK/head' | grep -i 'HttpOnly' | grep -qi 'SameSite=Lax'"
    check "the page moves on to the app (refresh, no token)" sh -c "grep -qF 'http-equiv=\"refresh\"' '$WORK/body' && grep -qF '/p/$key/scene-list/' '$WORK/body'"
    check "the app page with the cookie: 200" is "$(hcurl -b "$WORK/jar" "$page")" 200
    check "…with the CSP" head_has "^content-security-policy: default-src 'self'"
    check "…and the app's HTML" body_has "<title>Scene list</title>"
    check "/ with the cookie: 200" is "$(hcurl -b "$WORK/jar" "$BASE/")" 200

    # 5. Refusals.
    check "a foreign Host: 421" is "$(hcurl -b "$WORK/jar" -H "Host: evil.example:$APPS_PORT" "$page")" 421
    check "POST with a foreign Origin: 403" is "$(hcurl -b "$WORK/jar" -H 'Origin: http://evil.example' \
        -H 'Content-Type: application/json' --data '{"name":"script_info","arguments":{}}' "$api/call")" 403
    check "POST without Origin: 403" is "$(hcurl -b "$WORK/jar" -H 'Content-Type: application/json' \
        --data '{"name":"script_info","arguments":{}}' "$api/call")" 403
    check "%2e%2e in an app's path: 404" is "$(hcurl -b "$WORK/jar" --path-as-is "$page%2e%2e/app.json")" 404
    check "another folder's key: 409 not-open" is "$(jcurl -H 'Content-Type: application/json' \
        --data '{"name":"script_info","arguments":{}}' "$BASE/api/p/0123456789abcdef/a/scene-list/call")" 409

    # 6. A call.
    check "read_scene through the API: 200" is "$(jcurl -H 'Content-Type: application/json' \
        --data '{"name":"read_scene","arguments":{"scene":1}}' "$api/call")" 200
    check "…the scene's text, not an error" jq_is body '.is_error == false and (.text | contains("EXT. FARM HOUSE - NIGHT")) and (.text | contains("Laura smiles."))'

    # 7. The event stream (in the background, capped): hello, and what follows.
    curl -s -N --noproxy '*' --max-time 12 -b "$WORK/jar" "$BASE/api/events" > "$WORK/events.log" 2>&1 &
    local events=$!
    check "the event stream says hello" wait_for 5 grep -q '^event: hello' "$WORK/events.log"
    # The events tick's first look (every 250 ms) is its baseline: a change
    # before it would go unseen.
    sleep 0.6
    check "…naming the project and script" grep -qF "\"project\":\"$key\"" "$WORK/events.log"

    # 8. A suggestion from the app: its default review, on the card.
    local para
    jcurl -H 'Content-Type: application/json' --data '{"name":"list_paragraphs","arguments":{"contains":"I am go"}}' "$api/call" > /dev/null
    para=$(jq -r '.text | fromjson | .paragraphs[0].id' "$WORK/body")
    jcurl -H 'Content-Type: application/json' --data "{\"name\":\"suggest_changes\",\"arguments\":{\"suggestions\":[{\"paragraph_id\":$para,\"options\":[\"I am going to town tomorrow.\"]}]}}" "$api/call" > /dev/null
    check "suggest_changes: added 1" jq_is body '.is_error == false and (.text | fromjson | .added == 1)'
    check "into scene-list (app).review.txt" file_has "scene-list (app).review.txt" "I am going to town tomorrow."
    check "the session attaches it" wait_for 5 file_has film.scriptorium.json "scene-list (app).review.txt"
    check "the event log has script" wait_for 5 grep -q '^event: script' "$WORK/events.log"
    ctrl 1 # the first review, on its card
    sleep 0.5
    shot 01-suggested
    visual "01: the app's suggestion on the card (review 1, scene-list (app).review.txt, in the status line); the status bar names the scene-list app"

    # 9. The app's files change: reload, after about a second of quiet.
    touch "$WORK/apps/scene-list/index.html"
    check "touching index.html sends reload for scene-list" wait_for 5 grep -qF '"app":"scene-list"' "$WORK/events.log"
    kill "$events" 2> /dev/null
    wait "$events" 2> /dev/null

    # 10. The app's data.
    local code
    code=$(jcurl -X PUT -H 'Content-Type: application/json' --data '{"n":1}' "$api/data/notes.json")
    check "PUT data/notes.json: 2xx ($code)" is "${code%??}" 2
    check "it is on disk under data/" file_has apps/scene-list/data/notes.json '{"n":1}'
    check "GET it back: 200" is "$(jcurl "$api/data/notes.json")" 200
    check "…the same JSON" body_has '{"n":1}'
    check "data/%2e%2e/x: 404" is "$(jcurl --path-as-is "$api/data/%2e%2e/x")" 404
    check "data isn't served as a page: 404" is "$(jcurl "${page}data/notes.json")" 404

    # 11. A real browser.
    S16_browser "$url" "$key"

    # 12. Started again: the old cookie still works, with no /open.
    app_running && app_quit 0
    app_start "" SCRIPTORIUM_TEST_APPS_PORT=$APPS_PORT SCRIPTORIUM_TEST_OPEN_URL="$opened" || return
    check "after a restart the old cookie works: 200" wait_for 10 sh -c "[ \$(curl -s --noproxy '*' -o /dev/null -w '%{http_code}' -b '$WORK/jar' '$page') = 200 ]"
    if [ -n "${WDS:-}" ]; then
        # It may load twice (the first time before the script is open): what
        # counts is where it settles.
        check "the browser page reloads by itself: the scenes back, no overlay" wait_for 20 page_back
        browser_shot 05-reloaded
        browser_stop
    fi
    app_quit 0
}

# S16_browser URL KEY: step 11, in headless Firefox; SKIP without it. Leaves
# the browser running on the scene-list page with the app quit.
S16_browser() {
    local url=$1 key=$2
    if ! command -v firefox > /dev/null || ! command -v geckodriver > /dev/null; then
        log "SKIP the browser step: firefox or geckodriver isn't installed (hand test 3 covers it)"
        app_quit 0
        return
    fi
    if ! browser_start; then
        FAILS+=("couldn't start headless Firefox (see $WORK/geckodriver.log)")
        browser_stop
        app_quit 0
        return
    fi
    wd_go "$url"
    check "Firefox: the opening link lands on the app, token gone" wait_for 5 at_url "$BASE/p/$key/scene-list/"
    check "Firefox: the page shows the first scene within 5 s" wait_for 5 page_says "EXT. FARM HOUSE - NIGHT"
    browser_shot 02-scene-list
    check "list_apps: scene-list's page loaded" wait_for 5 app_listed scene-list 'page: loaded'
    local scene_window
    scene_window=$(wd GET "/session/$WDS/window" | jq -r .)

    # An app that loads a script from the internet: blocked, and reported.
    mkdir -p "$WORK/apps/csp-probe"
    printf '{ "scriptorium": 1, "title": "CSP probe", "description": "Loads a script from the internet." }\n' \
        > "$WORK/apps/csp-probe/app.json"
    printf '<!doctype html>\n<html><head><script src="/scriptorium.js"></script><title>CSP probe</title></head>\n<body><p>probe</p><script src="https://example.com/x.js"></script></body></html>\n' \
        > "$WORK/apps/csp-probe/index.html"
    mcp mcp3.jsonl 'open_app {"name": "csp-probe"}'
    local probe_url
    probe_url=$(tail -1 "$WORK/opened")
    check "open_app csp-probe opens a second URL" sh -c "echo '$probe_url' | grep -q '/p/$key/csp-probe/\$'"
    local probe_window
    probe_window=$(wd POST "/session/$WDS/window/new" '{"type":"tab"}' | jq -r .handle)
    wd POST "/session/$WDS/window" "{\"handle\":\"$probe_window\"}" > /dev/null
    wd_go "$probe_url"
    check "Firefox: the probe page shows the blocked load" wait_for 5 page_says "tried to load https://example.com/x.js from the internet"
    check "list_apps reports csp-probe's error naming the URL" wait_for 5 app_listed csp-probe 'last error: .*https://example.com/x.js'
    browser_shot 03-csp-probe
    wd DELETE "/session/$WDS/window" > /dev/null
    wd POST "/session/$WDS/window" "{\"handle\":\"$scene_window\"}" > /dev/null

    # Scriptorium quits: the scene-list page says so, without reloading.
    # (Back on its tab, Firefox sometimes loads the page again: the marker
    # goes on once the page has been still for a second.)
    check "Firefox: back on the scene list, marked once it is still" wait_for 10 mark_page "/p/$key/scene-list/"
    app_quit 0
    check "Firefox: after the quit the page says \"Scriptorium isn't running\"" \
        wait_for 10 page_says "Scriptorium isn't running"
    browser_shot 04-not-running
    check "…on the same page (not reloaded)" is "$(wd_js 'return window.s16marker === 1')" true
    visual "Firefox shots: 02 the scene list; 03 the bar naming https://example.com/x.js; 04 the \"isn't running\" overlay; 05 reloaded after the restart"
}
