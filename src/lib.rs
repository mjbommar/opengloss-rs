mod data;

#[cfg(feature = "web")]
pub mod web;

use data::{
    ArchivedCompressedTextStore, ArchivedDataStore, ArchivedEntryRecord, ArchivedPackedStrings,
    ArchivedRange, ArchivedSenseRecord, ArchivedStringId, ArchivedTextId, ArchivedU32,
};
use fst::Automaton;
use fst::automaton::Str;
use fst::{IntoStreamer, Map, Streamer};
use once_cell::sync::Lazy;
use parking_lot::Mutex;
use rapidfuzz::fuzz;
use rayon::prelude::*;
use rkyv::access_unchecked;
use rkyv::util::AlignedVec;
use std::cmp::Ordering;
use std::collections::{BinaryHeap, HashSet, VecDeque};
use std::fmt;
use std::io::{Cursor, Read};
use std::str;
use std::sync::OnceLock;
use zstd::stream::{Decoder as ZstdDecoder, decode_all};

static LEXEME_FST_BYTES: &[u8] = include_bytes!(env!("LEXEME_FST"));
static DATA_BYTES: &[u8] = include_bytes!(env!("OPENGLOSS_DATA"));

static LEXEME_MAP: Lazy<Map<&'static [u8]>> =
    Lazy::new(|| Map::new(LEXEME_FST_BYTES).expect("valid lexeme fst"));
static DATA_SLICE: Lazy<&'static AlignedVec> = Lazy::new(|| {
    let decompressed = decode_all(Cursor::new(DATA_BYTES)).expect("decompress opengloss data");
    let mut aligned = AlignedVec::with_capacity(decompressed.len());
    aligned.extend_from_slice(&decompressed);
    Box::leak(Box::new(aligned))
});
static DATA_STORE: Lazy<&'static ArchivedDataStore> =
    Lazy::new(|| unsafe { access_unchecked::<ArchivedDataStore>(DATA_SLICE.as_slice()) });
static STRING_CACHE: Lazy<Vec<OnceLock<&'static str>>> = Lazy::new(|| {
    let len = data_store().strings.len();
    (0..len).map(|_| OnceLock::new()).collect()
});
#[allow(clippy::type_complexity)]
static SUBSTRING_CACHE: Lazy<Mutex<lru::LruCache<String, Vec<(String, u32)>>>> =
    Lazy::new(|| Mutex::new(lru::LruCache::new(std::num::NonZeroUsize::new(64).unwrap())));
#[allow(clippy::type_complexity)]
static FUZZY_CACHE: Lazy<Mutex<lru::LruCache<(String, SearchConfig, usize), Vec<SearchResult>>>> =
    Lazy::new(|| Mutex::new(lru::LruCache::new(std::num::NonZeroUsize::new(32).unwrap())));

/// Read-only access to the lexeme trie.
pub struct LexemeIndex;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RelationKind {
    Synonym,
    Antonym,
    Hypernym,
    Hyponym,
}

impl RelationKind {
    pub fn label(self) -> &'static str {
        match self {
            RelationKind::Synonym => "synonym",
            RelationKind::Antonym => "antonym",
            RelationKind::Hypernym => "hypernym",
            RelationKind::Hyponym => "hyponym",
        }
    }

    fn all() -> &'static [RelationKind] {
        use RelationKind::*;
        const ALL: [RelationKind; 4] = [Synonym, Antonym, Hypernym, Hyponym];
        &ALL
    }
}

impl fmt::Display for RelationKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.label())
    }
}

#[derive(Debug, Clone)]
pub struct GraphOptions {
    pub max_depth: usize,
    pub max_nodes: usize,
    pub max_edges: usize,
    pub relations: Vec<RelationKind>,
}

