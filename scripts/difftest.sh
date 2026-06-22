#!/usr/bin/env bash
# Differential test: compare xort output against GNU sort across random inputs
# and flag combinations. xort uses byte ordering (LC_ALL=C), so we pin GNU sort
# to the C locale for a fair comparison.
set -u
XORT="${XORT:-./target/release/xort}"
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
gen_ints() { # pure integers (no fractions) to exercise the -n radix fast path
  awk -v n="$1" 'BEGIN{
    srand(23+n);
    j[0]="0";j[1]="abc";j[2]="";j[3]="-";
    for(i=0;i<n;i++){
      r=int(rand()*8);
      if(r==0) print int(rand()*200)-100;                 # small +/-
      else if(r==1) print int(rand()*1000000)-500000;     # wide range
      else if(r==2) printf "%07d\n", int(rand()*1000);     # leading zeros
      else if(r==3) printf "+%d\n", int(rand()*1000);      # explicit plus
      else if(r==4) print "-" int(rand()*1000);            # negative text
      else if(r==5) print j[int(rand()*4)];                # zero/junk/empty
      else print int(rand()*50);                           # low-cardinality dups
    }
  }'
}
gen_table() { # CSV-ish rows with several typed columns, space and colon variants
  awk -v n="$1" 'BEGIN{
    srand(11+n);
    names[0]="alice";names[1]="bob";names[2]="carol";names[3]="dave";
    mon[0]="Jan";mon[1]="Mar";mon[2]="Feb";mon[3]="Dec";mon[4]="Aug";
    for(i=0;i<n;i++){
      a=names[int(rand()*4)]; b=int(rand()*1000)-500;
      c=mon[int(rand()*5)]; d=int(rand()*100);
      print a" "b" "c" "d;
    }
  }'
}
gen_table_colon() {
  awk -v n="$1" 'BEGIN{
    srand(13+n);
    names[0]="alice";names[1]="bob";names[2]="carol";names[3]="dave";
    for(i=0;i<n;i++){ print names[int(rand()*4)]":"int(rand()*1000)-500":"int(rand()*100) }
  }'
}
gen_versions() {
  awk -v n="$1" 'BEGIN{
    srand(17+n);
    for(i=0;i<n;i++){ printf "v%d.%d.%d\n", int(rand()*12), int(rand()*20), int(rand()*30) }
  }'
}
gen_human() {
  awk -v n="$1" 'BEGIN{
    srand(19+n); s[0]="K";s[1]="M";s[2]="G";s[3]="";s[4]="T";
    for(i=0;i<n;i++){ printf "%d%s\n", int(rand()*900)+1, s[int(rand()*5)] }
  }'
}

fail=0; total=0
check() { # $1 = description; rest = flags
  local desc="$1"; shift
  total=$((total+1))
  out_g=$(sort "$@" "$input" 2>/dev/null)
  out_f=$("$XORT" "$@" "$input" 2>/dev/null)
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

  # --count must match `sort | uniq -c`
  total=$((total+1))
  cg=$(sort "$input" | uniq -c); cf=$("$XORT" --count "$input" 2>/dev/null)
  if [ "$cg" != "$cf" ]; then
    fail=$((fail+1)); echo "MISMATCH [words --count n=$size]"
    diff <(printf '%s' "$cg") <(printf '%s' "$cf") | head -6; echo "---"
  fi

  input="$tmp/nums.$size"; gen_nums "$size" > "$input"
  check "nums -n n=$size"      -n
  check "nums -nr n=$size"     -n -r
  check "nums -nu n=$size"     -n -u
  check "nums -g n=$size"      -g

  # pure integers exercise the -n radix fast path (byte-identical to GNU)
  input="$tmp/ints.$size"; gen_ints "$size" > "$input"
  check "ints -n n=$size"      -n
  check "ints -nr n=$size"     -n -r
  check "ints -nu n=$size"     -n -u
  check "ints -nur n=$size"    -n -u -r
  check "ints -ns n=$size"     -n -s

  # -k / -t field keys (whitespace-separated table)
  input="$tmp/table.$size"; gen_table "$size" > "$input"
  check "table -k1 n=$size"       -k1
  check "table -k2,2n n=$size"    -k2,2n
  check "table -k2n n=$size"      -k2n
  check "table -k1,1 -k2n"        -k1,1 -k2n
  check "table -k3,3M n=$size"    -k3,3M
  check "table -k4,4nr n=$size"   -k4,4nr
  check "table -k1.2 n=$size"     -k1.2
  check "table -k2,2 -u"          -k2,2 -u
  check "table -s -k1,1"          -s -k1,1

  # colon-delimited with -t
  input="$tmp/colon.$size"; gen_table_colon "$size" > "$input"
  check "colon -t: -k2,2n"        -t: -k2,2n
  check "colon -t: -k1,1"         -t: -k1,1
  check "colon -t: -k2,2nr -u"    -t: -k2,2nr -u

  # version & human
  input="$tmp/ver.$size"; gen_versions "$size" > "$input"
  check "versions -V n=$size"     -V
  input="$tmp/hum.$size"; gen_human "$size" > "$input"
  check "human -h n=$size"        -h
done

echo "=== $((total-fail))/$total passed ==="
[ "$fail" -eq 0 ]
