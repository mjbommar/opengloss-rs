## Highlights
- **Navigation everywhere**: lexeme/search/index pages now share a consistent “Home + quick search” toolbar backed by the `/api/typeahead` endpoint, so you can jump between words without scrolling back.
- **Friendly homepage copy**: the landing page speaks to everyday readers, while technical notes (CLI, API, trie) are tucked into a brief footnote.
- **Sitemap + type-ahead polish**: a sitemap index fans out to per-letter feeds, and type-ahead chips only link to existing lexemes (missing entries show disabled pills).

## Packaging
- `scripts/package_release.sh v0.3.8`
- Upload `dist/opengloss-rs-v0.3.8-<target>.tar.zst` and its `.sha256` to the GitHub release.
