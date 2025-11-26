use std::cmp;
use std::collections::HashMap;
use std::error::Error;
use std::io;

use atty::Stream;
#[cfg(feature = "web")]
use clap::Args;
use clap::{Parser, Subcommand, ValueEnum};
#[cfg(feature = "web")]
use opengloss_rs::web::{self, WebConfig, WebTheme};
use opengloss_rs::{
    FieldContribution, GraphOptions, GraphTraversal, LexemeIndex, RelationKind, SearchBreakdown,
    SearchSummary,
};
use serde_json::json;
#[cfg(feature = "web")]
use std::net::SocketAddr;
#[cfg(feature = "web")]
use std::sync::OnceLock;
use termimad::{FmtText, MadSkin, terminal_size};
#[cfg(feature = "web")]
use tokio::runtime::Builder as TokioRuntimeBuilder;
#[cfg(feature = "web")]
use tracing_subscriber::{EnvFilter, fmt};

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
    /// Run the embedded web server (requires the `web` feature).
    #[cfg(feature = "web")]
    Serve(ServeArgs),
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
    /// Search for lexemes that contain the provided substring.
    Search {
        /// Query text to search for.
        pattern: String,
        /// Maximum number of matches to return.
        #[arg(short, long, default_value_t = 10)]
        limit: usize,
        /// Search mode (fuzzy uses RapidFuzz scoring; substring scans lexeme forms only).
        #[arg(long, value_enum, default_value_t = SearchMode::Substring)]
        mode: SearchMode,
        /// Fields to search; omit to use defaults (word + definitions).
        #[arg(long = "field", value_enum)]
        fields: Vec<SearchField>,
        /// Weight for matching against the lexeme word itself.
        #[arg(long, default_value_t = 3.0)]
        weight_word: f32,
        /// Weight for definitions and senses.
        #[arg(long, default_value_t = 2.0)]
        weight_definitions: f32,
        /// Weight for synonyms list.
        #[arg(long, default_value_t = 1.0)]
        weight_synonyms: f32,
        /// Weight for the entry text body.
        #[arg(long, default_value_t = 1.5)]
        weight_text: f32,
        /// Weight for the encyclopedia article.
        #[arg(long, default_value_t = 1.5)]
        weight_encyclopedia: f32,
        /// Minimum normalized score (0-1) before emitting a hit.
        #[arg(long, default_value_t = 0.15)]
        min_score: f32,
        /// Print per-field scoring details and cache info.
        #[arg(long)]
        explain: bool,
    },
    /// Show the full entry for a lexeme.
    Show {
        /// Word or lexeme ID to display.
        query: String,
        /// Interpret the query as a lexeme ID instead of a word.
        #[arg(long)]
        by_id: bool,
    },
    /// Traverse neighbor relations (synonym, hypernym, etc.) as a small graph.
    Graph {
        /// Word or lexeme ID to use as the graph root.
        query: String,
        /// Interpret the query as a lexeme ID instead of a word.
        #[arg(long)]
        by_id: bool,
        /// Depth limit for breadth-first traversal (0 = only the root).
        #[arg(short, long, default_value_t = 2)]
        depth: usize,
        /// Relation types to follow; omit to include all.
        #[arg(long = "relation", value_enum)]
        relations: Vec<RelationArg>,
        /// Maximum number of nodes to visit (0 = unlimited).
        #[arg(long, default_value_t = 128)]
        max_nodes: usize,
        /// Maximum number of edges to record (0 = unlimited).
        #[arg(long, default_value_t = 256)]
        max_edges: usize,
        /// Output format: tree (text), json, or dot (GraphViz).
        #[arg(long, value_enum, default_value_t = GraphFormat::Tree)]
        format: GraphFormat,
    },
}

#[cfg(feature = "web")]
#[derive(Args, Debug)]
struct ServeArgs {
    /// Address to bind the HTTP server to.
    #[arg(long, default_value = "127.0.0.1:8080")]
    addr: String,
    /// Public base URL used for canonical links & sitemap entries.
    #[arg(long)]
    public_base: Option<String>,
    /// Render OpenAPI docs & JSON spec.
    #[arg(long, default_value_t = true)]
    openapi: bool,
    /// Front-end theme to load for HTML pages.
    #[arg(long, value_enum, default_value_t = ServeTheme::Tailwind)]
    theme: ServeTheme,
}

