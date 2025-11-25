## Highlights
- **Web explorer (feature-gated)**: new Axum-based server (`cargo run --no-default-features --features "cli web" -- serve ...`) serving HTML + JSON routes for `/`, `/lexeme`, `/search`, `/api/lexeme`, and `/api/search`, complete with Tailwind/Bootstrap themes and Askama templates.
- **Search diagnostics & caching**: CLI `lexeme search` now explains per-field scores (`--explain`), and substring/fuzzy caches are exposed for reuse in the web server.
- **Graph groundwork**: CLI `lexeme graph` now walks synonym/antonym/hypernym/hyponym edges with DOT/JSON/tree output, enabling richer relation tooling.
- **Tests + lint**: added web endpoint tests, tightened Clippy to `-D warnings`, and refreshed build pipeline scripts + release packaging.

## Artifacts
- `opengloss-rs-v0.3.0-x86_64-unknown-linux-gnu.tar.zst`
- `opengloss-rs-v0.3.0-x86_64-unknown-linux-gnu.tar.zst.sha256`

The tarball contains the fully static `opengloss-rs` binary (with the OpenGloss dataset embedded), README, and the dataset export script for reproducibility.