impl Default for GraphOptions {
    fn default() -> Self {
        Self {
            max_depth: 2,
            max_nodes: usize::MAX,
            max_edges: usize::MAX,
            relations: Vec::new(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct GraphNode {
    pub lexeme_id: u32,
    pub word: String,
    pub depth: usize,
    pub parent: Option<u32>,
    pub via: Option<RelationKind>,
}

#[derive(Debug, Clone)]
pub struct GraphEdge {
    pub from: u32,
    pub to: u32,
    pub relation: RelationKind,
}

#[derive(Debug, Clone)]
pub struct GraphTraversal {
    pub root: u32,
    pub nodes: Vec<GraphNode>,
    pub edges: Vec<GraphEdge>,
    pub max_depth_reached: usize,
}

impl LexemeIndex {
    /// Returns the lexeme ID for an exact word match.
    pub fn get(word: &str) -> Option<u32> {
        LEXEME_MAP.get(word).map(|value| value as u32)
    }

    /// Returns up to `limit` lexemes that start with the provided prefix.
    pub fn prefix(prefix: &str, limit: usize) -> Vec<(String, u32)> {
        let automaton = Str::new(prefix).starts_with();
        let mut stream = LEXEME_MAP.search(automaton).into_stream();
        let mut results = Vec::new();
        while let Some((key, value)) = stream.next() {
            let word = String::from_utf8(key.to_vec()).expect("stored lexeme is valid UTF-8");
            results.push((word, value as u32));
            if results.len() >= limit {
                break;
            }
        }
        results
    }

    /// Performs a substring search over all lexemes.
    pub fn search_contains(pattern: &str, limit: usize) -> Vec<(String, u32)> {
        if pattern.is_empty() {
            return Vec::new();
        }
        {
            let mut cache = SUBSTRING_CACHE.lock();
            if let Some(hit) = cache.get(pattern) {
                return hit.iter().take(limit).cloned().collect();
            }
        }

        let mut stream = LEXEME_MAP.stream();
        let mut results = Vec::new();
        while let Some((key, value)) = stream.next() {
            if let Ok(word) = std::str::from_utf8(key)
                && word.contains(pattern)
            {
                results.push((word.to_owned(), value as u32));
                if results.len() >= limit {
                    break;
                }
            }
        }

        let mut cache = SUBSTRING_CACHE.lock();
        cache.put(pattern.to_owned(), results.clone());
        results
    }

    /// Performs a weighted fuzzy search over all entries.
    pub fn search_fuzzy(query: &str, config: &SearchConfig, limit: usize) -> Vec<SearchResult> {
        Self::search_fuzzy_with_stats(query, config, limit).results
    }

    /// Performs a weighted fuzzy search and returns cache insights.
    pub fn search_fuzzy_with_stats(
        query: &str,
        config: &SearchConfig,
        limit: usize,
    ) -> SearchSummary {
        if query.trim().is_empty() || config.total_weight() <= 0.0 {
            return SearchSummary {
                results: Vec::new(),
                cache_hit: false,
            };
        }
        let store = data_store();
        let limit = limit.max(1);
        let config = config.clone();
        let key = (query.to_owned(), config.clone(), limit);
        {
            let mut cache = FUZZY_CACHE.lock();
            if let Some(hit) = cache.get(&key) {
                return SearchSummary {
                    results: hit.clone(),
                    cache_hit: true,
                };
            }
        }

        let heap = store
            .entries
            .par_iter()
            .filter_map(|entry| {
                score_entry(query, store, entry, &config).and_then(|score| {
                    if score < config.min_score {
                        None
                    } else {
                        let word = store.string_from_archived(entry.word).to_owned();
                        Some(RankedResult {
                            score,
                            lexeme_id: entry.lexeme_id.to_native(),
                            word,
                        })
                    }
                })
            })
            .fold(BinaryHeap::new, |mut heap, item| {
                push_ranked(&mut heap, item, limit);
                heap
            })
            .reduce(BinaryHeap::new, |mut left, mut right| {
                if left.len() < right.len() {
                    std::mem::swap(&mut left, &mut right);
                }
                for item in right.drain() {
                    push_ranked(&mut left, item, limit);
                }
                left
            });

        let results = drain_heap(heap);
        let mut cache = FUZZY_CACHE.lock();
        cache.put(key, results.clone());
        SearchSummary {
            results,
            cache_hit: false,
        }
    }

    /// Returns the lexeme entry for the given ID, if available.
    pub fn entry_by_id(lexeme_id: u32) -> Option<LexemeEntry<'static>> {
        data_store()
            .entries
            .get(lexeme_id as usize)
            .map(|entry| LexemeEntry {
                store: data_store(),
                entry,
            })
    }

    /// Resolves a word to its entry.
    pub fn entry_by_word(word: &str) -> Option<LexemeEntry<'static>> {
        Self::get(word).and_then(Self::entry_by_id)
    }

    /// Produces detailed score breakdowns for a set of results.
    pub fn explain_search(
        query: &str,
        config: &SearchConfig,
        results: &[SearchResult],
    ) -> Vec<SearchBreakdown> {
        let store = data_store();
        results
            .iter()
            .filter_map(|row| {
                store
                    .entries
                    .get(row.lexeme_id as usize)
                    .and_then(|entry| explain_entry(query, store, entry, config))
            })
            .collect()
    }

    /// Traverses the neighbor graph with a depth-limited BFS.
    pub fn traverse_graph(lexeme_id: u32, options: &GraphOptions) -> Option<GraphTraversal> {
        let opts = GraphOptions {
            max_depth: options.max_depth,
            max_nodes: if options.max_nodes == 0 {
                usize::MAX
            } else {
                options.max_nodes
            },
            max_edges: if options.max_edges == 0 {
                usize::MAX
            } else {
                options.max_edges
            },
            relations: if options.relations.is_empty() {
                RelationKind::all().to_vec()
            } else {
                options.relations.clone()
            },
        };
        let _ = Self::entry_by_id(lexeme_id)?;

        let mut visited: HashSet<u32> = HashSet::new();
        visited.insert(lexeme_id);
        let mut queue = VecDeque::new();
        queue.push_back((lexeme_id, 0usize, None, None));

        let mut nodes = Vec::new();
        let mut edges = Vec::new();
        let mut max_depth_reached = 0usize;

        while let Some((current_id, depth, parent, via)) = queue.pop_front() {
            if nodes.len() >= opts.max_nodes {
                break;
            }
            let entry = match Self::entry_by_id(current_id) {
                Some(e) => e,
                None => continue,
            };
            let word = entry.word().to_string();
            nodes.push(GraphNode {
                lexeme_id: current_id,
                word,
                depth,
                parent,
                via,
            });
            max_depth_reached = max_depth_reached.max(depth);

            if depth >= opts.max_depth {
                continue;
            }
            for relation in &opts.relations {
                let neighbors = entry.neighbor_ids(*relation);
                for neighbor_id in neighbors {
                    if visited.contains(&neighbor_id) {
                        continue;
                    }
                    if edges.len() >= opts.max_edges {
                        break;
                    }
                    if nodes.len() + queue.len() >= opts.max_nodes {
                        continue;
                    }
                    edges.push(GraphEdge {
                        from: current_id,
                        to: neighbor_id,
                        relation: *relation,
                    });
                    visited.insert(neighbor_id);
                    queue.push_back((neighbor_id, depth + 1, Some(current_id), Some(*relation)));
                }
                if edges.len() >= opts.max_edges {
                    break;
                }
            }
        }

        Some(GraphTraversal {
            root: lexeme_id,
            nodes,
            edges,
            max_depth_reached,
        })
    }
}

fn data_store() -> &'static ArchivedDataStore {
    *DATA_STORE
}

fn string_cache() -> &'static [OnceLock<&'static str>] {
    STRING_CACHE.as_slice()
}

