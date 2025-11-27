## Highlights
- **SEO fix for JSON-LD**: Script blocks now embed raw JSON instead of HTML-escaped strings, restoring proper Schema.org ingestion by crawlers.
- **Shared footer links**: Every page footer now points to the ArXiv paper, GitHub repo, and Hugging Face dataset so readers can cite, fork, or download the corpus quickly.

## Packaging
- `scripts/package_release.sh v0.4.2`
- Upload `dist/opengloss-rs-v0.4.2-<target>.tar.zst` and its `.sha256` to the GitHub release.
