# opengloss-rs

Utilities for working with the [OpenGloss](https://huggingface.co/datasets/mjbommar/opengloss-dictionary)
dictionary dataset in Rust. The crate builds a static finite-state transducer for the lexeme index
and embeds the full entry metadata (definitions, relations, encyclopedia text) directly inside the
binary.

## Prerequisites

- Rust 1.82+ (edition 2024 project).
- Python 3 + [`uv`](https://github.com/astral-sh/uv) to download/export the dataset.
- Enough disk/RAM: the raw JSONL dump is ~3 GB, and the compiled binary is currently ~830 MB.

## Exporting the dataset

The Hugging Face files are **not** committed to git. Export them locally before building:

1. Export the lexeme list and entry metadata from Hugging Face (requires Python + `uv`):
   ```bash
   uv run --with datasets python scripts/export_lexemes.py
   ```
   This writes `data/lexemes.tsv` (lexeme IDs) and `data/entries.jsonl`
   (senses, morphology, encyclopedic metadata). The files live outside of `git`
   because they are large.
2. Build the crate. The `build.rs` script turns those files into a compact
   [fst](https://docs.rs/fst/latest/fst/) map plus a packed `DataStore`, compresses everything with
   Zstd, and embeds the artifacts via `include_bytes!`:
   ```bash
   cargo build --release        # or cargo run / cargo bench
   ```

If you change the dataset or script output, rebuild to regenerate the embedded store.

## Features & CLI

- `cli` (default): pulls in the Clap-based command-line interface. Disable via
  `cargo build --no-default-features` if you only need the library.

When enabled, the CLI provides rich output or JSON via `--json`:

```bash
cargo run -- lexeme get 3d "a b c labels"
cargo run -- lexeme prefix geo --limit 5
cargo run -- --json lexeme prefix bio
cargo run -- lexeme show 3d
cargo run -- --json lexeme show 42 --by-id
```

Example JSON output:

```bash
cargo run -- --json lexeme get sphere
# [
#   {
#     "lexeme_id": 12345,
#     "word": "sphere"
#   }
# ]
```

## Implementation notes

- Lexeme IDs are assigned densely in insertion order while exporting, so they fit in `u32`.
- The trie is generated at build time via `fst::MapBuilder` and included with `include_bytes!`,
  so runtime lookups are zero-copy.
- A second build artifact (`opengloss_data.rkyv.zst`) packs the entry metadata, parts of speech,
  senses, and aggregated synonym/antonym/example lists. It is Zstd-compressed during build so the
  binary stays manageable, then decompressed/aligned once at runtime for zero-copy access.
- `LexemeIndex` in `src/lib.rs` exposes exact-match (`get`), prefix (`prefix`), and entry
  resolution helpers (`entry_by_word`, `entry_by_id`).
- All short strings (lexemes, relation labels, etc.) are interned into a packed UTF-8 blob so they
  can be accessed zero-copy without keeping separate heap allocations alive.
- Long-form entry text and encyclopedia prose are placed in a compressed chunk store during the
  build step, so the binary still carries the full content while only decompressing paragraphs on
  demand.

## Performance snapshot

Criterion benchmarks live in `benches/lexeme.rs`. Run them with:

```bash
cargo bench
```

Useful numbers from the latest run (Linux x86_64, release build):

| Benchmark | Median | Notes |
| --- | --- | --- |
| `cold_load::decompress_blob` | 349 ms | One-time cost to inflate the embedded data store |
| `entry_lookup::algorithm` | 6.5 µs | Reads lexeme + entry text |
| `entry_lookup::dog` | 10.3 µs | Longer article, still sub-11 µs |
| `prefix_lookup::bio_10` | 1.73 µs | FST prefix search returning 10 results |
| `prefix_lookup::micro_25` | 4.12 µs | Prefix search returning 25 results |

First access inflates the archive into an `AlignedVec`, so the CLI stabilizes at ~3.1 GB RSS after
the initial lookup. Subsequent queries run without additional allocation or decompression.

## Packaging releases

Use the helper script to produce GitHub-friendly artifacts (tarball + SHA256):

```bash
scripts/package_release.sh v0.1.0
ls dist/
```

The archive currently includes the `opengloss-rs` binary, README, and the export script. Upload the
tarball (`dist/opengloss-rs-<version>-<target>.tar.zst`) and its `.sha256` to a GitHub Release.
See [RELEASING.md](RELEASING.md) for a full walkthrough, including recommended checks before
publishing.
