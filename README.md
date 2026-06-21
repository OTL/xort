# fsort

**A fast, modern, parallel drop-in replacement for the Unix `sort` command.**

`fsort` aims to be to `sort` what [ripgrep] is to `grep` and [fd] is to `find`:
compatible enough to drop into your existing scripts and muscle memory, but
**parallel by default**, measurably faster, and with a few things the classic
tool simply can't do.

> **Status:** early. Milestone 1 (a correct, parallel plain sort with the core
> flags) is implemented and differentially tested against GNU `sort`. See the
> [roadmap](#roadmap).

[ripgrep]: https://github.com/BurntSushi/ripgrep
[fd]: https://github.com/sharkdp/fd

## Why

GNU `sort` leaves performance on the table — historically single-threaded, and
it pays a heavy Unicode-collation cost in non-`C` locales. `fsort` is parallel
by default and compares bytes by default (equivalent to `LC_ALL=C`), which is
both fast and predictable.

Measured with [`hyperfine`](https://github.com/sharkdp/hyperfine) (5 runs,
1 warmup) against **GNU coreutils sort 9.4 on 4 cores**. To be fair, **both
tools run under `LC_ALL=C` and use all cores** — we deliberately do *not*
exploit GNU sort's much larger slowdown in a UTF-8 locale. Output is verified
**byte-identical** to GNU in every case.

| Workload (input) | GNU sort | fsort | Speedup |
|---|---:|---:|---:|
| Numeric, 10M ints (`-n`) | 4.81 s | **2.03 s** | **2.37×** |
| Float, 10M decimals (`-n`) | 4.92 s | **2.32 s** | **2.12×** |
| Text, 8M lines | 2.15 s | **1.23 s** | **1.74×** |
| Unique text, 8M lines (`-u`) | 2.54 s | **1.54 s** | **1.65×** |
| Top-100, 10M ints (`--top 100`) | 2.12 s | **0.64 s** | **3.33×** |

Reproduce on your own hardware and corpus with
[`scripts/benchmark.sh`](scripts/benchmark.sh); the raw hyperfine tables are in
[`benchmarks/results.md`](benchmarks/results.md). Correctness is independently
checked against GNU `sort` by [`scripts/difftest.sh`](scripts/difftest.sh).

## Install

```sh
cargo install --path .   # from a checkout
```

## Usage

`fsort` accepts the common GNU `sort` flags:

```
fsort [FILE...]            sort lines of text (stdin if no files)

  -n, --numeric-sort       compare by leading numeric value
  -r, --reverse            reverse the result
  -u, --unique             output only the first of each equal-key run
  -s, --stable             keep input order among equal keys
  -f, --ignore-case        fold lower case to upper case
  -b, --ignore-leading-blanks
  -z, --zero-terminated    lines end with NUL, not newline
  -c, --check              check whether input is sorted; don't sort
  -o, --output=FILE        write result to FILE
      --parallel=N         use N threads

New in fsort:
      --top=N              emit only the first N lines in sort order, using a
                           bounded selection instead of a full sort + head
      --stats              print line counts and elapsed time to stderr
```

### Examples

```sh
fsort -n data.txt                 # numeric sort
fsort -u names.txt                # sorted unique
fsort -n --top 10 metrics.txt     # 10 smallest, far cheaper than sort | head
du -b * | fsort -n --top 5        # 5 largest, etc.
```

## Compatibility

`fsort`'s default ordering is a byte comparison (`LC_ALL=C` semantics), so its
output matches `LC_ALL=C sort`. The differential test
([`scripts/difftest.sh`](scripts/difftest.sh)) checks `fsort` against GNU `sort`
across random word/number inputs and flag combinations.

## Roadmap

- [x] **M1 — table stakes:** parallel in-memory sort, `-n -r -u -s -f -b -z -c -o`,
      `--top`, `--stats`, differential testing.
- [ ] **M2 — GNU-compat depth:** `-k`/`-t` field keys, `-g -h -V -M`, `-m`,
      `-S`/`-T` and external merge sort for inputs larger than RAM.
- [ ] **M3 — killer features:** `--header`, fused `-u --count` (built-in
      `sort | uniq -c`).
- [ ] **M4 — structured formats:** CSV/TSV (sort by column name), JSON/JSONL.
- [ ] **M5 — UX polish:** rich `--check` diagnostics, colorized key highlight,
      shell completions, man page.

## Development

```sh
cargo test --all                 # unit + integration tests
cargo clippy --all-targets -- -D warnings
bash scripts/difftest.sh         # diff against GNU sort (requires `sort`)
```

## License

MIT OR Unlicense, at your option.
