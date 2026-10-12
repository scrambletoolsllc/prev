#!/usr/bin/env bash
# The assistant bench: asks a build of prev the questions of a scenario
# in its assistant panel, on a headless desktop of its own, so the user's
# screen and pointer stay free, and logs each request the model gets.
# Run it on two builds and compare the logs to see what a change does.
#
#   bench.sh run PREV FILE MODEL LOG [SCENARIO]
#   bench.sh compare LOG...
#
# PREV is the prev to run, such as target/debug/prev; FILE is opened in
# it (a copy, so the file itself is not marked up); MODEL is
# PROVIDER:NAME, such as open-ai:gpt-6-luna or ollama:qwen3.8;
# SCENARIO is report.scenario if left out. The model's key is the one
# the build has saved: a debug build reads the keys a debug build saved.
# BENCH_CONTEXT gives the model a context in tokens, as Settings would,
# such as a small one to see a long chat near its end.
#
# Needs sway, wtype, grim and uv, on Linux.
set -euo pipefail

HERE=$(cd "$(dirname "$0")" && pwd)
# What run starts, for the cleanup when the script ends.
work="" sway_pid="" prev_pid=""
cleanup() {
    if [ -n "$prev_pid" ]; then kill "$prev_pid" 2>/dev/null || true; fi
    if [ -n "$sway_pid" ]; then kill "$sway_pid" 2>/dev/null || true; fi
    if [ -n "$work" ]; then rm -rf "$work"; fi
}
trap cleanup EXIT

run() {
    local prev=$1 file=$2 model=$3 log=$4 scenario=${5:-$HERE/report.scenario}
    local provider=${model%%:*} name=${model#*:}
    prev=$(realpath "$prev")
    log=$(realpath -m "$log")
    work=$(mktemp -d "${TMPDIR:-/tmp}/prev-bench.XXXXXX")

    # The model to ask, and nothing else of the user's settings; a debug
    # build reads prev-dev.toml and a release one prev.toml.
    mkdir -p "$work/config" "$work/data"
    for settings in prev-dev.toml prev.toml; do
        cat > "$work/config/$settings" <<EOF
animations = false
# The toolbar floats over the page, as the scenarios' places expect.
auto-hide-toolbar = true
assistant-model = "bench"

[[assistant-models]]
id = "bench"
provider = "$provider"
model = "$name"
${BENCH_CONTEXT:+context = $BENCH_CONTEXT}
EOF
    done
    cp "$file" "$work/"
    local opened="$work/$(basename "$file")"

    # The pointer's protocol code, made once.
    export PROTOCOLS=${XDG_CACHE_HOME:-$HOME/.cache}/prev-assistant-bench
    if [ ! -f "$PROTOCOLS/protocols/__init__.py" ]; then
        local xml
        xml=$(find "${CARGO_HOME:-$HOME/.cargo}/registry/src" -name \
            wlr-virtual-pointer-unstable-v1.xml 2>/dev/null | sort | tail -1)
        [ -n "$xml" ] || { echo "No wlr-virtual-pointer XML; build prev first." >&2; exit 1; }
        mkdir -p "$PROTOCOLS"
        uv run -q --with pywayland python -m pywayland.scanner \
            -i /usr/share/wayland/wayland.xml "$xml" -o "$PROTOCOLS/protocols" 2>/dev/null
    fi

    # A headless sway, on the first display name that is free.
    local before
    before=$(ls "$XDG_RUNTIME_DIR" | grep -E '^wayland-[0-9]+$' || true)
    WLR_BACKENDS=headless WLR_LIBINPUT_NO_DEVICES=1 WLR_RENDERER=pixman \
        sway -c "$HERE/sway.conf" > "$work/sway.log" 2>&1 &
    sway_pid=$!
    export WAYLAND_DISPLAY=""
    for _ in $(seq 50); do
        sleep 0.2
        WAYLAND_DISPLAY=$(comm -13 <(echo "$before") \
            <(ls "$XDG_RUNTIME_DIR" | grep -E '^wayland-[0-9]+$') | head -1)
        [ -n "$WAYLAND_DISPLAY" ] && break
    done
    [ -n "$WAYLAND_DISPLAY" ] || { echo "sway did not start; see $work/sway.log" >&2; exit 1; }
    export BENCH_SIZE=1600x1000

    XDG_CONFIG_HOME=$work/config XDG_DATA_HOME=$work/data PREV_ASSIST_LOG=$log \
        "$prev" "$opened" > "$work/prev.log" 2>&1 &
    prev_pid=$!
    sleep 6

    local field="" newchat=""
    pointer() { uv run -q --with pywayland python "$HERE/pointer.py" "$@"; }
    while read -r step rest; do
        case $step in
            ''|'#'*) ;;
            field) field=$rest ;;
            newchat) newchat=$rest ;;
            click) pointer click $rest ;;
            drag) pointer drag $rest ;;
            sleep) sleep "$rest" ;;
            new)
                # shellcheck disable=SC2086
                pointer click $newchat
                sleep 1
                ;;
            ask)
                ask "$rest" "$field" "$log"
                ;;
            *) echo "No step called $step" >&2; exit 1 ;;
        esac
    done < "$scenario"
    grim "${log%.*}.png" 2>/dev/null || true
}