pub struct LexemeEntry<'a> {
    store: &'a ArchivedDataStore,
    entry: &'a ArchivedEntryRecord,
}

impl<'a> LexemeEntry<'a> {
    pub fn lexeme_id(&self) -> u32 {
        self.entry.lexeme_id.to_native()
    }

    pub fn word(&self) -> &'a str {
        self.store.string_from_archived(self.entry.word)
    }

    pub fn text(&self) -> Option<String> {
        self.entry
            .text
            .as_ref()
            .map(|id| self.store.decompress_long_text(*id))
    }

    pub fn entry_id(&self) -> &'a str {
        self.store.string_from_archived(self.entry.entry_id)
    }

    pub fn is_stopword(&self) -> bool {
        self.entry.is_stopword
    }

    pub fn stopword_reason(&self) -> Option<&'a str> {
        self.entry
            .stopword_reason
            .as_ref()
            .map(|id| self.store.string_from_archived(*id))
    }

    pub fn parts_of_speech(&'a self) -> impl Iterator<Item = &'a str> + 'a {
        string_iter(
            self.store,
            &self.entry.parts_of_speech,
            self.store.entry_parts_of_speech.as_slice(),
        )
    }

    pub fn senses(&'a self) -> SenseIter<'a> {
        let slice = range_slice(self.store.senses.as_slice(), &self.entry.senses);
        SenseIter {
            store: self.store,
            senses: slice,
            index: 0,
        }
    }

    pub fn etymology_summary(&self) -> Option<&'a str> {
        self.entry
            .etymology_summary
            .as_ref()
            .map(|id| self.store.string_from_archived(*id))
    }

    pub fn has_etymology(&self) -> bool {
        self.entry.has_etymology
    }

    pub fn etymology_cognates(&'a self) -> impl Iterator<Item = &'a str> + 'a {
        string_iter(
            self.store,
            &self.entry.etymology_cognates,
            self.store.entry_etymology_cognates.as_slice(),
        )
    }

    pub fn encyclopedia_entry(&self) -> Option<String> {
        self.entry
            .encyclopedia_entry
            .as_ref()
            .map(|id| self.store.decompress_long_text(*id))
    }

    pub fn has_encyclopedia(&self) -> bool {
        self.entry.has_encyclopedia
    }

    pub fn all_definitions(&'a self) -> impl Iterator<Item = &'a str> + 'a {
        string_iter(
            self.store,
            &self.entry.all_definitions,
            self.store.entry_all_definitions.as_slice(),
        )
    }

    pub fn all_synonyms(&'a self) -> impl Iterator<Item = &'a str> + 'a {
        string_iter(
            self.store,
            &self.entry.all_synonyms,
            self.store.entry_all_synonyms.as_slice(),
        )
    }

    pub fn all_antonyms(&'a self) -> impl Iterator<Item = &'a str> + 'a {
        string_iter(
            self.store,
            &self.entry.all_antonyms,
            self.store.entry_all_antonyms.as_slice(),
        )
    }

    pub fn all_hypernyms(&'a self) -> impl Iterator<Item = &'a str> + 'a {
        string_iter(
            self.store,
            &self.entry.all_hypernyms,
            self.store.entry_all_hypernyms.as_slice(),
        )
    }

    pub fn all_hyponyms(&'a self) -> impl Iterator<Item = &'a str> + 'a {
        string_iter(
            self.store,
            &self.entry.all_hyponyms,
            self.store.entry_all_hyponyms.as_slice(),
        )
    }

    pub fn all_collocations(&'a self) -> impl Iterator<Item = &'a str> + 'a {
        string_iter(
            self.store,
            &self.entry.all_collocations,
            self.store.entry_all_collocations.as_slice(),
        )
    }

    pub fn all_inflections(&'a self) -> impl Iterator<Item = &'a str> + 'a {
        string_iter(
            self.store,
            &self.entry.all_inflections,
            self.store.entry_all_inflections.as_slice(),
        )
    }

    pub fn all_derivations(&'a self) -> impl Iterator<Item = &'a str> + 'a {
        string_iter(
            self.store,
            &self.entry.all_derivations,
            self.store.entry_all_derivations.as_slice(),
        )
    }

    pub fn all_examples(&'a self) -> impl Iterator<Item = &'a str> + 'a {
        string_iter(
            self.store,
            &self.entry.all_examples,
            self.store.entry_all_examples.as_slice(),
        )
    }

    pub fn synonym_neighbor_ids(&'a self) -> impl Iterator<Item = u32> + 'a {
        id_iter(
            &self.entry.synonym_neighbors,
            self.store.entry_synonym_neighbors.as_slice(),
        )
    }

    pub fn antonym_neighbor_ids(&'a self) -> impl Iterator<Item = u32> + 'a {
        id_iter(
            &self.entry.antonym_neighbors,
            self.store.entry_antonym_neighbors.as_slice(),
        )
    }

    pub fn hypernym_neighbor_ids(&'a self) -> impl Iterator<Item = u32> + 'a {
        id_iter(
            &self.entry.hypernym_neighbors,
            self.store.entry_hypernym_neighbors.as_slice(),
        )
    }

    pub fn hyponym_neighbor_ids(&'a self) -> impl Iterator<Item = u32> + 'a {
        id_iter(
            &self.entry.hyponym_neighbors,
            self.store.entry_hyponym_neighbors.as_slice(),
        )
    }

    pub fn neighbor_ids(&'a self, relation: RelationKind) -> Vec<u32> {
        match relation {
            RelationKind::Synonym => self.synonym_neighbor_ids().collect(),
            RelationKind::Antonym => self.antonym_neighbor_ids().collect(),
            RelationKind::Hypernym => self.hypernym_neighbor_ids().collect(),
            RelationKind::Hyponym => self.hyponym_neighbor_ids().collect(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct SearchConfig {
    pub weight_word: f32,
    pub weight_definitions: f32,
    pub weight_synonyms: f32,
    pub weight_text: f32,
    pub weight_encyclopedia: f32,
    pub min_score: f32,
}

impl Default for SearchConfig {
    fn default() -> Self {
        Self {
            weight_word: 3.0,
            weight_definitions: 2.0,
            weight_synonyms: 1.0,
            weight_text: 1.5,
            weight_encyclopedia: 1.5,
            min_score: 0.15,
        }
    }
}

impl SearchConfig {
    pub fn total_weight(&self) -> f32 {
        self.weight_word
            + self.weight_definitions
            + self.weight_synonyms
            + self.weight_text
            + self.weight_encyclopedia
    }
}

impl PartialEq for SearchConfig {
    fn eq(&self, other: &Self) -> bool {
        self.weight_word.to_bits() == other.weight_word.to_bits()
            && self.weight_definitions.to_bits() == other.weight_definitions.to_bits()
            && self.weight_synonyms.to_bits() == other.weight_synonyms.to_bits()
            && self.weight_text.to_bits() == other.weight_text.to_bits()
            && self.weight_encyclopedia.to_bits() == other.weight_encyclopedia.to_bits()
            && self.min_score.to_bits() == other.min_score.to_bits()
    }
}

impl Eq for SearchConfig {}

impl std::hash::Hash for SearchConfig {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.weight_word.to_bits().hash(state);
        self.weight_definitions.to_bits().hash(state);
        self.weight_synonyms.to_bits().hash(state);
        self.weight_text.to_bits().hash(state);
        self.weight_encyclopedia.to_bits().hash(state);
        self.min_score.to_bits().hash(state);
    }
}

#[derive(Debug, Clone)]
pub struct SearchResult {
    pub lexeme_id: u32,
    pub word: String,
    pub score: f32,
}

#[derive(Debug, Clone)]
pub struct SearchSummary {
    pub results: Vec<SearchResult>,
    pub cache_hit: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FieldKind {
    Word,
    Definitions,
    Synonyms,
    Text,
    Encyclopedia,
}

impl FieldKind {
    fn label(self) -> &'static str {
        match self {
            FieldKind::Word => "word",
            FieldKind::Definitions => "definitions",
            FieldKind::Synonyms => "synonyms",
            FieldKind::Text => "text",
            FieldKind::Encyclopedia => "encyclopedia",
        }
    }
}

impl fmt::Display for FieldKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.label())
    }
}

#[derive(Debug, Clone)]
pub struct FieldContribution {
    pub field: FieldKind,
    pub score: f32,
    pub weight: f32,
    pub sample: Option<String>,
}

#[derive(Debug, Clone)]
pub struct SearchBreakdown {
    pub lexeme_id: u32,
    pub word: String,
    pub total_score: f32,
    pub fields: Vec<FieldContribution>,
}

pub struct SenseIter<'a> {
    store: &'a ArchivedDataStore,
    senses: &'a [ArchivedSenseRecord],
    index: usize,
}

impl<'a> Iterator for SenseIter<'a> {
    type Item = SenseRef<'a>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.index >= self.senses.len() {
            return None;
        }
        let sense = &self.senses[self.index];
        self.index += 1;
        Some(SenseRef {
            store: self.store,
            sense,
        })
    }
}

