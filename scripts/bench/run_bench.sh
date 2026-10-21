#!/usr/bin/env bash
# cargo-quicksilver vs cargo-mutants on one subject crate, exhaustive and per
# commit. Every run starts from its own cold scratch directory except the
# QS per-commit runs, which deliberately reuse one session (warm target/ and
# Bifrost store) because that is the per-commit workflow being measured.
#
# Usage: run_bench.sh QS_BIN SUBJECT_URL SUBJECT_SHA OUT_DIR [MODES]
#   MODES: space-separated subset of
#          "cm-fn cm-all qs-coverage qs-sites qs-static replay"
#   QS_BIN may be "-" when no QS mode runs (cargo-mutants legs only).
# Env:   CM_JOBS (default 8), QS_BUILD_JOBS (4), QS_TEST_JOBS (nproc),
#        REPLAY_COMMITS (10), QS_SELECTIONS ("sites") for the replay,
#        REPLAY_TOOLS ("cm qs"): which tools the replay runs.
set -euo pipefail

QS="${1:?QS_BIN}"; URL="${2:?SUBJECT_URL}"; SHA="${3:?SUBJECT_SHA}"; OUT="${4:?OUT_DIR}"
MODES="${5:-cm-fn cm-all qs-coverage qs-sites replay}"
CM_JOBS="${CM_JOBS:-8}"
QS_BUILD_JOBS="${QS_BUILD_JOBS:-4}"
QS_TEST_JOBS="${QS_TEST_JOBS:-$(nproc)}"
REPLAY_COMMITS="${REPLAY_COMMITS:-10}"
QS_SELECTIONS="${QS_SELECTIONS:-sites}"
REPLAY_TOOLS="${REPLAY_TOOLS:-cm qs}"
want() { [[ " $REPLAY_TOOLS " == *" $1 "* ]]; }
# cargo-mutants FnValue only: everything except operator, match-guard and
# match-arm mutants. Operator tokens contain no letters.
FN_ONLY=': (replace [^\sA-Za-z_]+ with [^\sA-Za-z_]+|delete [^\sA-Za-z_]+|replace match guard |delete match arm )'

mkdir -p "$OUT"
NAME="$(basename "$URL" .git)"
WORK="$OUT/work"; mkdir -p "$WORK"
SRC="$WORK/$NAME"
[[ -d "$SRC/.git" ]] || git clone -q "$URL" "$SRC"
git -C "$SRC" checkout -q --detach "$SHA"
unset RUSTUP_TOOLCHAIN RUSTC_WRAPPER

record() { # record LABEL CMD...  -> $OUT/LABEL.{log,time}
  local label="$1"; shift
  echo "== $label $(date -u +%FT%TZ) load: $(cut -d' ' -f1-3 /proc/loadavg)" | tee -a "$OUT/summary.txt"
  # QS exits 1 when a mutant is missed and 2 on a differential failure;
  # cargo-mutants exits 2 when mutants are missed. Record, do not stop.
  /usr/bin/time -v -o "$OUT/$label.time" "$@" > "$OUT/$label.log" 2>&1 || echo "   exit status $?" | tee -a "$OUT/summary.txt"
  grep -E 'Elapsed \(wall|Maximum resident' "$OUT/$label.time" | sed 's/^\s*/   /' | tee -a "$OUT/summary.txt"
}

fresh_copy() { # fresh_copy DIR -> clean checkout of $SHA at DIR
  rm -rf "$1"; git clone -q "$SRC" "$1"; git -C "$1" checkout -q --detach "$SHA"
}

for mode in $MODES; do
  case "$mode" in
    cm-fn)
      fresh_copy "$WORK/cm-fn"
      (cd "$WORK/cm-fn" && record cm-fn cargo mutants --test-tool nextest -j "$CM_JOBS" \
        --exclude-re "$FN_ONLY" -o "$OUT/cm-fn")
      ;;
    cm-all)
      fresh_copy "$WORK/cm-all"
      (cd "$WORK/cm-all" && record cm-all cargo mutants -j "$CM_JOBS" -o "$OUT/cm-all")
      ;;
    qs-coverage|qs-sites|qs-static)
      sel="${mode#qs-}"
      fresh_copy "$WORK/$mode"
      rm -rf "$WORK/$mode-scratch"; mkdir -p "$WORK/$mode-scratch"
      record "$mode" "$QS" --manifest-path "$WORK/$mode/Cargo.toml" \
        --scratch-root "$WORK/$mode-scratch" --clone-backend copy --allow-copy \
        --build-jobs "$QS_BUILD_JOBS" --test-jobs "$QS_TEST_JOBS" \
        --test-selection "$sel" --json "$OUT/$mode.json" run
      ;;
    replay)
      # Keep the most recent commits whose diff yields at least one
      # function-body mutant; formatting and manifest-only commits test nothing.
      if [[ ! -s "$OUT/replay-commits.txt" ]]; then
        fresh_copy "$WORK/replay-pick"
        picked=()
        while read -r c; do
          git -C "$WORK/replay-pick" checkout -q --detach "$c"
          git -C "$SRC" diff "$c^" "$c" > "$WORK/pick.diff"
          n="$(cd "$WORK/replay-pick" && cargo mutants --list --in-diff "$WORK/pick.diff" \
                 --exclude-re "$FN_ONLY" 2>/dev/null | grep -c . || true)"
          echo "$c $n" >> "$OUT/replay-pick.txt"
          [[ "$n" -gt 0 ]] && picked+=("$c")
          [[ "${#picked[@]}" -ge "$REPLAY_COMMITS" ]] && break
        done < <(git -C "$SRC" log --no-merges --format=%H -n 400 "$SHA" -- '*.rs')
        printf '%s\n' "${picked[@]}" | tac > "$OUT/replay-commits.txt"
      fi
      mapfile -t COMMITS < "$OUT/replay-commits.txt"
      want qs || QS_SELECTIONS=""
      for sel in $QS_SELECTIONS; do
        rm -rf "$WORK/replay-qs-$sel" "$WORK/replay-qs-$sel-scratch"
        git clone -q "$SRC" "$WORK/replay-qs-$sel"; mkdir -p "$WORK/replay-qs-$sel-scratch"
      done
      for c in "${COMMITS[@]}"; do
        short="${c:0:10}"
        git -C "$SRC" diff "$c^" "$c" > "$OUT/replay-$short.diff"
        if want cm; then
          fresh_copy "$WORK/replay-cm"; git -C "$WORK/replay-cm" checkout -q --detach "$c"
          (cd "$WORK/replay-cm" && record "replay-cm-$short" cargo mutants --test-tool nextest \
            -j "$CM_JOBS" --exclude-re "$FN_ONLY" --in-diff "$OUT/replay-$short.diff" -o "$OUT/replay-cm-$short")
        fi
        for sel in $QS_SELECTIONS; do
          git -C "$WORK/replay-qs-$sel" checkout -q --detach "$c"
          record "replay-qs-$sel-$short" "$QS" --manifest-path "$WORK/replay-qs-$sel/Cargo.toml" \
            --scratch-root "$WORK/replay-qs-$sel-scratch" --clone-backend copy --allow-copy \
            --build-jobs "$QS_BUILD_JOBS" --test-jobs "$QS_TEST_JOBS" --test-selection "$sel" \
            --in-diff "$c^...$c" --json "$OUT/replay-qs-$sel-$short.json" run
        done
      done
      ;;
    *) echo "unknown mode $mode" >&2; exit 2 ;;
  esac
done
echo "done $(date -u +%FT%TZ)" | tee -a "$OUT/summary.txt"
