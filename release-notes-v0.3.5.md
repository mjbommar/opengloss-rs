## Highlights
- **Cleaner lexeme layout**: removed the redundant “Entry Text” block from the web UI so users jump straight into definitions, senses, or the encyclopedia article. Navigation chips and overview cards now focus on those core sections.
- **Markdown safety**: updated regression tests to ensure rich markup still renders inside the remaining sections.

## Packaging
- `scripts/package_release.sh v0.3.5`
- Upload `dist/opengloss-rs-v0.3.5-<target>.tar.zst` and its `.sha256` to the GitHub release.