pub struct SenseRef<'a> {
    store: &'a ArchivedDataStore,
    sense: &'a ArchivedSenseRecord,
}

impl<'a> SenseRef<'a> {
    pub fn lexeme_id(&self) -> u32 {
        self.sense.lexeme_id.to_native()
    }

    pub fn part_of_speech(&self) -> Option<&'a str> {
        self.sense
            .part_of_speech
            .as_ref()
            .map(|id| self.store.string_from_archived(*id))
    }

    pub fn definition(&self) -> Option<&'a str> {
        self.sense
            .definition
            .as_ref()
            .map(|id| self.store.string_from_archived(*id))
    }

    pub fn sense_index(&self) -> i32 {
        self.sense.sense_index.to_native()
    }

    pub fn synonyms(&'a self) -> impl Iterator<Item = &'a str> + 'a {
        string_iter(
            self.store,
            &self.sense.synonyms,
            self.store.sense_synonyms.as_slice(),
        )
    }

    pub fn antonyms(&'a self) -> impl Iterator<Item = &'a str> + 'a {
        string_iter(
            self.store,
            &self.sense.antonyms,
            self.store.sense_antonyms.as_slice(),
        )
    }

    pub fn hypernyms(&'a self) -> impl Iterator<Item = &'a str> + 'a {
        string_iter(
            self.store,
            &self.sense.hypernyms,
            self.store.sense_hypernyms.as_slice(),
        )
    }

    pub fn hyponyms(&'a self) -> impl Iterator<Item = &'a str> + 'a {
        string_iter(
            self.store,
            &self.sense.hyponyms,
            self.store.sense_hyponyms.as_slice(),
        )
    }

    pub fn examples(&'a self) -> impl Iterator<Item = &'a str> + 'a {
        string_iter(
            self.store,
            &self.sense.examples,
            self.store.sense_examples.as_slice(),
        )
    }
}

