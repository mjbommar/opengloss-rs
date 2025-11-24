use std::cmp::Ordering;
use std::collections::HashMap;
use std::env;
use std::error::Error;
use std::fs::{self, File};
use std::io::{BufRead, BufReader, BufWriter};
use std::path::{Path, PathBuf};

use fst::MapBuilder;
use rkyv::{rancor::Error as RkyvError, to_bytes};
use serde::Deserialize;
use zstd::bulk::compress as zstd_compress;

#[path = "src/data.rs"]
mod data_model;
use data_model::{
    CompressedTextStore, DataStore, EntryRecord, PackedStrings, Range, SenseRecord, StringId,
    TextId,
};

const STORE_ENTRY_TEXT: bool = true;
const STORE_ENCYCLOPEDIA_TEXT: bool = true;
// Use moderate defaults so rebuilds remain fast; individual texts can still be recompressed later
// or tuned via these constants if we need smaller artifacts.
const ARCHIVE_COMPRESSION_LEVEL: i32 = 4;
const LONG_TEXT_COMPRESSION_LEVEL: i32 = 5;
const STRING_COMPRESSION_LEVEL: i32 = 5;

fn main() -> Result<(), Box<dyn Error>> {
    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR")?);
    let out_dir = PathBuf::from(env::var("OUT_DIR")?);

    let lexeme_rows = load_lexemes(&manifest_dir)?;
    build_fst(&lexeme_rows, &out_dir)?;
    let lexeme_lookup: HashMap<String, u32> = lexeme_rows
        .iter()
        .map(|(word, id)| (word.clone(), *id))
        .collect();
    build_data_store(&manifest_dir, &out_dir, lexeme_rows.len(), lexeme_lookup)?;

    Ok(())
}

fn load_lexemes(manifest_dir: &Path) -> Result<Vec<(String, u32)>, Box<dyn Error>> {
    let lexeme_file = manifest_dir.join("data/lexemes.tsv");
    println!("cargo:rerun-if-changed={}", lexeme_file.display());
    if !lexeme_file.exists() {
        panic!(
            "Missing {}. Run `uv run --with datasets python scripts/export_lexemes.py`.",
            lexeme_file.display()
        );
    }
    let file = BufReader::new(File::open(&lexeme_file)?);
    let mut rows: Vec<(String, u32)> = Vec::new();
    for (idx, line_res) in file.lines().enumerate() {
        let line = line_res?;
        if idx == 0 && line.starts_with("lexeme_id") {
            continue;
        }
        if line.trim().is_empty() {
            continue;
        }
        let mut parts = line.splitn(4, '\t');
        let id_str = parts
            .next()
            .ok_or_else(|| format!("Missing lexeme_id in line {idx}"))?;
        let word = parts
            .next()
            .ok_or_else(|| format!("Missing word in line {idx}"))?
            .to_owned();
        if word.is_empty() {
            continue;
        }
        let lexeme_id: u32 = id_str.parse()?;
        rows.push((word, lexeme_id));
    }
    Ok(rows)
}

fn build_fst(rows: &[(String, u32)], out_dir: &Path) -> Result<(), Box<dyn Error>> {
    let mut sorted = rows.to_vec();
    sorted.sort_by(|a, b| match a.0.as_str().cmp(b.0.as_str()) {
        Ordering::Equal => a.1.cmp(&b.1),
        other => other,
    });
    for pair in sorted.windows(2) {
        if pair[0].0 == pair[1].0 {
            panic!("Duplicate lexeme {:?}", pair[0].0);
        }
    }

    let fst_path = out_dir.join("lexemes.fst");
    let writer = BufWriter::new(File::create(&fst_path)?);
    let mut builder = MapBuilder::new(writer)?;
    for (word, id) in &sorted {
        builder.insert(word, u64::from(*id))?;
    }
    builder.finish()?;
    println!("cargo:rustc-env=LEXEME_FST={}", fst_path.display());
    Ok(())
}

