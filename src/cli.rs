use std::cmp;
use std::error::Error;

use clap::{Parser, Subcommand};
use opengloss_rs::LexemeIndex;
use serde_json::json;

#[derive(Parser, Debug)]
#[command(name = "opengloss-rs", about = "Explore OpenGloss data", version)]
pub struct Cli {
    /// Emit JSON instead of human-readable tables.
    #[arg(long, global = true)]
    json: bool,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Operations related to lexemes.
    #[command(subcommand)]
    Lexeme(LexemeCommand),
}

#[derive(Subcommand, Debug)]
enum LexemeCommand {
    /// Look up lexeme IDs for exact word matches.
    Get {
        /// One or more lexeme forms to look up.
        #[arg(required = true)]
        words: Vec<String>,
    },
    /// List lexemes that start with the provided prefix.
    Prefix {
        /// Prefix to search for.
        prefix: String,
        /// Maximum number of matches to return.
        #[arg(short, long, default_value_t = 10)]
        limit: usize,
    },
    /// Show the full entry for a lexeme.
    Show {
        /// Word or lexeme ID to display.
        query: String,
        /// Interpret the query as a lexeme ID instead of a word.
        #[arg(long)]
        by_id: bool,
    },
}

pub fn run() -> Result<(), Box<dyn Error>> {
    let cli = Cli::parse();
    match cli.command {
        Command::Lexeme(LexemeCommand::Get { words }) => handle_get(words, cli.json),
        Command::Lexeme(LexemeCommand::Prefix { prefix, limit }) => {
            handle_prefix(prefix, limit, cli.json)
        }
        Command::Lexeme(LexemeCommand::Show { query, by_id }) => {
            handle_show(query, by_id, cli.json)
        }
    }
}

fn handle_get(words: Vec<String>, as_json: bool) -> Result<(), Box<dyn Error>> {
    let results: Vec<(String, Option<u32>)> = words
        .into_iter()
        .map(|word| {
            let id = LexemeIndex::get(&word);
            (word, id)
        })
        .collect();

    if as_json {
        let payload: Vec<_> = results
            .iter()
            .map(|(word, id)| json!({ "word": word, "lexeme_id": id }))
            .collect();
        println!("{}", serde_json::to_string_pretty(&payload)?);
    } else {
        print_lookup_table(&results);
    }
    Ok(())
}

fn handle_prefix(prefix: String, limit: usize, as_json: bool) -> Result<(), Box<dyn Error>> {
    let limit = cmp::max(1, limit);
    let matches = LexemeIndex::prefix(&prefix, limit);

    if as_json {
        let payload = json!({
            "prefix": prefix,
            "limit": limit,
            "results": matches.iter().map(|(word, id)| {
                json!({"word": word, "lexeme_id": id})
            }).collect::<Vec<_>>(),
        });
        println!("{}", serde_json::to_string_pretty(&payload)?);
    } else {
        print_prefix_table(&prefix, &matches);
    }
    Ok(())
}

fn handle_show(query: String, by_id: bool, as_json: bool) -> Result<(), Box<dyn Error>> {
    let entry = if by_id {
        let id: u32 = query
            .parse()
            .map_err(|_| format!("Failed to parse lexeme ID from {query:?}"))?;
        LexemeIndex::entry_by_id(id).ok_or_else(|| format!("No entry found for lexeme ID {id}"))?
    } else {
        LexemeIndex::entry_by_word(&query)
            .ok_or_else(|| format!("No entry found for word {query:?}"))?
    };

    if as_json {
        let payload = entry_to_json(&entry);
        println!("{}", serde_json::to_string_pretty(&payload)?);
    } else {
        print_entry(&entry);
    }
    Ok(())
}

fn print_lookup_table(rows: &[(String, Option<u32>)]) {
    if rows.is_empty() {
        println!("No words provided.");
        return;
    }
    let width = rows
        .iter()
        .map(|(word, _)| word.len())
        .max()
        .unwrap_or(4)
        .max("WORD".len());
    println!("{:<width$}  {}", "WORD", "LEXEME_ID", width = width);
    println!("{:-<width$}  {}", "", "----------", width = width);
    for (word, id) in rows {
        let value = id
            .map(|v| v.to_string())
            .unwrap_or_else(|| "<missing>".to_string());
        println!("{:<width$}  {}", word, value, width = width);
    }
}

fn print_prefix_table(prefix: &str, rows: &[(String, u32)]) {
    if rows.is_empty() {
        println!("No lexemes matched prefix \"{prefix}\".");
        return;
    }
    let width = rows
        .iter()
        .map(|(word, _)| word.len())
        .max()
        .unwrap_or(prefix.len())
        .max("WORD".len());
    println!("Matches for prefix \"{prefix}\":");
    println!("{:<width$}  {}", "WORD", "LEXEME_ID", width = width);
    println!("{:-<width$}  {}", "", "----------", width = width);
    for (word, id) in rows {
        println!("{:<width$}  {}", word, id, width = width);
    }
}