fn string_iter<'a>(
    store: &'a ArchivedDataStore,
    range: &'a ArchivedRange,
    bucket: &'a [ArchivedStringId],
) -> impl Iterator<Item = &'a str> + 'a {
    let slice = range_slice(bucket, range);
    slice.iter().map(move |id| store.string_from_archived(*id))
}

fn id_iter<'a>(
    range: &'a ArchivedRange,
    bucket: &'a [ArchivedU32],
) -> impl Iterator<Item = u32> + 'a {
    let slice = range_slice(bucket, range);
    slice.iter().map(|id| id.to_native())
}

fn range_slice<'a, T>(data: &'a [T], range: &'a ArchivedRange) -> &'a [T] {
    let start = range.start.to_native() as usize;
    let len = range.len.to_native() as usize;
    &data[start..start + len]
}

trait StoreStrings {
    fn string_from_archived(&self, id: ArchivedStringId) -> &str;
}

impl StoreStrings for ArchivedDataStore {
    fn string_from_archived(&self, id: ArchivedStringId) -> &str {
        let idx = id.to_native() as usize;
        string_cache()[idx].get_or_init(|| {
            let owned = self.strings.decompress(idx);
            Box::leak(owned.into_boxed_str())
        })
    }
}

impl ArchivedPackedStrings {
    fn len(&self) -> usize {
        self.offsets.as_slice().len()
    }