fn build_data_store(
    manifest_dir: &Path,
    out_dir: &Path,
    expected_entries: usize,
    lexeme_lookup: HashMap<String, u32>,
) -> Result<(), Box<dyn Error>> {
    let entries_path = manifest_dir.join("data/entries.jsonl");
    println!("cargo:rerun-if-changed={}", entries_path.display());
    if !entries_path.exists() {
        panic!(
            "Missing {}. Run `uv run --with datasets python scripts/export_lexemes.py`.",
            entries_path.display()
        );
    }

    let file = BufReader::new(File::open(&entries_path)?);
    let mut builder = DataBuilder::new(expected_entries, lexeme_lookup);
    for (line_idx, line_res) in file.lines().enumerate() {
        let line = line_res?;
        if line.trim().is_empty() {
            continue;
        }
        let entry: EntryJson = serde_json::from_str(&line)
            .map_err(|err| format!("Failed to parse JSON line {}: {err}", line_idx + 1))?;
        builder.add_entry(entry)?;
    }

    let store = builder.finish(expected_entries)?;
    let bytes = to_bytes::<RkyvError>(&store)
        .map_err(|err| format!("Failed to serialize data store: {err}"))?
        .into_vec();
    let compressed = zstd_compress(&bytes, ARCHIVE_COMPRESSION_LEVEL)
        .expect("compress archived data store with zstd");

    let data_path = out_dir.join("opengloss_data.rkyv");
    fs::write(&data_path, compressed)?;
    println!("cargo:rustc-env=OPENGLOSS_DATA={}", data_path.display());
    Ok(())
}

#[derive(Debug, Deserialize)]
struct EntryJson {
    lexeme_id: u32,
    entry_id: String,
    word: String,
    text: Option<String>,
    #[serde(default)]
    is_stopword: bool,
    stopword_reason: Option<String>,
    #[serde(default)]
    parts_of_speech: Vec<String>,
    #[serde(default)]
    senses: Vec<SenseJson>,
    #[serde(default)]
    has_etymology: bool,
    etymology_summary: Option<String>,
    #[serde(default)]
    etymology_cognates: Vec<String>,
    #[serde(default)]
    has_encyclopedia: bool,
    encyclopedia_entry: Option<String>,
    #[serde(default)]
    all_definitions: Vec<String>,
    #[serde(default)]
    all_synonyms: Vec<String>,
    #[serde(default)]
    all_antonyms: Vec<String>,
    #[serde(default)]
    all_hypernyms: Vec<String>,
    #[serde(default)]
    all_hyponyms: Vec<String>,
    #[serde(default)]
    all_collocations: Vec<String>,
    #[serde(default)]
    all_inflections: Vec<String>,
    #[serde(default)]
    all_derivations: Vec<String>,
    #[serde(default)]
    all_examples: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct SenseJson {
    part_of_speech: Option<String>,
    sense_index: Option<i32>,
    definition: Option<String>,
    #[serde(default)]
    synonyms: Vec<String>,
    #[serde(default)]
    antonyms: Vec<String>,
    #[serde(default)]
    hypernyms: Vec<String>,
    #[serde(default)]
    hyponyms: Vec<String>,
    #[serde(default)]
    examples: Vec<String>,
}

struct DataBuilder {
    strings: StringTable,
    long_texts: CompressedTextTable,
    entries: Vec<EntryRecord>,
    entry_parts_of_speech: Vec<StringId>,
    senses: Vec<SenseRecord>,
    sense_synonyms: Vec<StringId>,
    sense_antonyms: Vec<StringId>,
    sense_hypernyms: Vec<StringId>,
    sense_hyponyms: Vec<StringId>,
    sense_examples: Vec<StringId>,
    entry_all_definitions: Vec<StringId>,
    entry_all_synonyms: Vec<StringId>,
    entry_all_antonyms: Vec<StringId>,
    entry_all_hypernyms: Vec<StringId>,
    entry_all_hyponyms: Vec<StringId>,
    entry_all_collocations: Vec<StringId>,
    entry_all_inflections: Vec<StringId>,
    entry_all_derivations: Vec<StringId>,
    entry_all_examples: Vec<StringId>,
    entry_etymology_cognates: Vec<StringId>,
    entry_synonym_neighbors: Vec<u32>,
    entry_antonym_neighbors: Vec<u32>,
    entry_hypernym_neighbors: Vec<u32>,
    entry_hyponym_neighbors: Vec<u32>,
    lexeme_lookup: HashMap<String, u32>,
}

impl DataBuilder {
    fn new(expected_entries: usize, lexeme_lookup: HashMap<String, u32>) -> Self {
        Self {
            strings: StringTable::default(),
            long_texts: CompressedTextTable::default(),
            entries: Vec::with_capacity(expected_entries),
            entry_parts_of_speech: Vec::new(),
            senses: Vec::new(),
            sense_synonyms: Vec::new(),
            sense_antonyms: Vec::new(),
            sense_hypernyms: Vec::new(),
            sense_hyponyms: Vec::new(),
            sense_examples: Vec::new(),
            entry_all_definitions: Vec::new(),
            entry_all_synonyms: Vec::new(),
            entry_all_antonyms: Vec::new(),
            entry_all_hypernyms: Vec::new(),
            entry_all_hyponyms: Vec::new(),
            entry_all_collocations: Vec::new(),
            entry_all_inflections: Vec::new(),
            entry_all_derivations: Vec::new(),
            entry_all_examples: Vec::new(),
            entry_etymology_cognates: Vec::new(),
            entry_synonym_neighbors: Vec::new(),
            entry_antonym_neighbors: Vec::new(),
            entry_hypernym_neighbors: Vec::new(),
            entry_hyponym_neighbors: Vec::new(),
            lexeme_lookup,
        }
    }

