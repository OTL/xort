#!/usr/bin/env bash
# Reproducible, multi-axis benchmark: xort vs GNU sort (and mlr/jq/csvsort for
# structured data), driven by hyperfine.
#
# Axes:
#   A. Size scaling      — 1M / 10M / 100M, numeric + text
#   B. Distribution      — uniform / sorted / reverse / nearly-sorted / low-card
#   C. External (>RAM)   — tiny -S forces spill; verified via --stats chunk count
#   D. Field keys        — -k/-t multi-key sorts vs GNU
#   E. Structured        — CSV/JSON vs mlr / csvsort / jq (the differentiators)
#   F. Thread scaling    — --parallel 1..nproc
#
# Metrics: wall time (hyperfine mean±sigma, relative), throughput (MB/s,
# Mlines/s), and peak RSS (when GNU `/usr/bin/time -v` is available).
#
# Fairness notes:
#   * Both tools run under LC_ALL=C (byte ordering) — the *conservative* compare;
#     GNU sort is far slower in a UTF-8 locale and we deliberately don't exploit it.
#   * A warmup run primes the page cache so we measure CPU/algorithm, not cold disk.
#   * Output is verified byte-identical to the reference before any timing is
#     trusted (except structured tools, which reserialize — see that section).
#
# Env knobs:
#   QUICK=1     small sizes, fewer runs, skip 100M  (CI smoke / fast iteration)
#   SIZES=...   space-separated row counts          (default 1M 10M 100M)
#   RUNS=N WARMUP=N   hyperfine runs/warmup
#   XORT=path   binary under test (default ./target/release/xort)
#   DATA=dir    dataset directory (default a fresh mktemp -d)
#   OUT=path CSVOUT=path   report outputs
set -euo pipefail
export LC_ALL=C

XORT="${XORT:-./target/release/xort}"
OUT="${OUT:-benchmarks/results.md}"
CSVOUT="${CSVOUT:-benchmarks/results.csv}"
DATA="${DATA:-$(mktemp -d)}"
QUICK="${QUICK:-0}"
NPROC="$(nproc)"

if [ "$QUICK" = 1 ]; then
  : "${SIZES:=100000 1000000}"
  : "${RUNS:=3}"; : "${WARMUP:=1}"
else
  : "${SIZES:=1000000 10000000 100000000}"
  : "${RUNS:=5}"; : "${WARMUP:=1}"
fi

mkdir -p "$(dirname "$OUT")"
: > "$CSVOUT"
echo "section,command,mean_s,bytes,lines" >> "$CSVOUT"

has() { command -v "$1" >/dev/null 2>&1; }

# Peak-RSS measurement needs GNU `/usr/bin/time -v` (not the bash builtin).
TIME_V=0
if has /usr/bin/time && /usr/bin/time -v true 2>&1 | grep -q "Maximum resident"; then
  TIME_V=1
fi
HAVE_MLR=0;     has mlr     && HAVE_MLR=1
HAVE_JQ=0;      has jq      && HAVE_JQ=1
HAVE_CSVSORT=0; has csvsort && HAVE_CSVSORT=1

note() { echo "$*" >> "$OUT"; echo >> "$OUT"; }

# --------------------------------------------------------------------------
# Dataset generation (deterministic; awk-portable)
# --------------------------------------------------------------------------
gen_uniform_ints() { awk -v n="$1" 'BEGIN{srand(1);for(i=0;i<n;i++)print int(rand()*100000000)}' > "$2"; }
gen_sorted_ints()  { awk -v n="$1" 'BEGIN{for(i=1;i<=n;i++)print i}'                              > "$2"; }
gen_reverse_ints() { awk -v n="$1" 'BEGIN{for(i=n;i>=1;i--)print i}'                              > "$2"; }
# nearly-sorted: ascending with small local jitter
gen_nearly_ints()  { awk -v n="$1" 'BEGIN{srand(4);for(i=1;i<=n;i++)print i+int(rand()*200-100)}' > "$2"; }
# low-cardinality: only ~100 distinct values (dup-heavy, exercises -u)
gen_lowcard_ints() { awk -v n="$1" 'BEGIN{srand(5);for(i=0;i<n;i++)print int(rand()*100)}'        > "$2"; }
gen_words()        { awk -v n="$1" 'BEGIN{srand(3);for(i=0;i<n;i++){printf "%c%c%c%c%c%c\n",
                       97+int(rand()*26),97+int(rand()*26),97+int(rand()*26),
                       97+int(rand()*26),97+int(rand()*26),97+int(rand()*26)}}'                    > "$2"; }