    fn compressed_slice(&self, idx: usize) -> &[u8] {
        let start = self.offsets.as_slice()[idx].to_native() as usize;
        let len = self.lengths.as_slice()[idx].to_native() as usize;
        let data = self.data.as_slice();
        &data[start..start + len]
    }

    fn decompress(&self, idx: usize) -> String {
        let bytes = self.compressed_slice(idx);
        let decoded = decode_all(Cursor::new(bytes)).expect("string chunk decompresses");
        String::from_utf8(decoded).expect("string chunk valid UTF-8")
    }
}

impl ArchivedCompressedTextStore {
    fn decompress(&self, id: ArchivedTextId) -> String {
        let idx = id.to_native() as usize;
        let start = self.offsets.as_slice()[idx].to_native() as usize;
        let len = self.lengths.as_slice()[idx].to_native() as usize;
        let data = self.data.as_slice();
        let bytes = &data[start..start + len];
        let mut decoder = ZstdDecoder::new(Cursor::new(bytes)).expect("long text chunk decoder");
        let mut output = Vec::new();
        decoder
            .read_to_end(&mut output)
            .expect("long text chunk decompresses");
        String::from_utf8(output).expect("long text chunk is valid UTF-8")
    }
}

impl ArchivedDataStore {
    fn decompress_long_text(&self, id: ArchivedTextId) -> String {
        self.long_texts.decompress(id)
    }
}