    fn add_entry(&mut self, entry: EntryJson) -> Result<(), Box<dyn Error>> {
        let expected_id = self.entries.len() as u32;
        if entry.lexeme_id != expected_id {
            return Err(format!(
                "Entries must be ordered by lexeme_id (expected {}, got {})",
                expected_id, entry.lexeme_id
            )
            .into());
        }

        let word_id = self.strings.intern_owned(entry.word);
        let entry_id = self.strings.intern_owned(entry.entry_id);
        let text_id = if STORE_ENTRY_TEXT {
            entry.text.map(|t| self.long_texts.intern_owned(t))
        } else {
            None
        };
        let stopword_reason = self.strings.intern_option(entry.stopword_reason);
        let etymology_summary = self.strings.intern_option(entry.etymology_summary);
        let encyclopedia_entry = if STORE_ENCYCLOPEDIA_TEXT {
            entry
                .encyclopedia_entry
                .map(|value| self.long_texts.intern_owned(value))
        } else {
            None
        };

        let parts_of_speech = push_strings(
            &mut self.strings,
            &mut self.entry_parts_of_speech,
            entry.parts_of_speech.into_iter(),
        );
        let senses_range = self.push_senses(entry.lexeme_id, entry.senses);
        let etymology_cognates = push_strings(
            &mut self.strings,
            &mut self.entry_etymology_cognates,
            entry.etymology_cognates.into_iter(),
        );
        let synonym_neighbors = push_neighbor_refs(
            &self.lexeme_lookup,
            &mut self.entry_synonym_neighbors,
            entry.all_synonyms.iter(),
        );
        let antonym_neighbors = push_neighbor_refs(
            &self.lexeme_lookup,
            &mut self.entry_antonym_neighbors,
            entry.all_antonyms.iter(),
        );
        let hypernym_neighbors = push_neighbor_refs(
            &self.lexeme_lookup,
            &mut self.entry_hypernym_neighbors,
            entry.all_hypernyms.iter(),
        );
        let hyponym_neighbors = push_neighbor_refs(
            &self.lexeme_lookup,
            &mut self.entry_hyponym_neighbors,
            entry.all_hyponyms.iter(),
        );

        let all_definitions = push_strings(
            &mut self.strings,
            &mut self.entry_all_definitions,
            entry.all_definitions.into_iter(),
        );
        let all_synonyms = push_strings(
            &mut self.strings,
            &mut self.entry_all_synonyms,
            entry.all_synonyms.into_iter(),
        );
        let all_antonyms = push_strings(
            &mut self.strings,
            &mut self.entry_all_antonyms,
            entry.all_antonyms.into_iter(),
        );
        let all_hypernyms = push_strings(
            &mut self.strings,
            &mut self.entry_all_hypernyms,
            entry.all_hypernyms.into_iter(),
        );
        let all_hyponyms = push_strings(
            &mut self.strings,
            &mut self.entry_all_hyponyms,
            entry.all_hyponyms.into_iter(),
        );
        let all_collocations = push_strings(
            &mut self.strings,
            &mut self.entry_all_collocations,
            entry.all_collocations.into_iter(),
        );
        let all_inflections = push_strings(
            &mut self.strings,
            &mut self.entry_all_inflections,
            entry.all_inflections.into_iter(),
        );
        let all_derivations = push_strings(
            &mut self.strings,
            &mut self.entry_all_derivations,
            entry.all_derivations.into_iter(),
        );
        let all_examples = push_strings(
            &mut self.strings,
            &mut self.entry_all_examples,
            entry.all_examples.into_iter(),
        );

        self.entries.push(EntryRecord {
            lexeme_id: entry.lexeme_id,
            word: word_id,
            entry_id,
            text: text_id,
            is_stopword: entry.is_stopword,
            stopword_reason,
            parts_of_speech,
            senses: senses_range,
            has_etymology: entry.has_etymology,
            etymology_summary,
            etymology_cognates,
            has_encyclopedia: entry.has_encyclopedia && STORE_ENCYCLOPEDIA_TEXT,
            encyclopedia_entry,
            all_definitions,
            all_synonyms,
            all_antonyms,
            all_hypernyms,
            all_hyponyms,
            all_collocations,
            all_inflections,
            all_derivations,
            all_examples,
            synonym_neighbors,
            antonym_neighbors,
            hypernym_neighbors,
            hyponym_neighbors,
        });

        Ok(())
    }

