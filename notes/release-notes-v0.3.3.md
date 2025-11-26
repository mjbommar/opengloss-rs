## Highlights
- **HTML-ready markdown**: rendered entry text, per-sense definitions, definition aggregates, and encyclopedia sections are now injected with `|safe` in the Askama templates so browsers no longer show literal `<h1>`/`<p>` tags. A regression test (`lexeme_markdown_renders_html`) now asserts that escaped entities never reach the page.

## Packaging
- `scripts/package_release.sh v0.3.3`
- Upload `dist/opengloss-rs-v0.3.3-<target>.tar.zst` and its `.sha256` to the GitHub release.