#[cfg(feature = "web")]
#[derive(Copy, Clone, Debug, ValueEnum, Eq, PartialEq)]
enum ServeTheme {
    Tailwind,
    Bootstrap,
}

#[cfg(feature = "web")]
impl From<ServeTheme> for WebTheme {
    fn from(value: ServeTheme) -> Self {
        match value {
            ServeTheme::Tailwind => WebTheme::Tailwind,
            ServeTheme::Bootstrap => WebTheme::Bootstrap,
        }
    }
}

pub fn run() -> Result<(), Box<dyn Error>> {
    let cli = Cli::parse();
    match cli.command {
        Command::Lexeme(LexemeCommand::Get { words }) => handle_get(words, cli.json),
        Command::Lexeme(LexemeCommand::Prefix { prefix, limit }) => {
            handle_prefix(prefix, limit, cli.json)
        }
        Command::Lexeme(LexemeCommand::Search {
            pattern,
            limit,
            mode,
            fields,
            weight_word,
            weight_definitions,
            weight_synonyms,
            weight_text,
            weight_encyclopedia,
            min_score,
            explain,
        }) => handle_search(
            pattern,
            limit,
            cli.json,
            mode,
            fields,
            weight_word,
            weight_definitions,
            weight_synonyms,
            weight_text,
            weight_encyclopedia,
            min_score,
            explain,
        ),
        Command::Lexeme(LexemeCommand::Show { query, by_id }) => {
            handle_show(query, by_id, cli.json)
        }
        Command::Lexeme(LexemeCommand::Graph {
            query,
            by_id,
            depth,
            relations,
            max_nodes,
            max_edges,
            format,
        }) => handle_graph(
            query, by_id, depth, relations, max_nodes, max_edges, format, cli.json,
        ),
        #[cfg(feature = "web")]
        Command::Serve(args) => handle_serve(args),
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

#[allow(clippy::too_many_arguments)]
fn handle_search(
    pattern: String,
    limit: usize,
    as_json: bool,
    mode: SearchMode,
    fields: Vec<SearchField>,
    weight_word: f32,
    weight_definitions: f32,
    weight_synonyms: f32,
    weight_text: f32,
    weight_encyclopedia: f32,
    min_score: f32,
    explain: bool,
) -> Result<(), Box<dyn Error>> {
    if pattern.trim().is_empty() {
        return Err("Search pattern cannot be empty".into());
    }
    match mode {
        SearchMode::Substring => {
            if explain {
                return Err("--explain is only available for fuzzy search".into());
            }
            let limit = cmp::max(1, limit);
            let matches = LexemeIndex::search_contains(&pattern, limit);
            if as_json {
                let payload = json!({
                    "mode": "substring",
                    "pattern": pattern,
                    "limit": limit,
                    "results": matches.iter().map(|(word, id)| {
                        json!({"word": word, "lexeme_id": id})
                    }).collect::<Vec<_>>(),
                });
                println!("{}", serde_json::to_string_pretty(&payload)?);
            } else {
                print_search_table(&pattern, &matches);
            }
            Ok(())
        }
        SearchMode::Fuzzy => {
            let selected = if fields.is_empty() {
                vec![SearchField::Word, SearchField::Definitions]
            } else {
                fields
            };
            let mut config = opengloss_rs::SearchConfig {
                weight_word,
                weight_definitions,
                weight_synonyms,
                weight_text,
                weight_encyclopedia,
                min_score,
            };
            apply_field_filter(&mut config, &selected);
            if config.total_weight() <= 0.0 {
                return Err("All search weights are zero; nothing to search".into());
            }
            let limit = cmp::max(1, limit);
            let summary = LexemeIndex::search_fuzzy_with_stats(&pattern, &config, limit);
            let diagnostics = if explain {
                LexemeIndex::explain_search(&pattern, &config, &summary.results)
            } else {
                Vec::new()
            };
            if as_json {
                let payload = json!({
                    "mode": "fuzzy",
                    "pattern": pattern,
                    "limit": limit,
                    "cache_hit": summary.cache_hit,
                    "config": {
                        "weight_word": config.weight_word,
                        "weight_definitions": config.weight_definitions,
                        "weight_synonyms": config.weight_synonyms,
                        "weight_text": config.weight_text,
                        "weight_encyclopedia": config.weight_encyclopedia,
                        "min_score": config.min_score,
                        "fields": selected.iter().map(|f| f.to_string()).collect::<Vec<_>>(),
                    },
                    "results": summary.results.iter().map(|row| {
                        json!({
                            "lexeme_id": row.lexeme_id,
                            "word": row.word,
                            "score": row.score,
                        })
                    }).collect::<Vec<_>>(),
                    "diagnostics": if explain {
                        Some(json!({
                            "cache_hit": summary.cache_hit,
                            "breakdowns": diagnostics.iter().map(breakdown_to_json).collect::<Vec<_>>(),
                        }))
                    } else {
                        None
                    }
                });
                println!("{}", serde_json::to_string_pretty(&payload)?);
            } else {
                print_fuzzy_table(&pattern, &summary.results);
                if explain {
                    print_search_diagnostics(&summary, &diagnostics);
                } else {
                    println!(
                        "\nCache: {}",
                        if summary.cache_hit { "hit" } else { "miss" }
                    );
                }
            }
            Ok(())
        }
    }
}

fn handle_show(query: String, by_id: bool, as_json: bool) -> Result<(), Box<dyn Error>> {
    let entry = resolve_entry(&query, by_id)?;

    if as_json {
        let payload = entry_to_json(&entry);
        println!("{}", serde_json::to_string_pretty(&payload)?);
    } else {
        print_entry(&entry);
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn handle_graph(
    query: String,
    by_id: bool,
    depth: usize,
    relations: Vec<RelationArg>,
    max_nodes: usize,
    max_edges: usize,
    mut format: GraphFormat,
    force_json: bool,
) -> Result<(), Box<dyn Error>> {
    let lexeme_id = resolve_lexeme_id(&query, by_id)?;
    let mut options = GraphOptions {
        max_depth: depth,
        max_nodes,
        max_edges,
        ..GraphOptions::default()
    };
    if !relations.is_empty() {
        options.relations = relations.into_iter().map(RelationArg::into).collect();
    }
    let graph = LexemeIndex::traverse_graph(lexeme_id, &options)
        .ok_or_else(|| user_error(format!("No entry found for {query:?}")))?;
    if force_json {
        format = GraphFormat::Json;
    }
    match format {
        GraphFormat::Tree => print_graph_tree(&graph),
        GraphFormat::Json => {
            let payload = graph_to_json(&graph);
            println!("{}", serde_json::to_string_pretty(&payload)?);
        }
        GraphFormat::Dot => {
            println!("{}", graph_to_dot(&graph));
        }
    }
    Ok(())
}

#[cfg(feature = "web")]
fn handle_serve(args: ServeArgs) -> Result<(), Box<dyn Error>> {
    init_web_logging();
    let addr: SocketAddr = args
        .addr
        .parse()
        .map_err(|_| user_error(format!("Invalid socket address {:?}", args.addr)))?;
    let base_url = normalize_base_url(&addr, args.public_base.as_deref());
    let config = WebConfig {
        addr,
        enable_openapi: args.openapi,
        theme: args.theme.into(),
        base_url,
    };
    tracing::info!(
        %config.addr,
        openapi = config.enable_openapi,
        theme = ?config.theme,
        "Starting OpenGloss web server"
    );
    let runtime = TokioRuntimeBuilder::new_multi_thread()
        .enable_all()
        .build()?;
    runtime.block_on(web::serve(config))?;
    tracing::info!("OpenGloss web server shut down");
    Ok(())
}

#[cfg(feature = "web")]
static WEB_LOGGER: OnceLock<()> = OnceLock::new();

#[cfg(feature = "web")]
fn init_web_logging() {
    WEB_LOGGER.get_or_init(|| {
        let env_filter = EnvFilter::try_from_default_env()
            .unwrap_or_else(|_| EnvFilter::new("info,tower_http=info"));
        let _ = fmt()
            .with_env_filter(env_filter)
            .with_target(false)
            .compact()
            .try_init();
    });
}

#[cfg(feature = "web")]
fn normalize_base_url(addr: &SocketAddr, hint: Option<&str>) -> String {
    let mut candidate = hint
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| format!("http://{}", addr));
    if !candidate.contains("://") {
        candidate = format!("https://{candidate}");
    }
    while candidate.ends_with('/') {
        candidate.pop();
    }
    candidate
}

fn resolve_entry(
    query: &str,
    by_id: bool,
) -> Result<opengloss_rs::LexemeEntry<'static>, Box<dyn Error>> {
    let lexeme_id = resolve_lexeme_id(query, by_id)?;
    LexemeIndex::entry_by_id(lexeme_id)
        .ok_or_else(|| user_error(format!("No entry found for lexeme ID {lexeme_id}")))
}

fn resolve_lexeme_id(query: &str, by_id: bool) -> Result<u32, Box<dyn Error>> {
    if by_id {
        query
            .parse::<u32>()
            .map_err(|_| user_error(format!("Failed to parse lexeme ID from {query:?}")))
    } else {
        LexemeIndex::get(query)
            .ok_or_else(|| user_error(format!("No entry found for word {query:?}")))
    }
}

#[allow(clippy::uninlined_format_args)]
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
    println!("{:<width$}  LEXEME_ID", "WORD", width = width);
    println!("{:-<width$}  ----------", "", width = width);
    for (word, id) in rows {
        let value = id
            .map(|v| v.to_string())
            .unwrap_or_else(|| "<missing>".to_string());
        println!("{word:<width$}  {value}", width = width);
    }
}

#[allow(clippy::uninlined_format_args)]
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
    println!("{:<width$}  LEXEME_ID", "WORD", width = width);
    println!("{:-<width$}  ----------", "", width = width);
    for (word, id) in rows {
        println!("{word:<width$}  {id}", width = width);
    }
}

