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

## Running the CLI

The `cli` feature is enabled by default. Build normally (`cargo run -- <args>`) to use the command
line tooling, or disable it with `cargo build --no-default-features` when you only need the library.
Every subcommand accepts the global `--json` flag to emit machine-readable output instead of the
default column/table views.

### Quick start

```bash
# Discover all commands/flags
cargo run -- --help

# Show help for a specific subcommand
cargo run -- lexeme search --help
```

### Subcommands at a glance

| Command | Description | Example |
| --- | --- | --- |
| `lexeme get <word>...` | Exact lookup of one or more surface forms, returning lexeme IDs. | `cargo run -- lexeme get "general relativity" tensor` |
| `lexeme prefix <prefix>` | Prefix lookup backed by the compiled FST. | `cargo run -- lexeme prefix geo --limit 5` |
| `lexeme search <pattern>` | Substring or fuzzy search across words, definitions, synonyms, entry text, and encyclopedia content. | `cargo run -- lexeme search biodegradable --mode fuzzy --field word --field definitions` |
| `lexeme show <query>` | Render the full entry (definitions, senses, encyclopedia text, etymology). | `cargo run -- lexeme show 3d` / `cargo run -- --json lexeme show 42 --by-id` |
| `lexeme graph <query>` | Traverse relation edges (synonym/antonym/hypernym/hyponym) and dump them as a tree, JSON, or GraphViz DOT. | `cargo run -- lexeme graph algorithm --depth 2 --format tree` |

### Lookup, prefix, and substring helpers

```bash
# Exact lookups
cargo run -- lexeme get "a b c labels" "machine learning"

# Prefix expansion (limit defaults to 10)
cargo run -- lexeme prefix bio --limit 20

# Substring search when you do not need fuzziness
cargo run -- lexeme search algorithm --mode substring --limit 15
```

### Weighted fuzzy search

`lexeme search` defaults to a RapidFuzz-backed ranking that blends matches across the word,
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

Run `cargo run -- lexeme search --help` for the full list of knobs (field list, per-field weights,
min score, cache diagnostics). The JSON mode is convenient when calling the binary from scripts:

```bash
cargo run -- --json lexeme get sphere
# [
#   {
#     "lexeme_id": 12345,
#     "word": "sphere"
#   }
# ]
```

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

### Launching searches programmatically

All lookup-oriented subcommands respect `--json`, so you can integrate them into scripts without
parsing text tables. Combine that with `--limit`, `--mode`, and the weight flags to shape the API
surface you need.

## Web server & HTTP API

Enable the `web` feature to expose an Axum/Tokio HTTP server alongside the CLI:

```bash
cargo run --no-default-features --features "cli web" -- serve \
  --addr 127.0.0.1:8090 \
  --theme tailwind \
  --openapi true
```

`serve` options:

- `--addr <ip:port>`: socket address to bind (defaults to `127.0.0.1:8080`).
- `--theme tailwind|bootstrap`: pick the CSS framework for the HTML explorer.
- `--openapi`: reserved for future OpenAPI docs; keep it enabled for forwards compatibility.
- `--public-base <url>`: public origin used for canonical links, JSON-LD metadata, and sitemap
  entries (defaults to `http://<addr>`).

Logging is wired up via `tracing_subscriber` with an `info`-level default. Override it with
`RUST_LOG`, e.g. `RUST_LOG=debug,tower_http=trace cargo run --no-default-features --features "cli web" -- serve`.

### Prefix index & sitemap

- `GET /index?letters=<n>&prefix=<abc>`: interactive lexeme index that groups every entry by
  configurable prefix depth (1–4 letters) and shows the first 750 matches for the selected prefix.
- `GET /sitemap.xml`: auto-generated sitemap with canonical URLs for every lexeme plus the home and
  index pages. Point search consoles at this endpoint once you host the explorer publicly.

### HTML explorer

- `GET /`: landing page with quick links.
- `GET /lexeme?word=<word>` or `?id=<lexeme_id>`: rendered entry view.
- `GET /search?q=<query>&mode=fuzzy|substring&limit=<n>`: table of search hits with deep links to
  `/lexeme`.
- `GET /index`: browsable prefix index described above.

Lexeme pages now open with an overview strip that surfaces how many senses exist, the
part-of-speech distribution, and whether an encyclopedia article is available, plus quick navigation
chips to jump straight to the definitions, senses, or encyclopedia section.

Entry bodies, aggregated definitions, and encyclopedia articles are authored in Markdown inside the
dataset; the web explorer renders them to HTML via [`markdown-rs`](https://github.com/wooorm/markdown-rs)
so headings, emphasis, tables, and lists survive intact.

### JSON API

| Method | Path | Query parameters | Description |
| --- | --- | --- | --- |
| `GET` | `/api/lexeme` | `word=<string>` **or** `id=<u32>` | Returns the full `LexemePayload` (entry metadata, senses, relations, encyclopedia text). |
| `GET` | `/api/search` | `q=<string>&mode=fuzzy|substring&limit=1..100` | Returns `results[]` with lexeme IDs, forms, and optional scores (for fuzzy mode). |
| `GET` | `/healthz` | *(none)* | Simple readiness probe emitting `{ "status": "ok" }`. |

Sample interactions:

```bash
# Fetch an entry
curl 'http://127.0.0.1:8090/api/lexeme?word=dog' | jq '.word, .lexeme_id'

# Run a fuzzy search and capture scores
curl 'http://127.0.0.1:8090/api/search?q=gravitation&mode=fuzzy&limit=5' | jq '.results[] | {word, score}'

# HTML endpoints for browsers
open http://127.0.0.1:8090/lexeme?word=algorithm
```

The Axum router also exposes `/lexeme` and `/search` in HTML, so you can point browsers at the same
instance you use for API calls. Responses are automatically gzip/zstd-compressed via
`tower-http::CompressionLayer`.

### Structured data

Every lexeme page now embeds JSON-LD that describes the word as a Schema.org `DefinedTerm`, complete
with breadcrumbs, synonyms, and encyclopedia text when available. Search pages emit
`SearchResultsPage` metadata, and the home page advertises a `SearchAction` so Google and friends can
deep-link into the explorer. Set `--public-base` (or `PUBLIC_BASE` once you add env plumbing) to the
canonical origin before hosting the service so the sitemap and JSON-LD point at the correct domain.

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

Use the helper script to produce GitHub-friendly artifacts (tarball + SHA256). It compiles the binary
with both `cli` and `web` features so the packaged executable always exposes the `serve` subcommand.

```bash
scripts/package_release.sh v0.1.0
ls dist/
```

The archive currently includes the `opengloss-rs` binary, README, and the export script. Upload the
tarball (`dist/opengloss-rs-<version>-<target>.tar.zst`) and its `.sha256` to a GitHub Release.
Set `CARGO_FEATURES` if you need a different feature set (e.g., `CARGO_FEATURES="cli"`).
See [RELEASING.md](RELEASING.md) for a full walkthrough, including recommended checks before
publishing.