#[derive(Clone)]
struct RankedResult {
    score: f32,
    lexeme_id: u32,
    word: String,
}

impl Eq for RankedResult {}

impl PartialEq for RankedResult {
    fn eq(&self, other: &Self) -> bool {
        self.score.eq(&other.score)
    }
}

impl Ord for RankedResult {
    fn cmp(&self, other: &Self) -> Ordering {
        self.score
            .partial_cmp(&other.score)
            .unwrap_or(Ordering::Equal)
            .reverse()
            .then_with(|| self.lexeme_id.cmp(&other.lexeme_id).reverse())
    }
}

impl PartialOrd for RankedResult {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

fn push_ranked(heap: &mut BinaryHeap<RankedResult>, item: RankedResult, limit: usize) {
    if heap.len() < limit {
        heap.push(item);
    } else if let Some(mut peek) = heap.peek_mut()
        && item.score > peek.score
    {
        *peek = item;
    }
}

fn drain_heap(mut heap: BinaryHeap<RankedResult>) -> Vec<SearchResult> {
    let mut out = Vec::with_capacity(heap.len());
    while let Some(item) = heap.pop() {
        out.push(SearchResult {
            lexeme_id: item.lexeme_id,
            word: item.word,
            score: item.score,
        });
    }
    out.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(Ordering::Equal));
    out
}

fn score_entry(
    query: &str,
    store: &ArchivedDataStore,
    entry: &ArchivedEntryRecord,
    config: &SearchConfig,
) -> Option<f32> {
    let mut total_weight = 0.0;
    let mut accum = 0.0;

    if config.weight_word > 0.0 {
        let word = store.string_from_archived(entry.word);
        let s = fuzzy_score(query, word);
        total_weight += config.weight_word;
        accum += s * config.weight_word;
    }

    if config.weight_definitions > 0.0 {
        let s = best_range_score(
            query,
            store,
            &entry.all_definitions,
            store.entry_all_definitions.as_slice(),
        );
        total_weight += config.weight_definitions;
        accum += s * config.weight_definitions;
    }

    if config.weight_synonyms > 0.0 {
        let s = best_range_score(
            query,
            store,
            &entry.all_synonyms,
            store.entry_all_synonyms.as_slice(),
        );
        total_weight += config.weight_synonyms;
        accum += s * config.weight_synonyms;
    }

    if config.weight_text > 0.0
        && let Some(text_id) = entry.text.as_ref()
    {
        let text = store.decompress_long_text(*text_id);
        let s = fuzzy_score(query, &text);
        total_weight += config.weight_text;
        accum += s * config.weight_text;
    }

    if config.weight_encyclopedia > 0.0
        && let Some(enc_id) = entry.encyclopedia_entry.as_ref()
    {
        let text = store.decompress_long_text(*enc_id);
        let s = fuzzy_score(query, &text);
        total_weight += config.weight_encyclopedia;
        accum += s * config.weight_encyclopedia;
    }

    if total_weight > 0.0 {
        Some(accum / total_weight)
    } else {
        None
    }
}