# Types QUESTION in the field at FIELD, sends it, and waits for the model's
# last request of the answer, the one that calls no tool and is not for a
# summary of the chat.
ask() {
    local question=$1 field=$2 log=$3
    jq -cn --arg question "$question" '{question: $question}' >> "$log"
    local start
    start=$(wc -l < "$log")
    # shellcheck disable=SC2086
    uv run -q --with pywayland python "$HERE/pointer.py" click $field
    # wtype's first key can come before the keyboard is ready, so a Shift
    # goes first; Ctrl+A replaces whatever the field holds.
    wtype -k Shift_L -s 100 -M ctrl a -m ctrl "$question" -s 200 -k Return
    local answered=""
    for _ in $(seq 90); do
        sleep 2
        if tail -n +$((start + 1)) "$log" \
            | jq -e -s 'any(.[]; .calls == [] and (.summary | not))' > /dev/null; then
            answered=yes
            break
        fi
    done
    echo "== $question"
    if [ -z "$answered" ]; then
        # Most often a place in the scenario that misses on this screen.
        grim "${log%.*}-stuck.png" 2>/dev/null || true
        echo "No answer in 3 minutes; the screen is in ${log%.*}-stuck.png" >&2
        exit 1
    fi
    tail -n +$((start + 1)) "$log" | jq -r \
        '"  \(.input_tokens) in (\(.cached_tokens // 0) cached), \(.output_tokens) out, \(.seconds) s, \(if .summary then "a summary of the chat" else "calls \(.calls | join(", "))" end)"'
}

# Each log's totals for each question and in all.
compare() {
    for log in "$@"; do
        echo "== $log"
        jq -rs '
            reduce .[] as $line ({rows: []};
                if $line.question then .rows += [{question: $line.question,
                    requests: 0, input: 0, cached: 0, output: 0, seconds: 0, calls: []}]
                else .rows[-1] |= (.requests += 1
                    | .input += ($line.input_tokens // 0)
                    | .cached += ($line.cached_tokens // 0)
                    | .output += ($line.output_tokens // 0)
                    | .seconds += $line.seconds
                    | .calls += $line.calls) end)
            | .rows as $rows
            | ($rows[] | "  \(.question): \(.input) in (\(.cached) cached), \(.output) out, \(.seconds * 10 | round / 10) s, \(.calls | length) calls"),
              "  all: \([$rows[].input] | add) in (\([$rows[].cached] | add) cached), \([$rows[].output] | add) out, \([$rows[].seconds] | add * 10 | round / 10) s, \([$rows[].calls | length] | add) calls"
        ' "$log"
    done
}

case ${1:-} in
    run) shift; run "$@" ;;
    compare) shift; compare "$@" ;;
    *) sed -n '2,19s/^# \{0,1\}//p' "$0"; exit 1 ;;
esac