gen_csv()          { awk -v n="$1" 'BEGIN{srand(6);print "id,name,value";
                       for(i=0;i<n;i++)printf "%d,item%06d,%d\n",i,int(rand()*1000000),int(rand()*1000000)}' > "$2"; }
gen_jsonl()        { awk -v n="$1" 'BEGIN{srand(7);for(i=0;i<n;i++)printf "{\"id\":%d,\"value\":%d}\n",i,int(rand()*1000000)}' > "$2"; }

bytes_of() { wc -c < "$1" | tr -d ' '; }
lines_of() { wc -l < "$1" | tr -d ' '; }

# --------------------------------------------------------------------------
# Measurement helpers
# --------------------------------------------------------------------------
# Parallel arrays the caller fills before run_suite: LBL[] CMD[] BYTES[] LINES[]
declare -a LBL CMD BYTES LINES

rss_of() { # cmd-string -> "NN MB" or "n/a"
  [ "$TIME_V" = 1 ] || { echo "n/a"; return; }
  local kb
  /usr/bin/time -v bash -c "$1" 2>"$DATA/.rss" >/dev/null || true
  kb=$(grep "Maximum resident" "$DATA/.rss" | grep -oE '[0-9]+' | head -1)
  [ -n "$kb" ] && awk -v k="$kb" 'BEGIN{printf "%.0f MB", k/1024}' || echo "n/a"
}

run_suite() { # $1 = section title
  local title="$1"
  local md="$DATA/$(echo "$title" | tr ' /,()' '_____').md"
  local json="$md.json"
  local args=() i
  for i in "${!LBL[@]}"; do args+=( -n "${LBL[$i]}" "${CMD[$i]}" ); done
  hyperfine --warmup "$WARMUP" --runs "$RUNS" \
            --export-markdown "$md" --export-json "$json" "${args[@]}"

  local -a MEANS
  mapfile -t MEANS < <(grep -o '"mean":[^,]*' "$json" | sed 's/.*: *//')

  {
    echo "### $title"; echo
    cat "$md"; echo
    echo "| Command | Throughput (MB/s) | Mlines/s | Peak RSS |"
    echo "|:---|---:|---:|---:|"
    for i in "${!LBL[@]}"; do
      local mean="${MEANS[$i]:-}" mbps mls rss
      mbps=$(awk -v b="${BYTES[$i]}" -v t="$mean" 'BEGIN{if(t>0)printf "%.0f",b/t/1e6;else print "-"}')
      mls=$(awk  -v l="${LINES[$i]}" -v t="$mean" 'BEGIN{if(t>0)printf "%.2f",l/t/1e6;else print "-"}')
      rss=$(rss_of "${CMD[$i]}")
      echo "| \`${LBL[$i]}\` | $mbps | $mls | $rss |"
    done
    echo
  } >> "$OUT"

  for i in "${!LBL[@]}"; do
    echo "$title,${LBL[$i]},${MEANS[$i]:-},${BYTES[$i]},${LINES[$i]}" >> "$CSVOUT"
  done
  LBL=(); CMD=(); BYTES=(); LINES=()
}

add() { # label cmd file  -> push a measurement using file's byte/line counts
  LBL+=("$1"); CMD+=("$2"); BYTES+=("$(bytes_of "$3")"); LINES+=("$(lines_of "$3")")
}

parity() { # desc  actual-cmd  expected-cmd
  if cmp -s <(eval "$2") <(eval "$3"); then echo "  ok: $1"; else echo "  MISMATCH: $1"; exit 1; fi
}

