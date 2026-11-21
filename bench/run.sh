#!/usr/bin/env bash
# Measures logshape throughput and peak memory on a synthetic log.
#
#   bench/run.sh                      # 5,000,000 lines, seed 42, 5 runs
#   N_LINES=20000000 RUNS=3 bench/run.sh
#
# The log comes from examples/genlog.rs (see that file for what it contains)
# and is fully determined by N_LINES and SEED. Wall time, CPU time (user plus
# system) and peak resident set size are taken from /usr/bin/time. Throughput
# is computed from wall time; MB means 10^6 bytes.
set -euo pipefail

N_LINES="${N_LINES:-5000000}"
SEED="${SEED:-42}"
RUNS="${RUNS:-5}"

cd "$(dirname "$0")/.."
cargo build --release --quiet --bin logshape --example genlog

mkdir -p bench/data
log="bench/data/synthetic-${N_LINES}-${SEED}.log"
if [ ! -s "$log" ]; then
  target/release/examples/genlog --lines "$N_LINES" --seed "$SEED" > "$log"
fi
bytes=$(wc -c < "$log" | tr -d ' ')
echo "log:      $log"
echo "lines:    $N_LINES"
echo "bytes:    $bytes"
echo "command:  target/release/logshape --json $log > /dev/null"
echo

# One unmeasured run so that every measured run reads from the page cache.
target/release/logshape --json "$log" > /dev/null

if [ "$(uname)" = "Darwin" ]; then
  time_flag="-l"
else
  time_flag="-v"
fi

stats="bench/data/time.txt"
results=""
for run in $(seq 1 "$RUNS"); do
  /usr/bin/time "$time_flag" target/release/logshape --json "$log" > /dev/null 2> "$stats"
  if [ "$(uname)" = "Darwin" ]; then
    secs=$(awk '/ real / { print $1 }' "$stats")
    cpu=$(awk '/ real / { print $3 + $5 }' "$stats")
    rss=$(awk '/maximum resident set size/ { print $1 }' "$stats")
  else
    secs=$(awk -F': ' '/Elapsed \(wall clock\)/ { n = split($2, t, ":"); s = 0; for (i = 1; i <= n; i++) s = s * 60 + t[i]; print s }' "$stats")
    cpu=$(awk -F': ' '/(User|System) time/ { c += $2 } END { print c }' "$stats")
    rss=$(awk -F': ' '/Maximum resident set size/ { print $2 * 1024 }' "$stats")
  fi
  awk -v s="$secs" -v c="$cpu" -v n="$N_LINES" -v b="$bytes" -v r="$rss" -v run="$run" 'BEGIN {
    printf "run %d:  wall %.2f s  cpu %.2f s  %.0f lines/s  %.1f MB/s  peak RSS %.1f MiB\n", run, s, c, n / s, b / s / 1e6, r / 1048576
  }'
  results="${results}${secs}"$'\n'
done
rm -f "$stats"

printf '%s' "$results" | sort -n | awk -v n="$N_LINES" -v b="$bytes" '
  { t[NR] = $1 }
  END {
    s = t[int((NR + 1) / 2)]
    printf "\nmedian:  %.2f s  %.0f lines/s  %.1f MB/s\n", s, n / s, b / s / 1e6
  }'

# The same file through two standard tools, to show how busy the machine is.
echo
for reference in "wc -l" "grep -c ERROR"; do
  # shellcheck disable=SC2086
  /usr/bin/time -p $reference "$log" > /dev/null 2> "$stats"
  awk -v what="$reference" '
    /^real/ { wall = $2 } /^user/ { cpu += $2 } /^sys/ { cpu += $2 }
    END { printf "reference:  %-14s wall %.2f s  cpu %.2f s\n", what, wall, cpu }' "$stats"
done
rm -f "$stats"
