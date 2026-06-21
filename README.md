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
both fast and predictable. On a 10M-line integer file (4 cores):

| Command | Wall time | Output |
|---|---|---|
| `sort -n` (GNU coreutils 9.4, `LC_ALL=C`) | ~7.2 s | — |
| `fsort -n` | **~3.7 s** | byte-identical to GNU |
| `sort -n \| head -10` | ~2.1 s | — |
| `fsort -n --top 10` | **~0.76 s** | byte-identical to `sort \| head` |

*(Indicative numbers from a 4-core dev box; reproduce with
[`scripts/difftest.sh`](scripts/difftest.sh) and your own corpus. Honest,
reproducible benchmarks are a project goal — these will be replaced with a
`hyperfine` suite.)*

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