# --------------------------------------------------------------------------
# Report header
# --------------------------------------------------------------------------
DISK_FREE_KB=$(df -Pk "$DATA" | awk 'NR==2{print $4}')
{
  echo "# xort vs GNU sort — benchmark results"
  echo
  echo "Generated by \`scripts/benchmark.sh\`$( [ "$QUICK" = 1 ] && echo ' (QUICK mode)')."
  echo
  echo "## Environment"
  echo
  echo "| | |"
  echo "|:---|:---|"
  echo "| CPU | $(lscpu 2>/dev/null | sed -n 's/^Model name: *//p' | head -1) |"
  echo "| Cores | $NPROC |"
  echo "| RAM | $(awk '/MemTotal/{printf "%.1f GiB", $2/1048576}' /proc/meminfo) |"
  echo "| Kernel | $(uname -sr) |"
  echo "| Data FS | $(df -PT "$DATA" | awk 'NR==2{print $2}') on $(df -P "$DATA" | awk 'NR==2{print $1}') |"
  echo "| xort | $($XORT --version 2>/dev/null | head -1) |"
  echo "| GNU sort | $(sort --version | head -1 | grep -oE '[0-9]+\.[0-9]+') |"
  echo "| hyperfine | $(hyperfine --version | awk '{print $2}') |"
  echo "| mlr | $( [ "$HAVE_MLR" = 1 ] && mlr --version | head -1 || echo 'not installed') |"
  echo "| jq | $( [ "$HAVE_JQ" = 1 ] && jq --version || echo 'not installed') |"
  echo "| csvsort | $( [ "$HAVE_CSVSORT" = 1 ] && echo 'present' || echo 'not installed') |"
  echo
  echo "## Methodology"
  echo
  echo "- Both tools run under \`LC_ALL=C\` (byte ordering) — the conservative comparison."
  echo "- $RUNS timed runs, $WARMUP warmup (page cache primed); inputs are regular files."
  echo "- Output verified **byte-identical** to GNU \`sort\` before timing, except the"
  echo "  structured section (mlr/jq reserialize, so only timing is compared there)."
  [ "$TIME_V" = 1 ] || echo "- Peak RSS shows \`n/a\`: GNU \`/usr/bin/time -v\` is not available here."
  echo
} > "$OUT"

# ==========================================================================
echo "Datasets in $DATA"
echo "== A. Size scaling =="
note "## A. Size scaling"
for n in $SIZES; do
  if [ "$n" -ge 100000000 ]; then
    # ~1 GB on disk; need headroom for sort + output. Skip if tight.
    if [ "${DISK_FREE_KB:-0}" -lt 5000000 ]; then
      note "_100M skipped: < 5 GB free on the data filesystem._"; continue
    fi
  fi
  ints="$DATA/ints_$n.txt"; words="$DATA/words_$n.txt"
  gen_uniform_ints "$n" "$ints"; gen_words "$n" "$words"
  parity "numeric ${n}" "$XORT -n '$ints'" "sort -n '$ints'"
  parity "text ${n}"    "$XORT '$words'"   "sort '$words'"
  add "GNU numeric ${n}" "sort -n '$ints' > /dev/null" "$ints"
  add "xort numeric ${n}" "$XORT -n '$ints' > /dev/null" "$ints"
  add "GNU text ${n}" "sort '$words' > /dev/null" "$words"
  add "xort text ${n}" "$XORT '$words' > /dev/null" "$words"
  run_suite "Numeric + text, ${n} rows"
done

# ==========================================================================
echo "== B. Distribution =="
note "## B. Data distribution (numeric, fixed size)"
DSIZE=$(for n in $SIZES; do echo "$n"; done | awk '$1<=10000000' | tail -1)
[ -n "$DSIZE" ] || DSIZE=$(echo $SIZES | awk '{print $1}')
note "Fixed at **${DSIZE} rows**; \`xort -n\` vs \`sort -n\` on each distribution."
for dist in uniform sorted reverse nearly lowcard; do
  f="$DATA/dist_${dist}.txt"
  "gen_${dist}_ints" "$DSIZE" "$f"
  parity "$dist" "$XORT -n '$f'" "sort -n '$f'"
  add "GNU $dist" "sort -n '$f' > /dev/null" "$f"
  add "xort $dist" "$XORT -n '$f' > /dev/null" "$f"
  run_suite "Distribution: $dist (${DSIZE} rows)"
done

# ==========================================================================
echo "== C. External / >RAM (-S spill) =="
note "## C. External merge sort (>RAM, \`-S\`)"
EXT="$DATA/ext.txt"
gen_uniform_ints "$DSIZE" "$EXT"
SBUF="4M"
chunks=$($XORT -n -S "$SBUF" --stats "$EXT" 2>&1 >"$DATA/ext.sorted" | grep -oE '[0-9]+ spilled' | grep -oE '[0-9]+')
parity "external == in-memory" "cat '$DATA/ext.sorted'" "$XORT -n '$EXT'"
note "Forced spill with \`-S $SBUF\` on ${DSIZE} rows → **${chunks:-?} spilled chunks**, output byte-identical to the in-memory sort."
add "GNU sort -S $SBUF" "sort -n -S $SBUF '$EXT' > /dev/null" "$EXT"
add "xort -S $SBUF (external)" "$XORT -n -S $SBUF '$EXT' > /dev/null" "$EXT"
add "xort (in-memory)" "$XORT -n '$EXT' > /dev/null" "$EXT"
run_suite "External vs in-memory (${DSIZE} rows)"

