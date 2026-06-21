#!/usr/bin/env bash
# Differential test: compare fsort output against GNU sort across random inputs
# and flag combinations. fsort uses byte ordering (LC_ALL=C), so we pin GNU sort
# to the C locale for a fair comparison.
set -u
FSORT="${FSORT:-./target/release/fsort}"
export LC_ALL=C
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT

gen_words() { # random short ASCII words, some duplicates, mixed case
  awk -v n="$1" 'BEGIN{
    srand(42+n);
    w[0]="apple";w[1]="Banana";w[2]="cherry";w[3]="apple";w[4]="DATE";
    w[5]="fig";w[6]="grape";w[7]="";w[8]="  spaced";w[9]="banana";
    for(i=0;i<n;i++){ print w[int(rand()*10)] }
  }'
}
gen_nums() { # random ints/floats incl. negatives and junk
  awk -v n="$1" 'BEGIN{
    srand(7+n);
    for(i=0;i<n;i++){
      r=int(rand()*5);
      if(r==0) print int(rand()*2000)-1000;
      else if(r==1) printf "%.2f\n", rand()*100-50;
      else if(r==2) print "abc"; else if(r==3) print "";
      else print int(rand()*1000000);
    }
  }'
}

fail=0; total=0
check() { # $1 = description; rest = flags
  local desc="$1"; shift
  total=$((total+1))
  out_g=$(sort "$@" "$input" 2>/dev/null)
  out_f=$("$FSORT" "$@" "$input" 2>/dev/null)
  if [ "$out_g" != "$out_f" ]; then
    fail=$((fail+1))
    echo "MISMATCH [$desc] flags: $*"
    diff <(printf '%s' "$out_g") <(printf '%s' "$out_f") | head -8
    echo "---"
  fi
}

for size in 0 1 5 50 500; do
  input="$tmp/words.$size"; gen_words "$size" > "$input"
  check "words n=$size"        ;
  check "words -r n=$size"     -r
  check "words -u n=$size"     -u
  check "words -f n=$size"     -f
  check "words -b n=$size"     -b
  check "words -fu n=$size"    -f -u
  check "words -ru n=$size"    -r -u

  input="$tmp/nums.$size"; gen_nums "$size" > "$input"
  check "nums -n n=$size"      -n
  check "nums -nr n=$size"     -n -r
  check "nums -nu n=$size"     -n -u
done

echo "=== $((total-fail))/$total passed ==="
[ "$fail" -eq 0 ]
