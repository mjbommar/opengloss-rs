## Highlights
- **Smarter type-ahead**: prefix lookups now fall back to substring matches whenever you finish typing a word that has no direct prefix hits (e.g., “object” now surfaces “3d object”). This keeps the suggestions list populated even for multi-word entries.
- Added regression tests to ensure the fallback stays wired up.

## Packaging
- `scripts/package_release.sh v0.3.9`
- Upload `dist/opengloss-rs-v0.3.9-<target>.tar.zst` and its `.sha256` to the GitHub release.
