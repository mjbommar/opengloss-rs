## Highlights
- **Web-ready binary**: release tarballs are now built with `--no-default-features --features "cli web"`, so the shipped executable always includes the `serve` subcommand and Axum UI/API.
- **Configurable feature builds**: set `CARGO_FEATURES` (e.g., `cli`) before running `scripts/package_release.sh` if you need a smaller CLI-only artifact.

## Packaging
- `scripts/package_release.sh v0.3.1`
- Upload `dist/opengloss-rs-v0.3.1-<target>.tar.zst` and its `.sha256` to the GitHub release.