# ==========================================================================
echo "== D. Field keys =="
note "## D. Field keys (\`-k\`/\`-t\`)"
KF="$DATA/keyed.csv"; gen_csv "$DSIZE" "$KF"
# Treat the CSV as plain text and sort by column 3 (value) numerically.
parity "-k3,3n -t," "$XORT -t, -k3,3n '$KF'" "sort -t, -k3,3n '$KF'"
parity "multi-key"  "$XORT -t, -k2,2 -k3,3n '$KF'" "sort -t, -k2,2 -k3,3n '$KF'"
add "GNU -k3,3n -t," "sort -t, -k3,3n '$KF' > /dev/null" "$KF"
add "xort -k3,3n -t," "$XORT -t, -k3,3n '$KF' > /dev/null" "$KF"
add "GNU -k2,2 -k3,3n" "sort -t, -k2,2 -k3,3n '$KF' > /dev/null" "$KF"
add "xort -k2,2 -k3,3n" "$XORT -t, -k2,2 -k3,3n '$KF' > /dev/null" "$KF"
run_suite "Field-key sorts (${DSIZE} rows)"

# ==========================================================================
echo "== E. Structured (CSV / JSON) =="
note "## E. Structured formats (the differentiators)"
note "GNU \`sort\` cannot sort by CSV column **name** (quoting-aware) or JSON fields; the natural rivals are mlr / csvsort / jq. Output is **not** byte-compared (mlr/jq reserialize); timing only."
# Cap the structured size: jq slurps + reserializes and is very slow at 10M+,
# and 2M rows is already illustrative.
SF=$(awk -v d="$DSIZE" 'BEGIN{print (d<2000000)?d:2000000}')
# CSV
CSVF="$DATA/struct.csv"; gen_csv "$SF" "$CSVF"
add "xort --csv -k value -n" "$XORT --csv --header -k value -n '$CSVF' > /dev/null" "$CSVF"
[ "$HAVE_MLR" = 1 ]     && add "mlr sort -nf value"   "mlr --csv sort -nf value '$CSVF' > /dev/null" "$CSVF"
[ "$HAVE_CSVSORT" = 1 ] && add "csvsort -c value"     "csvsort -c value '$CSVF' > /dev/null" "$CSVF"
if [ "${#LBL[@]}" -gt 1 ]; then run_suite "CSV: sort by column 'value' (${SF} rows)"
else note "_CSV competitors (mlr/csvsort) not installed — xort-only timing:_"; run_suite "CSV: sort by column 'value' (${SF} rows)"; fi
# JSON
JF="$DATA/struct.jsonl"; gen_jsonl "$SF" "$JF"
add "xort --jsonl -k .value -n" "$XORT --jsonl -k .value -n '$JF' > /dev/null" "$JF"
[ "$HAVE_JQ" = 1 ] && add "jq -s sort_by(.value)" "jq -s -c 'sort_by(.value)|.[]' '$JF' > /dev/null" "$JF"
run_suite "JSONL: sort by .value (${SF} rows)"

# ==========================================================================
echo "== F. Thread scaling =="
note "## F. Thread scaling (\`--parallel\`)"
TS="$DATA/threads.txt"; gen_uniform_ints "$DSIZE" "$TS"
P=1; PS=()
while [ "$P" -le "$NPROC" ]; do PS+=("$P"); P=$((P*2)); done
[ "${PS[-1]}" -eq "$NPROC" ] || PS+=("$NPROC")
for p in "${PS[@]}"; do add "xort --parallel $p" "$XORT -n --parallel $p '$TS' > /dev/null" "$TS"; done
add "GNU sort --parallel $NPROC" "sort -n --parallel=$NPROC '$TS' > /dev/null" "$TS"
run_suite "Thread scaling, numeric (${DSIZE} rows)"

echo
echo "Wrote $OUT and $CSVOUT"