#[allow(clippy::uninlined_format_args)]
fn print_search_table(pattern: &str, rows: &[(String, u32)]) {
    if rows.is_empty() {
        println!("No lexemes contain \"{pattern}\".");
        return;
    }
    let width = rows
        .iter()
        .map(|(word, _)| word.len())
        .max()
        .unwrap_or(pattern.len())
        .max("WORD".len());
    println!("Matches for substring \"{pattern}\":");
    println!("{:<width$}  LEXEME_ID", "WORD", width = width);
    println!("{:-<width$}  ----------", "", width = width);
    for (word, id) in rows {
        println!("{word:<width$}  {id}", width = width);
    }
}

#[allow(clippy::uninlined_format_args)]
fn print_fuzzy_table(pattern: &str, rows: &[opengloss_rs::SearchResult]) {
    if rows.is_empty() {
        println!("No fuzzy matches found for \"{pattern}\".");
        return;
    }
    let width = rows
        .iter()
        .map(|row| row.word.len())
        .max()
        .unwrap_or(pattern.len())
        .max("WORD".len());
    println!("Fuzzy matches for \"{pattern}\":");
    println!(
        "{:<width$}  {:<8}  LEXEME_ID",
        "WORD",
        "SCORE",
        width = width
    );
    println!(
        "{:-<width$}  {:<8}  ----------",
        "",
        "--------",
        width = width
    );
    for row in rows {
        println!(
            "{word:<width$}  {score:<8.3}  {id}",
            word = row.word,
            score = row.score,
            id = row.lexeme_id,
            width = width
        );
    }
}

