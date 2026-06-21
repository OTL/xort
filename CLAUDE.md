# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this is

`xort` — a fast, parallel, drop-in replacement for the Unix `sort` command (binary and library are both named `xort`). The goal is to be a faithful GNU `sort` drop-in *and* measurably faster, with extra features GNU lacks (`--top`, `--count`, `--header`, CSV/JSON, colorized keys). Default ordering is byte comparison, so output must match `LC_ALL=C sort`.

## Commands

```sh
cargo build --release                       # optimized binary at target/release/xort
cargo test --all                            # 23 unit + 55 integration tests
cargo test --test cli <name>                # run one integration test by name
cargo test -p xort <name>                   # run one unit test (in src/*.rs #[cfg(test)])
cargo clippy --all-targets -- -D warnings   # lint gate (CI fails on any warning)
cargo fmt --check                           # format gate
RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --lib   # doc gate (#![warn(missing_docs)] is on)
cargo llvm-cov --fail-under-lines 80        # coverage gate (CI enforces >=80%)

bash scripts/difftest.sh                    # PRIMARY correctness gate: diff vs GNU sort (130 cases)
XORT=./target/debug/xort bash scripts/difftest.sh   # run difftest against a debug build
QUICK=1 bash scripts/benchmark.sh           # fast multi-axis benchmark (CI smoke)
```

CI (`.github/workflows/ci.yml`) runs three jobs: `test/lint` (fmt, clippy, test, doc, difftest), `bench-smoke` (`QUICK=1` benchmark — correctness gates only, no timing assertions), and `coverage` (>=80% lines).

## Correctness is the contract

Output must be **byte-identical to `LC_ALL=C sort`**. `scripts/difftest.sh` is the authoritative gate — any change to comparison, key parsing, stability, or dedup semantics must keep it at 130/130. When touching those areas, run it (and extend it) before trusting anything. Exit codes follow GNU: `0` success, `1` for `-c` disorder, `2` for errors.

## Architecture

**Layering.** `src/main.rs` is a thin shell: parse args → `Cli::into_config` → `engine::run` → print stats / map exit code. Everything testable lives in the library (`src/lib.rs`). The core design boundary: the **format layer decides *what the key is*; the engine only orders key bytes.** The engine never learns about CSV/JSON.

**`Config` (`src/config.rs`) is the resolved job.** `Cli::into_config` validates flags and produces an immutable `Config`. Two helpers on it drive dispatch and are the place to look first:
- `is_simple_global()` — true when there are no `-k` keys, no typed flag (`-g/-h/-V/-M`), and plain text format. This selects the fast Milestone-1 paths.
- `build_sorter()` — builds a `key::Sorter` (the multi-key comparison plan) for everything else. `suppress_last_resort = stable || unique`.

**`engine::run` is a dispatch tree** (in this priority order — match it when adding paths):
1. structured format (`Format != Text`) → `format::run_structured`
2. `-S` set and a plain/unique sort → `external::run_external` (the only path that does NOT hold all input in memory)
3. `-m` → `merge_sorted` (k-way merge of pre-sorted inputs)
4. `-c` → `check_sorted`
5. `--count` → `grouped_counts`
6. else in-memory: `is_simple_global()` ? (`numeric_order` | `byte_order`) : `general_order`

**Comparators.** `src/compare.rs` holds the per-kind comparison logic (`compare_kind` dispatches on `key::Kind`: Bytes/Numeric/General/Human/Version/Month) plus `NumericKey` (a parse-once numeric representation). `src/key.rs` holds `KeyDef` (one `-k` spec, 1-based fields), `parse_key_spec` (the densest GNU-compat surface — `F.C,F.C[opts]` with all-or-nothing global option inheritance), and `Sorter` (`compare`, `key_equal`, `check_compare`, key-range extraction for highlighting).

**Decorate-sort-undecorate (DSU) is load-bearing for speed — do not regress it.** The hot sort paths precompute each line's key *once* rather than re-extracting/re-parsing on every comparison (which is O(n log n) extractions):
- `numeric_order` — global `-n`, decorates with `NumericKey`.
- `single_key_order` — one `-k` key; decorates each line into a `Dec` (numeric parsed up front, else a zero-copy slice).
- `general_order` multi-key — decorates all keys into one flat `Vec<Dec>` (`dec[i*k + j]`); records carry only an index into it, avoiding a per-line allocation.

All in-memory paths sort borrowed `&[u8]` slices into one buffer (zero-copy) via rayon `par_sort_*`; `--top` uses `select_nth_unstable_by` to avoid a full sort. If you add a path, preserve both properties.

**External sort (`src/external.rs`).** Reads input in `-S`-sized blocks of zero-copy line slices, sorts each chunk in parallel (rayon), spills to `tempfile` runs, then k-way merges through a `BinaryHeap` with large buffers. A single chunk streams straight to output with no temp file. `run_external` returns `(lines, chunks)`; `chunks > 1` means it genuinely spilled, surfaced via `--stats`.

**Structured formats (`src/format.rs`).** CSV/TSV via the `csv` crate (`ByteRecord`, quoting-correct, `-k` resolves a column by name against the header); JSON array / JSONL via `serde_json` with a `.path.to.field` key and a type-preserving order (Null < Bool < Number < String). `-z` is rejected with CSV. This module owns its own read/sort/write; it does not go through the text engine.

## Gotchas

- **`-h` and `-V` are NOT help/version.** `src/cli.rs` sets `disable_help_flag`/`disable_version_flag` so `-h` = human-numeric and `-V` = version-sort (GNU semantics); `--help`/`--version` are long-only.
- **Stability:** there is no separate stable sort path — equal-key ties fall back to a whole-line last-resort comparison (under `global_reverse`) unless `suppress_last_resort` (`-s`/`-u`) is set, in which case the parallel *stable* sort preserves input order.
- **Color must never corrupt pipes:** key highlighting is gated on TTY + `NO_COLOR` + `--color` via `anstream` (`src/diag.rs`).
- Completions (`--completions SHELL`) and the man page (`--man`) are generated at runtime in `cli.rs`/`main.rs` via `clap_complete`/`clap_mangen` — there is no `build.rs`.

## Benchmarks

`scripts/benchmark.sh` is a multi-axis suite (size scaling, data distribution, external/>RAM, field keys, structured vs mlr/jq/csvsort, thread scaling) that parity-checks each scenario against GNU before timing. Knobs: `QUICK=1`, `SIZES="1000000 10000000"`, `RUNS`, `WARMUP`, `XORT=<bin>`. It detects `mlr`/`jq`/`csvsort` and `/usr/bin/time -v` and skips gracefully when absent. Results land in `benchmarks/results.md` / `results.csv`. When reporting numbers, prefer a clean full run over a single sample — run-to-run variance is real (multi-key `-k` is roughly at parity with GNU and noisy).

When benchmarking a change, build into a separate target dir (`CARGO_TARGET_DIR=/tmp/... cargo build --release`) so you don't clobber `target/release/xort` while a benchmark run is using it.