    fn push_senses(&mut self, lexeme_id: u32, senses: Vec<SenseJson>) -> Range {
        let start = self.senses.len() as u32;
        for sense in senses {
            let part_of_speech = sense
                .part_of_speech
                .map(|pos| self.strings.intern_owned(pos));
            let definition = sense.definition.map(|def| self.strings.intern_owned(def));
            let synonyms = push_strings(
                &mut self.strings,
                &mut self.sense_synonyms,
                sense.synonyms.into_iter(),
            );
            let antonyms = push_strings(
                &mut self.strings,
                &mut self.sense_antonyms,
                sense.antonyms.into_iter(),
            );
            let hypernyms = push_strings(
                &mut self.strings,
                &mut self.sense_hypernyms,
                sense.hypernyms.into_iter(),
            );
            let hyponyms = push_strings(
                &mut self.strings,
                &mut self.sense_hyponyms,
                sense.hyponyms.into_iter(),
            );
            let examples = push_strings(
                &mut self.strings,
                &mut self.sense_examples,
                sense.examples.into_iter(),
            );

            self.senses.push(SenseRecord {
                lexeme_id,
                part_of_speech,
                sense_index: sense.sense_index.unwrap_or(-1),
                definition,
                synonyms,
                antonyms,
                hypernyms,
                hyponyms,
                examples,
            });
        }
        Range::new(start, self.senses.len() as u32 - start)
    }