fn print_search_diagnostics(summary: &SearchSummary, breakdowns: &[SearchBreakdown]) {
    println!("\nSearch diagnostics:");
    println!(
        "  Cache: {}",
        if summary.cache_hit { "hit" } else { "miss" }
    );
    if breakdowns.is_empty() {
        println!("  No breakdowns available.");
        return;
    }
    for (idx, row) in breakdowns.iter().enumerate() {
        println!(
            "\nResult #{idx}: {} (#{}) — total {:.3}",
            row.word, row.lexeme_id, row.total_score
        );
        if row.fields.is_empty() {
            println!("    (no weighted fields)");
            continue;
        }
        println!("    {:<14} {:>7} {:>7}  SAMPLE", "FIELD", "SCORE", "WEIGHT");
        for field in &row.fields {
            print_field_line(field);
        }
    }
}

fn print_field_line(field: &FieldContribution) {
    let sample = field.sample.as_deref().unwrap_or("-");
    println!(
        "    {:<14} {:>7.3} {:>7.3}  {}",
        field.field, field.score, field.weight, sample
    );
}

fn breakdown_to_json(row: &SearchBreakdown) -> serde_json::Value {
    json!({
        "lexeme_id": row.lexeme_id,
        "word": row.word,
        "total_score": row.total_score,
        "fields": row.fields.iter().map(|field| {
            json!({
                "field": field.field.to_string(),
                "score": field.score,
                "weight": field.weight,
                "sample": field.sample,
            })
        }).collect::<Vec<_>>(),
    })
}

