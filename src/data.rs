use rkyv::{Archive, Serialize};

pub type StringId = u32;
#[allow(dead_code)]
pub type ArchivedStringId = <StringId as Archive>::Archived;
pub type TextId = u32;
#[allow(dead_code)]
pub type ArchivedTextId = <TextId as Archive>::Archived;

#[derive(Archive, Serialize, Debug, Clone, Copy)]
pub struct Range {
    pub start: u32,
    pub len: u32,
}

#[allow(dead_code)]
impl Range {
    pub const fn new(start: u32, len: u32) -> Self {
        Self { start, len }
    }
}

#[derive(Archive, Serialize, Debug)]
pub struct EntryRecord {
    pub lexeme_id: u32,
    pub word: StringId,
    pub entry_id: StringId,
    pub text: Option<TextId>,
    pub is_stopword: bool,
    pub stopword_reason: Option<StringId>,
    pub parts_of_speech: Range,
    pub senses: Range,
    pub has_etymology: bool,
    pub etymology_summary: Option<StringId>,
    pub etymology_cognates: Range,
    pub has_encyclopedia: bool,
    pub encyclopedia_entry: Option<TextId>,
    pub all_definitions: Range,
    pub all_synonyms: Range,
    pub all_antonyms: Range,
    pub all_hypernyms: Range,
    pub all_hyponyms: Range,
    pub all_collocations: Range,
    pub all_inflections: Range,
    pub all_derivations: Range,
    pub all_examples: Range,
}

#[derive(Archive, Serialize, Debug)]
pub struct SenseRecord {
    pub lexeme_id: u32,
    pub part_of_speech: Option<StringId>,
    pub sense_index: i32,
    pub definition: Option<StringId>,
    pub synonyms: Range,
    pub antonyms: Range,
    pub hypernyms: Range,
    pub hyponyms: Range,
    pub examples: Range,
}

#[derive(Archive, Serialize, Debug)]
pub struct PackedStrings {
    pub offsets: Vec<u32>,
    pub lengths: Vec<u32>,
    pub data: Vec<u8>,
}

#[derive(Archive, Serialize, Debug)]
pub struct CompressedTextStore {
    pub offsets: Vec<u32>,
    pub lengths: Vec<u32>,
    pub data: Vec<u8>,
}

#[derive(Archive, Serialize, Debug)]
pub struct DataStore {
    pub strings: PackedStrings,
    pub long_texts: CompressedTextStore,
    pub entries: Vec<EntryRecord>,
    pub entry_parts_of_speech: Vec<StringId>,
    pub senses: Vec<SenseRecord>,
    pub sense_synonyms: Vec<StringId>,
    pub sense_antonyms: Vec<StringId>,
    pub sense_hypernyms: Vec<StringId>,
    pub sense_hyponyms: Vec<StringId>,
    pub sense_examples: Vec<StringId>,
    pub entry_all_definitions: Vec<StringId>,
    pub entry_all_synonyms: Vec<StringId>,
    pub entry_all_antonyms: Vec<StringId>,
    pub entry_all_hypernyms: Vec<StringId>,
    pub entry_all_hyponyms: Vec<StringId>,
    pub entry_all_collocations: Vec<StringId>,
    pub entry_all_inflections: Vec<StringId>,
    pub entry_all_derivations: Vec<StringId>,
    pub entry_all_examples: Vec<StringId>,
    pub entry_etymology_cognates: Vec<StringId>,
}