    fn finish(self, expected_entries: usize) -> Result<DataStore, Box<dyn Error>> {
        if self.entries.len() != expected_entries {
            return Err(format!(
                "Expected {expected_entries} entries, but found {}",
                self.entries.len()
            )
            .into());
        }
        Ok(DataStore {
            strings: self.strings.into_store(),
            long_texts: self.long_texts.into_store(),
            entries: self.entries,
            entry_parts_of_speech: self.entry_parts_of_speech,
            senses: self.senses,
            sense_synonyms: self.sense_synonyms,
            sense_antonyms: self.sense_antonyms,
            sense_hypernyms: self.sense_hypernyms,
            sense_hyponyms: self.sense_hyponyms,
            sense_examples: self.sense_examples,
            entry_all_definitions: self.entry_all_definitions,
            entry_all_synonyms: self.entry_all_synonyms,
            entry_all_antonyms: self.entry_all_antonyms,
            entry_all_hypernyms: self.entry_all_hypernyms,
            entry_all_hyponyms: self.entry_all_hyponyms,
            entry_all_collocations: self.entry_all_collocations,
            entry_all_inflections: self.entry_all_inflections,
            entry_all_derivations: self.entry_all_derivations,
            entry_all_examples: self.entry_all_examples,
            entry_etymology_cognates: self.entry_etymology_cognates,
            entry_synonym_neighbors: self.entry_synonym_neighbors,
            entry_antonym_neighbors: self.entry_antonym_neighbors,
            entry_hypernym_neighbors: self.entry_hypernym_neighbors,
            entry_hyponym_neighbors: self.entry_hyponym_neighbors,
        })
    }
}

#[derive(Default)]
struct StringTable {
    map: HashMap<Box<str>, StringId>,
    offsets: Vec<u32>,
    lengths: Vec<u32>,
    data: Vec<u8>,
}

impl StringTable {
    fn intern_owned(&mut self, value: String) -> StringId {
        if let Some(&id) = self.map.get(value.as_str()) {
            return id;
        }
        let id = self.offsets.len() as u32;
        let compressed = zstd_compress(value.as_bytes(), STRING_COMPRESSION_LEVEL)
            .expect("compress short string with zstd");
        self.offsets.push(self.data.len() as u32);
        self.lengths.push(compressed.len() as u32);
        self.data.extend_from_slice(&compressed);
        self.map.insert(value.into_boxed_str(), id);
        id
    }

    fn intern_option(&mut self, value: Option<String>) -> Option<StringId> {
        value.map(|v| self.intern_owned(v))
    }

    fn into_store(self) -> PackedStrings {
        PackedStrings {
            offsets: self.offsets,
            lengths: self.lengths,
            data: self.data,
        }
    }
}

#[derive(Default)]
struct CompressedTextTable {
    map: HashMap<Box<str>, TextId>,
    offsets: Vec<u32>,
    lengths: Vec<u32>,
    data: Vec<u8>,
}

impl CompressedTextTable {
    fn intern_owned(&mut self, value: String) -> TextId {
        if let Some(&id) = self.map.get(value.as_str()) {
            return id;
        }
        let compressed = zstd_compress(value.as_bytes(), LONG_TEXT_COMPRESSION_LEVEL)
            .expect("compress long-form text with zstd");
        let id = self.offsets.len() as u32;
        self.offsets.push(self.data.len() as u32);
        self.lengths.push(compressed.len() as u32);
        self.data.extend_from_slice(&compressed);
        self.map.insert(value.into_boxed_str(), id);
        id
    }

    fn into_store(self) -> CompressedTextStore {
        CompressedTextStore {
            offsets: self.offsets,
            lengths: self.lengths,
            data: self.data,
        }
    }
}

fn push_strings<I>(table: &mut StringTable, target: &mut Vec<StringId>, iter: I) -> Range
where
    I: IntoIterator<Item = String>,
{
    let start = target.len() as u32;
    for value in iter {
        target.push(table.intern_owned(value));
    }
    Range::new(start, target.len() as u32 - start)
}

fn push_neighbor_refs<'a, I>(
    lookup: &HashMap<String, u32>,
    target: &mut Vec<u32>,
    iter: I,
) -> Range
where
    I: IntoIterator<Item = &'a String>,
{
    let start = target.len() as u32;
    for value in iter {
        if let Some(&lex_id) = lookup.get(value.as_str()) {
            target.push(lex_id);
        }
    }
    Range::new(start, target.len() as u32 - start)
}