fn print_graph_tree(graph: &GraphTraversal) {
    if graph.nodes.is_empty() {
        println!("No nodes were visited; consider increasing --depth or --max-nodes.");
        return;
    }
    let node_map: HashMap<u32, &opengloss_rs::GraphNode> = graph
        .nodes
        .iter()
        .map(|node| (node.lexeme_id, node))
        .collect();
    let root = match node_map.get(&graph.root) {
        Some(node) => node,
        None => {
            println!(
                "Graph root #{:?} is missing from traversal output.",
                graph.root
            );
            return;
        }
    };
    let mut children: HashMap<u32, Vec<(u32, RelationKind)>> = HashMap::new();
    for edge in &graph.edges {
        children
            .entry(edge.from)
            .or_default()
            .push((edge.to, edge.relation));
    }
    for edges in children.values_mut() {
        edges.sort_by(|(left_id, left_rel), (right_id, right_rel)| {
            let left_word = node_map
                .get(left_id)
                .map(|node| node.word.as_str())
                .unwrap_or("");
            let right_word = node_map
                .get(right_id)
                .map(|node| node.word.as_str())
                .unwrap_or("");
            left_word
                .cmp(right_word)
                .then_with(|| left_rel.label().cmp(right_rel.label()))
        });
    }

    println!(
        "Graph root: {} (#{}), visited {} nodes / {} edges, reached depth {}",
        root.word,
        root.lexeme_id,
        graph.nodes.len(),
        graph.edges.len(),
        graph.max_depth_reached
    );
    if let Some(kids) = children.get(&graph.root) {
        for (child_id, relation) in kids {
            print_graph_branch(*child_id, *relation, 0, &node_map, &children);
        }
    } else {
        println!("  (no neighbors within the current limits)");
    }
}

fn print_graph_branch(
    node_id: u32,
    relation: RelationKind,
    depth: usize,
    nodes: &HashMap<u32, &opengloss_rs::GraphNode>,
    children: &HashMap<u32, Vec<(u32, RelationKind)>>,
) {
    if let Some(node) = nodes.get(&node_id) {
        let padding = "  ".repeat(depth + 1);
        println!(
            "{padding}- [{}] {} (#{} depth {})",
            relation, node.word, node.lexeme_id, node.depth
        );
        if let Some(kids) = children.get(&node_id) {
            for (child_id, rel) in kids {
                print_graph_branch(*child_id, *rel, depth + 1, nodes, children);
            }
        }
    }
}

fn graph_to_json(graph: &GraphTraversal) -> serde_json::Value {
    json!({
        "root": graph.root,
        "max_depth_reached": graph.max_depth_reached,
        "nodes": graph.nodes.iter().map(|node| {
            json!({
                "lexeme_id": node.lexeme_id,
                "word": node.word,
                "depth": node.depth,
                "parent": node.parent,
                "relation": node.via.map(|rel| rel.to_string()),
            })
        }).collect::<Vec<_>>(),
        "edges": graph.edges.iter().map(|edge| {
            json!({
                "from": edge.from,
                "to": edge.to,
                "relation": edge.relation.to_string(),
            })
        }).collect::<Vec<_>>(),
    })
}

fn graph_to_dot(graph: &GraphTraversal) -> String {
    let mut out = String::from("digraph Opengloss {\n  node [shape=box];\n");
    for node in &graph.nodes {
        let label = format!("{} (#{} depth {})", node.word, node.lexeme_id, node.depth);
        out.push_str(&format!(
            "  n{} [label=\"{}\"];\n",
            node.lexeme_id,
            escape_label(&label)
        ));
    }
    for edge in &graph.edges {
        out.push_str(&format!(
            "  n{} -> n{} [label=\"{}\"];",
            edge.from,
            edge.to,
            escape_label(edge.relation.label())
        ));
        out.push('\n');
    }
    out.push_str("}\n");
    out
}

