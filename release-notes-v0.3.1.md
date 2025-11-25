## Highlights
- **Web-ready binary**: release tarballs are now built with `--no-default-features --features "cli web"`, so the shipped executable always includes the `serve` subcommand and Axum UI/API.
- **Configurable feature builds**: set `CARGO_FEATURES` (e.g., `cli`) before running `scripts/package_release.sh` if you need a smaller CLI-only artifact.
- **Prefix index + SEO tooling**: added `/index` (configurable prefix browser), `/sitemap.xml`, and Schema.org JSON-LD so hosted instances get rich search previews.
- **Lexeme UX polish**: each `/lexeme` page now opens with summary cards (sense count, part-of-speech coverage, encyclopedia availability) and pill navigation links so users can jump directly to the content they care about.
- **Markdown rendering**: entry bodies, encyclopedia articles, and definitions now render via `markdown-rs`, preserving headings, emphasis, and tables from the source dataset.

## Packaging
- `scripts/package_release.sh v0.3.1`
- Upload `dist/opengloss-rs-v0.3.1-<target>.tar.zst` and its `.sha256` to the GitHub release.
