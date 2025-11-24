# Releasing opengloss-rs

These steps describe how to cut a GitHub release that includes the fully baked binary with all
OpenGloss data embedded.

## 1. Prep the workspace

```bash
git status -sb          # ensure clean tree
uv run --with datasets python scripts/export_lexemes.py
cargo fmt
cargo check
cargo test              # currently no tests, but keeps the workflow consistent
cargo bench             # captures fresh Criterion reports
```

Optional sanity checks:

- `target/release/opengloss-rs lexeme show algorithm` to confirm runtime output.
- `/usr/bin/time -v target/release/opengloss-rs lexeme show dog` to note current RSS.

## 2. Package the artifacts

Use the helper script (runs `cargo build --release`, assembles a tarball, emits a checksum):

```bash
scripts/package_release.sh v0.1.0
ls -lh dist/
```

This produces:

- `dist/opengloss-rs-v0.1.0-<target>.tar.zst`
- `dist/opengloss-rs-v0.1.0-<target>.tar.zst.sha256`

Inspect the archive if needed:

```bash
tar -tf dist/opengloss-rs-v0.1.0-*.tar.zst | head
```

## 3. Create the GitHub Release

1. Tag the commit: `git tag v0.1.0 && git push origin v0.1.0`.
2. Create the release via the UI or GitHub CLI. Example using `gh`:
   ```bash
   gh release create v0.1.0 \
     dist/opengloss-rs-v0.1.0-*.tar.zst \
     dist/opengloss-rs-v0.1.0-*.tar.zst.sha256 \
     --notes-file release-notes.md
   ```
3. Mention the benchmark snapshot and RAM footprint in the release notes.

## 4. Post-release

- Update the README/RELEASING notes with any new metrics if they changed substantially.
- Announce the release wherever appropriate (Hugging Face, blog post, etc.).
