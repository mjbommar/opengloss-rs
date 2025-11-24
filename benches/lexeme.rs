use criterion::{BenchmarkId, Criterion, black_box, criterion_group, criterion_main};
use opengloss_rs::{LexemeIndex, SearchConfig};
use std::io::{Cursor, Read};
use std::sync::Once;
use zstd::stream::Decoder as ZstdDecoder;

static DATA_BYTES: &[u8] = include_bytes!(env!("OPENGLOSS_DATA"));

fn ensure_loaded() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        // Trigger the lazy data-store initialization once so subsequent benches
        // only measure steady-state query performance.
        let _ = LexemeIndex::entry_by_id(0);
    });
}

fn bench_cold_load(c: &mut Criterion) {
    c.bench_function("cold_load::decompress_blob", |b| {
        b.iter(|| {
            let mut decoder = ZstdDecoder::new(Cursor::new(DATA_BYTES)).expect("cold-load decoder");
            let mut buf = [0u8; 64 * 1024];
            let mut total = 0usize;
            loop {
                let read = decoder.read(&mut buf).expect("stream read");
                if read == 0 {
                    break;
                }
                total += read;
            }
            black_box(total);
        });
    });
}

fn bench_entry_queries(c: &mut Criterion) {
    ensure_loaded();
    const WORDS: &[&str] = &["algorithm", "dog", "school", "science", "mathematics"];
    for &word in WORDS {
        c.bench_with_input(BenchmarkId::new("entry_lookup", word), &word, |b, &word| {
            b.iter(|| {
                let entry = LexemeIndex::entry_by_word(word).expect("entry present");
                black_box(entry.lexeme_id());
                black_box(entry.text());
            });
        });
    }
}

fn bench_prefix_queries(c: &mut Criterion) {
    ensure_loaded();
    const CASES: &[(&str, usize)] = &[("bio", 10), ("geo", 20), ("micro", 25)];
    for &(prefix, limit) in CASES {
        let label = format!("{prefix}_{limit}");
        c.bench_with_input(
            BenchmarkId::new("prefix_lookup", label),
            &(prefix, limit),
            |b, &(prefix, limit)| {
                b.iter(|| {
                    let results = LexemeIndex::prefix(prefix, limit);
                    black_box(results.len());
                });
            },
        );
    }
}

fn bench_substring_search(c: &mut Criterion) {
    ensure_loaded();
    const PATTERNS: &[(&str, usize)] = &[("bio", 25), ("graph", 25), ("tion", 25)];
    for &(pattern, limit) in PATTERNS {
        let id = format!("{pattern}_{limit}");
        c.bench_with_input(
            BenchmarkId::new("search_substring", id),
            &(pattern, limit),
            |b, &(pattern, limit)| {
                b.iter(|| {
                    let hits = LexemeIndex::search_contains(pattern, limit);
                    black_box(hits.len());
                });
            },
        );
    }
}

fn bench_fuzzy_search(c: &mut Criterion) {
    ensure_loaded();
    const QUERIES: &[(&str, usize)] = &[
        ("algorithm", 10),
        ("photosynthesis", 10),
        ("gravitation", 10),
    ];
    let config = SearchConfig::default();
    for &(query, limit) in QUERIES {
        c.bench_with_input(
            BenchmarkId::new("search_fuzzy", query),
            &(query, limit),
            |b, &(query, limit)| {
                b.iter(|| {
                    let hits = LexemeIndex::search_fuzzy(query, &config, limit);
                    black_box(hits.len());
                });
            },
        );
    }
}

criterion_group!(
    benches,
    bench_cold_load,
    bench_entry_queries,
    bench_prefix_queries,
    bench_substring_search,
    bench_fuzzy_search
);
criterion_main!(benches);