fn best_range_score(
    query: &str,
    store: &ArchivedDataStore,
    range: &ArchivedRange,
    bucket: &[ArchivedStringId],
) -> f32 {
    let mut best = 0.0;
    for value in string_iter(store, range, bucket) {
        let s = fuzzy_score(query, value);
        if s > best {
            best = s;
        }
    }
    best
}

fn fuzzy_score(query: &str, value: &str) -> f32 {
    if value.is_empty() {
        0.0
    } else {
        fuzz::ratio(query.chars(), value.chars()) as f32
    }
}

fn explain_entry(
    query: &str,
    store: &ArchivedDataStore,
    entry: &ArchivedEntryRecord,
    config: &SearchConfig,
) -> Option<SearchBreakdown> {
    let mut total_weight = 0.0;
    let mut accum = 0.0;
    let mut fields = Vec::new();

    if config.weight_word > 0.0 {
        let word = store.string_from_archived(entry.word);
        let score = fuzzy_score(query, word);
        total_weight += config.weight_word;
        accum += score * config.weight_word;
        fields.push(FieldContribution {
            field: FieldKind::Word,
            score,
            weight: config.weight_word,
            sample: Some(word.to_string()),
        });
    }

    if config.weight_definitions > 0.0 {
        let (score, sample) = best_range_score_with_sample(
            query,
            store,
            &entry.all_definitions,
            store.entry_all_definitions.as_slice(),
        );
        total_weight += config.weight_definitions;
        accum += score * config.weight_definitions;
        fields.push(FieldContribution {
            field: FieldKind::Definitions,
            score,
            weight: config.weight_definitions,
            sample,
        });
    }

    if config.weight_synonyms > 0.0 {
        let (score, sample) = best_range_score_with_sample(
            query,
            store,
            &entry.all_synonyms,
            store.entry_all_synonyms.as_slice(),
        );
        total_weight += config.weight_synonyms;
        accum += score * config.weight_synonyms;
        fields.push(FieldContribution {
            field: FieldKind::Synonyms,
            score,
            weight: config.weight_synonyms,
            sample,
        });
    }

    if config.weight_text > 0.0 {
        let text = entry
            .text
            .as_ref()
            .map(|id| store.decompress_long_text(*id));
        if let Some(body) = text {
            let score = fuzzy_score(query, &body);
            total_weight += config.weight_text;
            accum += score * config.weight_text;
            fields.push(FieldContribution {
                field: FieldKind::Text,
                score,
                weight: config.weight_text,
                sample: Some(truncate_sample(&body)),
            });
        }
    }

    if config.weight_encyclopedia > 0.0 {
        let text = entry
            .encyclopedia_entry
            .as_ref()
            .map(|id| store.decompress_long_text(*id));
        if let Some(body) = text {
            let score = fuzzy_score(query, &body);
            total_weight += config.weight_encyclopedia;
            accum += score * config.weight_encyclopedia;
            fields.push(FieldContribution {
                field: FieldKind::Encyclopedia,
                score,
                weight: config.weight_encyclopedia,
                sample: Some(truncate_sample(&body)),
            });
        }
    }

    if total_weight <= 0.0 {
        return None;
    }

    Some(SearchBreakdown {
        lexeme_id: entry.lexeme_id.to_native(),
        word: store.string_from_archived(entry.word).to_string(),
        total_score: accum / total_weight,
        fields,
    })
}

fn best_range_score_with_sample(
    query: &str,
    store: &ArchivedDataStore,
    range: &ArchivedRange,
    bucket: &[ArchivedStringId],
) -> (f32, Option<String>) {
    let mut best = 0.0;
    let mut sample = None;
    for value in string_iter(store, range, bucket) {
        let s = fuzzy_score(query, value);
        if s >= best {
            best = s;
            sample = Some(value.to_string());
        }
    }
    (best, sample.map(|text| truncate_sample(&text)))
}

fn truncate_sample(text: &str) -> String {
    const MAX: usize = 96;
    let mut snippet = String::new();
    for (idx, ch) in text.chars().enumerate() {
        if idx >= MAX {
            snippet.push('…');
            return snippet;
        }
        snippet.push(ch);
    }
    snippet
}