fn escape_label(label: &str) -> String {
    label.replace('"', "\\\"")
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
        render_markdown_block("Entry Text", &text);
    }

    if let Some(summary) = entry.etymology_summary() {
        println!("\nEtymology Summary:");
        println!("{summary}");
    }
    if let Some(encyclopedia) = entry.encyclopedia_entry() {
        render_markdown_block("Encyclopedia Entry", &encyclopedia);
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

    if let Some(neighbors) = format_neighbor_ids(entry.synonym_neighbor_ids(), 8) {
        println!("\nSynonym Links: {neighbors}");
    }
    if let Some(neighbors) = format_neighbor_ids(entry.antonym_neighbor_ids(), 8) {
        println!("Antonym Links: {neighbors}");
    }
    if let Some(neighbors) = format_neighbor_ids(entry.hypernym_neighbor_ids(), 8) {
        println!("Hypernym Links: {neighbors}");
    }
    if let Some(neighbors) = format_neighbor_ids(entry.hyponym_neighbor_ids(), 8) {
        println!("Hyponym Links: {neighbors}");
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

fn format_neighbor_ids<I>(ids: I, limit: usize) -> Option<String>
where
    I: Iterator<Item = u32>,
{
    let mut rendered = Vec::new();
    for (count, id) in ids.enumerate() {
        if count >= limit {
            rendered.push("…".to_string());
            break;
        }
        let label = LexemeIndex::entry_by_id(id)
            .map(|entry| format!("{} (#{id})", entry.word()))
            .unwrap_or_else(|| format!("#{id}"));
        rendered.push(label);
    }
    if rendered.is_empty() {
        None
    } else {
        Some(rendered.join(", "))
    }
}

fn apply_field_filter(config: &mut opengloss_rs::SearchConfig, fields: &[SearchField]) {
    if !fields.contains(&SearchField::Word) {
        config.weight_word = 0.0;
    }
    if !fields.contains(&SearchField::Definitions) {
        config.weight_definitions = 0.0;
    }
    if !fields.contains(&SearchField::Synonyms) {
        config.weight_synonyms = 0.0;
    }
    if !fields.contains(&SearchField::Text) {
        config.weight_text = 0.0;
    }
    if !fields.contains(&SearchField::Encyclopedia) {
        config.weight_encyclopedia = 0.0;
    }
}

fn stdout_is_tty() -> bool {
    atty::is(Stream::Stdout)
}

fn markdown_width() -> usize {
    let (width, _) = terminal_size();
    width.max(60) as usize
}

fn markdown_skin() -> MadSkin {
    MadSkin::default()
}

fn render_markdown_block(title: &str, body: &str) {
    let trimmed = body.trim();
    if trimmed.is_empty() {
        return;
    }
    println!("\n{title}:");
    if stdout_is_tty() {
        let skin = markdown_skin();
        let formatted = FmtText::from(&skin, trimmed, Some(markdown_width()));
        println!("{formatted}");
    } else {
        println!("{trimmed}");
    }
}

fn user_error(msg: impl Into<String>) -> Box<dyn Error> {
    io::Error::new(io::ErrorKind::InvalidInput, msg.into()).into()
}
#[derive(Copy, Clone, Debug, ValueEnum, Eq, PartialEq)]
enum SearchMode {
    Fuzzy,
    Substring,
}

#[derive(Copy, Clone, Debug, ValueEnum, Eq, PartialEq)]
enum GraphFormat {
    Tree,
    Json,
    Dot,
}

#[derive(Copy, Clone, Debug, ValueEnum, Eq, PartialEq, Hash)]
enum RelationArg {
    Synonym,
    Antonym,
    Hypernym,
    Hyponym,
}

impl From<RelationArg> for RelationKind {
    fn from(value: RelationArg) -> Self {
        match value {
            RelationArg::Synonym => RelationKind::Synonym,
            RelationArg::Antonym => RelationKind::Antonym,
            RelationArg::Hypernym => RelationKind::Hypernym,
            RelationArg::Hyponym => RelationKind::Hyponym,
        }
    }
}

#[derive(Copy, Clone, Debug, ValueEnum, Eq, PartialEq, Hash)]
enum SearchField {
    Word,
    Definitions,
    Synonyms,
    Text,
    Encyclopedia,
}

impl std::fmt::Display for SearchField {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let label = match self {
            SearchField::Word => "word",
            SearchField::Definitions => "definitions",
            SearchField::Synonyms => "synonyms",
            SearchField::Text => "text",
            SearchField::Encyclopedia => "encyclopedia",
        };
        write!(f, "{label}")
    }
}