fn entry_to_json(entry: &opengloss_rs::LexemeEntry<'_>) -> serde_json::Value {
    let senses = entry
        .senses()
        .map(|sense| {
            json!({
                "lexeme_id": sense.lexeme_id(),
                "sense_index": sense.sense_index(),
                "part_of_speech": sense.part_of_speech(),
                "definition": sense.definition(),
                "synonyms": sense.synonyms().collect::<Vec<_>>(),
                "antonyms": sense.antonyms().collect::<Vec<_>>(),
                "hypernyms": sense.hypernyms().collect::<Vec<_>>(),
                "hyponyms": sense.hyponyms().collect::<Vec<_>>(),
                "examples": sense.examples().collect::<Vec<_>>(),
            })
        })
        .collect::<Vec<_>>();

    json!({
        "lexeme_id": entry.lexeme_id(),
        "entry_id": entry.entry_id(),
        "word": entry.word(),
        "is_stopword": entry.is_stopword(),
        "stopword_reason": entry.stopword_reason(),
        "parts_of_speech": entry.parts_of_speech().collect::<Vec<_>>(),
        "text": entry.text(),
        "has_etymology": entry.has_etymology(),
        "etymology_summary": entry.etymology_summary(),
        "etymology_cognates": entry.etymology_cognates().collect::<Vec<_>>(),
        "has_encyclopedia": entry.has_encyclopedia(),
        "encyclopedia_entry": entry.encyclopedia_entry(),
        "all_definitions": entry.all_definitions().collect::<Vec<_>>(),
        "all_synonyms": entry.all_synonyms().collect::<Vec<_>>(),
        "all_antonyms": entry.all_antonyms().collect::<Vec<_>>(),
        "all_hypernyms": entry.all_hypernyms().collect::<Vec<_>>(),
        "all_hyponyms": entry.all_hyponyms().collect::<Vec<_>>(),
        "all_collocations": entry.all_collocations().collect::<Vec<_>>(),
        "all_inflections": entry.all_inflections().collect::<Vec<_>>(),
        "all_derivations": entry.all_derivations().collect::<Vec<_>>(),
        "all_examples": entry.all_examples().collect::<Vec<_>>(),
        "senses": senses,
    })
}

fn print_entry(entry: &opengloss_rs::LexemeEntry<'_>) {
    println!("Lexeme: {} (ID {})", entry.word(), entry.lexeme_id());
    println!("Entry ID: {}", entry.entry_id());
    println!(
        "Stopword: {}{}",
        entry.is_stopword(),
        entry
            .stopword_reason()
            .map(|reason| format!(" ({reason})"))
            .unwrap_or_default()
    );

    let parts: Vec<_> = entry.parts_of_speech().collect();
    if !parts.is_empty() {
        println!("Parts of Speech: {}", parts.join(", "));
    }

    if let Some(text) = entry.text() {
        let (preview, truncated) = truncate_text(&text, 600);
        println!(
            "\nEntry Text ({} chars){}",
            text.len(),
            if truncated { " [truncated]" } else { "" }
        );
        println!("{preview}");
    }

    if let Some(summary) = entry.etymology_summary() {
        println!("\nEtymology Summary:");
        println!("{summary}");
    }
    if let Some(encyclopedia) = entry.encyclopedia_entry() {
        let (preview, truncated) = truncate_text(&encyclopedia, 400);
        println!(
            "\nEncyclopedia Entry ({} chars){}",
            encyclopedia.len(),
            if truncated { " [truncated]" } else { "" }
        );
        println!("{preview}");
    }

    println!("\nSenses:");
    for sense in entry.senses() {
        let label = sense
            .part_of_speech()
            .map(|pos| pos.to_string())
            .unwrap_or_else(|| "unknown".to_string());
        let definition = sense.definition().unwrap_or("<definition unavailable>");
        println!("- [{} #{}] {}", label, sense.sense_index(), definition);

        if let Some(synonyms) = format_list(sense.synonyms().collect(), 6) {
            println!("    Synonyms: {synonyms}");
        }
        if let Some(antonyms) = format_list(sense.antonyms().collect(), 6) {
            println!("    Antonyms: {antonyms}");
        }
        if let Some(examples) = format_list(sense.examples().collect(), 3) {
            println!("    Examples: {examples}");
        }
    }
}

fn truncate_text(text: &str, limit: usize) -> (&str, bool) {
    if text.len() <= limit {
        (text, false)
    } else {
        let mut end = limit;
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        (&text[..end], true)
    }
}

fn format_list(items: Vec<&str>, limit: usize) -> Option<String> {
    if items.is_empty() {
        return None;
    }
    let truncated = items.len() > limit;
    let display = if truncated {
        &items[..limit]
    } else {
        &items[..]
    };
    let mut text = display.join(", ");
    if truncated {
        text.push_str(", …");
    }
    Some(text)
}
