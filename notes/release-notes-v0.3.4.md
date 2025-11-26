## Highlights
- **Trust raw HTML blocks**: lexeme entry text, definition aggregates, and encyclopedia sections now render embedded `<h1>`, `<p>`, `<iframe>`, etc. from the dataset without escaping. The Markdown renderer enables `allow_dangerous_html`/`allow_dangerous_protocol` and disables the GFM tag filter so trusted content flows through, plus new unit tests guard the behavior.

## Packaging
- `scripts/package_release.sh v0.3.4`
- Upload `dist/opengloss-rs-v0.3.4-<target>.tar.zst` and its `.sha256` to the GitHub release.
