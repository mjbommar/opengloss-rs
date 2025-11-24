#!/usr/bin/env python3
"""
Export lexeme metadata from the Hugging Face opengloss dictionary dataset.

This script writes a tab-delimited file at data/lexemes.tsv containing:
lexeme_id,word,dataset_row_id,dataset_row_index
"""

from __future__ import annotations

import csv
import json
import sys
from pathlib import Path

from datasets import load_dataset


DATASET_NAME = "mjbommar/opengloss-dictionary"
OUTPUT_FILE = Path(__file__).resolve().parents[1] / "data" / "lexemes.tsv"
ENTRIES_FILE = Path(__file__).resolve().parents[1] / "data" / "entries.jsonl"


def main() -> None:
    print(f"Loading dataset {DATASET_NAME!r} ...", file=sys.stderr)
    ds = load_dataset(DATASET_NAME, split="train")

    OUTPUT_FILE.parent.mkdir(parents=True, exist_ok=True)

    seen_words: dict[str, int] = {}
    rows: list[tuple[int, str, str, int]] = []

    with OUTPUT_FILE.open("w", encoding="utf-8", newline="") as tsv, ENTRIES_FILE.open(
        "w", encoding="utf-8"
    ) as jsonl:
        writer = csv.writer(tsv, delimiter="\t")
        writer.writerow(["lexeme_id", "word", "entry_id", "dataset_row_index"])

        for row_idx, row in enumerate(ds):
            word = (row.get("word") or "").strip()
            entry_id = row.get("id") or ""
            if not word or word in seen_words:
                continue

            lexeme_id = len(rows)
            seen_words[word] = lexeme_id
            rows.append((lexeme_id, word, entry_id, row_idx))
            writer.writerow([lexeme_id, word, entry_id, row_idx])

            entry = {
                "lexeme_id": lexeme_id,
                "entry_id": entry_id,
                "word": word,
                "text": row.get("text"),
                "is_stopword": bool(row.get("is_stopword", False)),
                "stopword_reason": row.get("stopword_reason"),
                "parts_of_speech": row.get("parts_of_speech") or [],
                "senses": _extract_senses(row.get("senses") or []),
                "has_etymology": bool(row.get("has_etymology", False)),
                "etymology_summary": row.get("etymology_summary"),
                "etymology_cognates": row.get("etymology_cognates") or [],
                "has_encyclopedia": bool(row.get("has_encyclopedia", False)),
                "encyclopedia_entry": row.get("encyclopedia_entry"),
                "all_definitions": row.get("all_definitions") or [],
                "all_synonyms": row.get("all_synonyms") or [],
                "all_antonyms": row.get("all_antonyms") or [],
                "all_hypernyms": row.get("all_hypernyms") or [],
                "all_hyponyms": row.get("all_hyponyms") or [],
                "all_collocations": row.get("all_collocations") or [],
                "all_inflections": row.get("all_inflections") or [],
                "all_derivations": row.get("all_derivations") or [],
                "all_examples": row.get("all_examples") or [],
            }
            jsonl.write(json.dumps(entry, ensure_ascii=False) + "\n")

    print(
        f"Wrote {len(rows)} lexemes to {OUTPUT_FILE.relative_to(Path.cwd())}",
        file=sys.stderr,
    )
    print(
        f"Wrote detailed entries to {ENTRIES_FILE.relative_to(Path.cwd())}",
        file=sys.stderr,
    )


def _extract_senses(senses: list[dict]) -> list[dict]:
    slim = []
    for sense in senses:
        slim.append(
            {
                "part_of_speech": sense.get("part_of_speech"),
                "sense_index": sense.get("sense_index"),
                "definition": sense.get("definition"),
                "synonyms": sense.get("synonyms") or [],
                "antonyms": sense.get("antonyms") or [],
                "hypernyms": sense.get("hypernyms") or [],
                "hyponyms": sense.get("hyponyms") or [],
                "examples": sense.get("examples") or [],
            }
        )
    return slim


if __name__ == "__main__":
    main()
