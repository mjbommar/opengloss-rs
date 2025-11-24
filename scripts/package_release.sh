#!/usr/bin/env bash
# Build and package a release artifact suitable for uploading to GitHub Releases.
set -euo pipefail

root_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$root_dir"

if [[ ! -f "data/entries.jsonl" || ! -f "data/lexemes.tsv" ]]; then
    echo "error: data export not found. Run 'uv run --with datasets python scripts/export_lexemes.py' first." >&2
    exit 1
fi

version="${1:-}"
if [[ -z "$version" ]]; then
    version="$(grep -m1 '^version' Cargo.toml | sed -E 's/.*\"([^\"]+)\".*/\1/')"
    echo "VERSION not provided; defaulting to Cargo.toml version ${version}" >&2
fi

target_triple="${TARGET_TRIPLE:-$(rustc -Vv | awk '/host:/ {print $2}')}"
dist_dir="$root_dir/dist"
staging_dir="$dist_dir/opengloss-rs-${version}-${target_triple}"

rm -rf "$staging_dir"
mkdir -p "$staging_dir"

echo "Building release binary..." >&2
cargo build --release

echo "Collecting artifacts..." >&2
cp target/release/opengloss-rs "$staging_dir/"
cp README.md "$staging_dir/README.md"
cp scripts/export_lexemes.py "$staging_dir/export_lexemes.py"

archive="$dist_dir/opengloss-rs-${version}-${target_triple}.tar.zst"
rm -f "$archive" "$archive.sha256"
echo "Creating archive $archive ..." >&2
tar --use-compress-program="zstd -19 --threads=0" -cf "$archive" -C "$staging_dir" .
sha256sum "$archive" > "$archive.sha256"

echo "Release artifact ready:"
ls -lh "$archive" "$archive.sha256"
