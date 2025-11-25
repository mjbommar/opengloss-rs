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
cargo run -- lexeme search biodegradable --mode fuzzy --field word --field definitions
cargo run -- lexeme search "general relativity" --explain
cargo run -- --json lexeme prefix bio
cargo run -- lexeme show 3d
cargo run -- --json lexeme show 42 --by-id
cargo run -- lexeme graph algorithm --depth 2 --format tree
cargo run -- --json lexeme graph dog --format json

# Launch the web explorer (feature-gated)
cargo run --no-default-features --features "cli web" -- serve --addr 127.0.0.1:8090
```

With the `web` feature enabled, the binary exposes:

- `GET /healthz` – readiness probe.
- `GET /api/lexeme?word=dog` or `?id=123` – JSON payload for a lexeme.
- `GET /api/search?q=dog&mode=fuzzy` – JSON search results (substring or fuzzy).
- `GET /lexeme?word=dog` & `GET /search?q=dog` – server-rendered HTML using Tailwind or Bootstrap, selectable via `--theme`.

`web-openapi` will later surface OpenAPI docs and Swagger UI once those routes are implemented.

### Weighted search

`lexeme search` defaults to a fuzzy RapidFuzz-backed ranking that blends matches across the word,
definitions, synonyms, entry text, and encyclopedia content. You can toggle fields or adjust their
weights directly:

```bash
# Focus on encyclopedia matches
cargo run -- lexeme search "gravitation" --field encyclopedia --weight-encyclopedia 3.5
# Fall back to substring-only matching across lexeme forms
cargo run -- lexeme search bio --mode substring
# Capture per-field contributions and cache stats
cargo run -- lexeme search tensor --explain --limit 5
```

Use `cargo run -- lexeme search --help` to see all knobs (field list, per-field weights, min score).

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
- `LexemeIndex` in `src/lib.rs` exposes exact-match (`get`), prefix (`prefix`), substring
  (`search_contains`), and weighted fuzzy search (`search_fuzzy`) helpers, plus entry resolution
  helpers (`entry_by_word`, `entry_by_id`), graph traversal (`traverse_graph`), and search
  diagnostics (`explain_search`/`search_fuzzy_with_stats`).
- All short strings (lexemes, relation labels, etc.) are stored as lazily decompressed,
  Zstd-compressed blobs inside the packed string arena; the first access inflates them into a cache,
  trimming the initial
  RSS while keeping hot strings accessible as `&'static str`.
- Long-form entry text and encyclopedia prose are placed in a compressed chunk store during the
  build step, so the binary still carries the full content while only decompressing paragraphs on
  demand.
- Neighbor relations (synonyms, antonyms, hypernyms, hyponyms) are resolved to lexeme IDs ahead of
  time, enabling fast lookups/graph traversals without repeated string matching.

### Graph traversal & visualization

The `lexeme graph` subcommand walks the synonym/antonym/hypernym/hyponym edges with a configurable
depth, relation filter, and node/edge caps. Output modes:

- `--format tree` (default): indented textual tree rooted at the query lexeme.
- `--format json`: structured payload with nodes/edges for downstream tooling.
- `--format dot`: GraphViz-compatible DOT file. Pipe it into `dot -Tpng` for quick diagrams.

Example:

```bash
cargo run -- lexeme graph "machine learning" --depth 2 --relation synonym --relation hypernym
# or visualize:
cargo run -- lexeme graph "machine learning" --depth 2 --format dot | dot -Tpng -o graph.png
```

## Search scoring

[RapidFuzz](https://docs.rs/rapidfuzz) powers the fuzzy search API (`fuzz::ratio` at present). Each
lexeme accumulates a weighted average across the enabled fields, and the normalized score (0–1) must
clear the `min_score` threshold (default `0.15`) to appear in the results. Setting a field’s weight
to zero effectively disables it, allowing you to experiment with Solr-style relevance tuning directly
from the CLI.

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
