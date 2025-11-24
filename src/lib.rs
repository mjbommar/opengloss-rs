mod data;

use data::{
    ArchivedCompressedTextStore, ArchivedDataStore, ArchivedEntryRecord, ArchivedPackedStrings,
    ArchivedRange, ArchivedSenseRecord, ArchivedStringId, ArchivedTextId,
};
use fst::Automaton;
use fst::automaton::Str;
use fst::{IntoStreamer, Map, Streamer};
use once_cell::sync::Lazy;
use rkyv::access_unchecked;
use rkyv::util::AlignedVec;
use std::io::{Cursor, Read};
use std::str;
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

/// Read-only access to the lexeme trie.
pub struct LexemeIndex;

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
}

fn data_store() -> &'static ArchivedDataStore {
    *DATA_STORE
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
        self.strings.get(id)
    }
}

impl ArchivedPackedStrings {
    fn get(&self, id: ArchivedStringId) -> &str {
        let idx = id.to_native() as usize;
        let start = self.offsets.as_slice()[idx].to_native() as usize;
        let len = self.lengths.as_slice()[idx].to_native() as usize;
        let data = self.data.as_slice();
        let bytes = &data[start..start + len];
        str::from_utf8(bytes).expect("stored string data is valid UTF-8")
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
