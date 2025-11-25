use crate::{LexemeEntry, LexemeIndex, SearchConfig};
use askama::Template;
use axum::{
    Json, Router,
    extract::{Query, State},
    http::StatusCode,
    response::{Html, IntoResponse, Response},
    routing::get,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::fmt;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::net::TcpListener;
use tokio::signal;
use tower_http::compression::CompressionLayer;
use tower_http::trace::{DefaultMakeSpan, DefaultOnResponse, TraceLayer};

type SharedState = Arc<AppState>;

#[derive(Clone)]
pub struct AppState {
    pub default_search: SearchConfig,
    pub theme: WebTheme,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, Default)]
pub enum WebTheme {
    #[default]
    Tailwind,
    Bootstrap,
}

impl fmt::Display for WebTheme {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            WebTheme::Tailwind => write!(f, "tailwind"),
            WebTheme::Bootstrap => write!(f, "bootstrap"),
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct Chrome {
    use_tailwind: bool,
    use_bootstrap: bool,
    body_class: &'static str,
    main_class: &'static str,
    card_class: &'static str,
    eyebrow_class: &'static str,
    headline_class: &'static str,
    lede_class: &'static str,
    cta_group_class: &'static str,
    button_class: &'static str,
    table_row_class: &'static str,
}

impl Chrome {
    fn new(theme: WebTheme) -> Self {
        match theme {
            WebTheme::Tailwind => Self {
                use_tailwind: true,
                use_bootstrap: false,
                body_class: "bg-slate-50 text-slate-900",
                main_class: "min-h-screen flex flex-col items-center justify-start py-10 px-4",
                card_class: "max-w-5xl w-full space-y-6",
                eyebrow_class: "uppercase tracking-wide text-sm text-slate-500",
                headline_class: "text-4xl font-extrabold tracking-tight",
                lede_class: "text-lg text-slate-600",
                cta_group_class: "flex flex-wrap gap-3",
                button_class: "inline-flex items-center rounded-md bg-slate-900 px-4 py-2 text-white font-semibold shadow hover:bg-slate-800 transition-colors",
                table_row_class: "border-b border-slate-200",
            },
            WebTheme::Bootstrap => Self {
                use_tailwind: false,
                use_bootstrap: true,
                body_class: "bg-light text-dark",
                main_class: "container py-5",
                card_class: "mx-auto col-lg-10",
                eyebrow_class: "text-uppercase text-muted mb-2",
                headline_class: "display-5 fw-bold",
                lede_class: "lead mb-4",
                cta_group_class: "d-flex flex-wrap gap-3",
                button_class: "btn btn-primary btn-lg px-4 py-2",
                table_row_class: "",
            },
        }
    }
}

#[derive(Clone)]
pub struct WebConfig {
    pub addr: SocketAddr,
    pub enable_openapi: bool,
    pub theme: WebTheme,
}

impl Default for WebConfig {
    fn default() -> Self {
        Self {
            addr: SocketAddr::from(([127, 0, 0, 1], 8080)),
            enable_openapi: true,
            theme: WebTheme::default(),
        }
    }
}

#[derive(Debug)]
pub enum WebError {
    Io(std::io::Error),
}

impl fmt::Display for WebError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            WebError::Io(err) => write!(f, "io error: {err}"),
        }
    }
}

impl std::error::Error for WebError {}

impl From<std::io::Error> for WebError {
    fn from(value: std::io::Error) -> Self {
        WebError::Io(value)
    }
}

pub async fn serve(config: WebConfig) -> Result<(), WebError> {
    let state = Arc::new(AppState {
        default_search: SearchConfig::default(),
        theme: config.theme,
    });
    let router = build_router(state, config.enable_openapi);
    let listener = TcpListener::bind(config.addr).await?;
    axum::serve(listener, router)
        .with_graceful_shutdown(shutdown_signal())
        .await?;
    Ok(())
}

#[derive(Debug)]
struct ApiError {
    status: StatusCode,
    message: String,
}

impl ApiError {
    fn bad_request(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            message: message.into(),
        }
    }

    fn not_found(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::NOT_FOUND,
            message: message.into(),
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let payload = json!({ "error": self.message });
        (self.status, Json(payload)).into_response()
    }
}

fn build_router(state: SharedState, _openapi: bool) -> Router {
    Router::new()
        .route("/", get(home))
        .route("/lexeme", get(lexeme_html))
        .route("/search", get(search_html))
        .route("/api/lexeme", get(api_lexeme))
        .route("/api/search", get(api_search))
        .route("/healthz", get(health))
        .with_state(state)
        .layer(
            TraceLayer::new_for_http()
                .make_span_with(DefaultMakeSpan::new().include_headers(true))
                .on_response(DefaultOnResponse::new().include_headers(true)),
        )
        .layer(CompressionLayer::new())
}

async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = signal::ctrl_c().await;
    };
    #[cfg(unix)]
    let terminate = async {
        use tokio::signal::unix::{SignalKind, signal};
        if let Ok(mut stream) = signal(SignalKind::terminate()) {
            let _ = stream.recv().await;
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }
}

async fn home(State(state): State<SharedState>) -> impl IntoResponse {
    Html(render_home(state.theme))
}

fn render_home(theme: WebTheme) -> String {
    let chrome = Chrome::new(theme);
    let (css_tag, js_tag) = match theme {
        WebTheme::Tailwind => (
            r#"<script src="https://cdn.jsdelivr.net/npm/@tailwindcss/browser@4"></script>"#,
            "",
        ),
        WebTheme::Bootstrap => (
            r#"<link href="https://cdn.jsdelivr.net/npm/bootstrap@5.3.8/dist/css/bootstrap.min.css" rel="stylesheet" integrity="sha384-sRIl4kxILFvY47J16cr9ZwB07vP4J8+LH7qKQnuqkuIAvNWLzeN8tE5YBujZqJLB" crossorigin="anonymous">"#,
            r#"<script src="https://cdn.jsdelivr.net/npm/bootstrap@5.3.8/dist/js/bootstrap.bundle.min.js" integrity="sha384-FKyoEForCGlyvwx9Hj09JcYn3nv7wiPVlz7YYwJrWVcXK/BmnVDxM+D2scQbITxI" crossorigin="anonymous"></script>"#,
        ),
    };
    let title = "OpenGloss • Embedded Dictionary Service";
    let intro = "Browse lexemes, run fuzzy searches, and view encyclopedia entries from the statically compiled OpenGloss dataset.";
    format!(
        r#"<!DOCTYPE html>
<html lang="en">
  <head>
    <meta charset="utf-8" />
    <meta name="viewport" content="width=device-width, initial-scale=1" />
    <title>{title}</title>
    {css_tag}
    {js_tag}
  </head>
  <body class="{body_class}">
    <main class="{main_class}">
      <div class="{card_class} space-y-6">
        <div>
          <p class="{eyebrow_class}">OpenGloss v{version}</p>
          <h1 class="{headline_class}">Explore the full OpenGloss lexicon via blazing-fast Rust endpoints.</h1>
          <p class="{lede_class}">{intro}</p>
        </div>
        <div class="{cta_group}">
          <a href="/lexeme?word=algorithm" class="{button_class}">Example lexeme</a>
          <a href="/search?q=gravitation" class="{button_class}">Run a search</a>
        </div>
      </div>
    </main>
  </body>
</html>"#,
        css_tag = css_tag,
        js_tag = js_tag,
        body_class = chrome.body_class,
        main_class = chrome.main_class,
        card_class = chrome.card_class,
        eyebrow_class = chrome.eyebrow_class,
        headline_class = chrome.headline_class,
        lede_class = chrome.lede_class,
        cta_group = chrome.cta_group_class,
        button_class = chrome.button_class,
        version = env!("CARGO_PKG_VERSION"),
        intro = intro,
    )
}

async fn health() -> impl IntoResponse {
    Json(json!({ "status": "ok", "service": "opengloss-web" }))
}

async fn lexeme_html(
    State(state): State<SharedState>,
    Query(params): Query<LexemeParams>,
) -> impl IntoResponse {
    match entry_from_params(&params) {
        Ok(entry) => {
            let chrome = Chrome::new(state.theme);
            let template = LexemeTemplate {
                chrome,
                payload: &LexemePayload::from_entry(&entry),
            };
            Html(
                template
                    .render()
                    .unwrap_or_else(|err| render_error_page(state.theme, err.to_string())),
            )
        }
        Err(err) => Html(render_error_page(state.theme, err.message)),
    }
}

async fn search_html(
    State(state): State<SharedState>,
    Query(params): Query<SearchParams>,
) -> impl IntoResponse {
    match parse_search_params(&params) {
        Ok((query, limit, mode)) => {
            let payload = match mode {
                SearchModeParam::Fuzzy => {
                    SearchResponsePayload::fuzzy(&query, &state.default_search, limit)
                }
                SearchModeParam::Substring => SearchResponsePayload::substring(&query, limit),
            };
            let chrome = Chrome::new(state.theme);
            let template = SearchTemplate {
                chrome,
                payload: &payload,
            };
            Html(
                template
                    .render()
                    .unwrap_or_else(|err| render_error_page(state.theme, err.to_string())),
            )
        }
        Err(err) => Html(render_error_page(state.theme, err.message)),
    }
}

async fn api_lexeme(Query(params): Query<LexemeParams>) -> Result<Json<LexemePayload>, ApiError> {
    let entry = entry_from_params(&params)?;
    Ok(Json(LexemePayload::from_entry(&entry)))
}

async fn api_search(
    State(state): State<SharedState>,
    Query(params): Query<SearchParams>,
) -> Result<Json<SearchResponsePayload>, ApiError> {
    let (query, limit, mode) = parse_search_params(&params)?;
    let payload = match mode {
        SearchModeParam::Fuzzy => {
            SearchResponsePayload::fuzzy(&query, &state.default_search, limit)
        }
        SearchModeParam::Substring => SearchResponsePayload::substring(&query, limit),
    };
    Ok(Json(payload))
}

#[derive(Debug, Deserialize)]
struct LexemeParams {
    word: Option<String>,
    id: Option<u32>,
}

#[derive(Debug, Deserialize)]
struct SearchParams {
    q: Option<String>,
    limit: Option<usize>,
    mode: Option<SearchModeParam>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct SensePayload {
    lexeme_id: u32,
    sense_index: i32,
    part_of_speech: Option<String>,
    definition: Option<String>,
    synonyms: Vec<String>,
    antonyms: Vec<String>,
    hypernyms: Vec<String>,
    hyponyms: Vec<String>,
    examples: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct LexemePayload {
    lexeme_id: u32,
    entry_id: String,
    word: String,
    is_stopword: bool,
    stopword_reason: Option<String>,
    parts_of_speech: Vec<String>,
    text: Option<String>,
    has_etymology: bool,
    etymology_summary: Option<String>,
    etymology_cognates: Vec<String>,
    has_encyclopedia: bool,
    encyclopedia_entry: Option<String>,
    all_definitions: Vec<String>,
    all_synonyms: Vec<String>,
    all_antonyms: Vec<String>,
    all_hypernyms: Vec<String>,
    all_hyponyms: Vec<String>,
    all_examples: Vec<String>,
    senses: Vec<SensePayload>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct SearchHitPayload {
    lexeme_id: u32,
    word: String,
    score: Option<f32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct SearchResponsePayload {
    query: String,
    mode: SearchModeParam,
    limit: usize,
    results: Vec<SearchHitPayload>,
}

impl LexemePayload {
    fn from_entry(entry: &LexemeEntry<'_>) -> Self {
        let senses = entry
            .senses()
            .map(|sense| SensePayload {
                lexeme_id: sense.lexeme_id(),
                sense_index: sense.sense_index(),
                part_of_speech: sense.part_of_speech().map(|s| s.to_string()),
                definition: sense.definition().map(|s| s.to_string()),
                synonyms: collect_iter(sense.synonyms()),
                antonyms: collect_iter(sense.antonyms()),
                hypernyms: collect_iter(sense.hypernyms()),
                hyponyms: collect_iter(sense.hyponyms()),
                examples: collect_iter(sense.examples()),
            })
            .collect();

        Self {
            lexeme_id: entry.lexeme_id(),
            entry_id: entry.entry_id().to_string(),
            word: entry.word().to_string(),
            is_stopword: entry.is_stopword(),
            stopword_reason: entry.stopword_reason().map(|s| s.to_string()),
            parts_of_speech: collect_iter(entry.parts_of_speech()),
            text: entry.text(),
            has_etymology: entry.has_etymology(),
            etymology_summary: entry.etymology_summary().map(|s| s.to_string()),
            etymology_cognates: collect_iter(entry.etymology_cognates()),
            has_encyclopedia: entry.has_encyclopedia(),
            encyclopedia_entry: entry.encyclopedia_entry(),
            all_definitions: collect_iter(entry.all_definitions()),
            all_synonyms: collect_iter(entry.all_synonyms()),
            all_antonyms: collect_iter(entry.all_antonyms()),
            all_hypernyms: collect_iter(entry.all_hypernyms()),
            all_hyponyms: collect_iter(entry.all_hyponyms()),
            all_examples: collect_iter(entry.all_examples()),
            senses,
        }
    }
}

impl SearchResponsePayload {
    fn substring(query: &str, limit: usize) -> Self {
        let results = LexemeIndex::search_contains(query, limit)
            .into_iter()
            .map(|(word, lexeme_id)| SearchHitPayload {
                lexeme_id,
                word,
                score: None,
            })
            .collect();

        Self {
            query: query.to_string(),
            mode: SearchModeParam::Substring,
            limit,
            results,
        }
    }

    fn fuzzy(query: &str, config: &SearchConfig, limit: usize) -> Self {
        let results = LexemeIndex::search_fuzzy(query, config, limit)
            .into_iter()
            .map(|row| SearchHitPayload {
                lexeme_id: row.lexeme_id,
                word: row.word,
                score: Some(row.score),
            })
            .collect();
        Self {
            query: query.to_string(),
            mode: SearchModeParam::Fuzzy,
            limit,
            results,
        }
    }
}

fn collect_iter<'a, I>(iter: I) -> Vec<String>
where
    I: IntoIterator<Item = &'a str>,
{
    iter.into_iter().map(|s| s.to_string()).collect()
}

fn entry_from_params(params: &LexemeParams) -> Result<LexemeEntry<'static>, ApiError> {
    if let Some(id) = params.id {
        return LexemeIndex::entry_by_id(id)
            .ok_or_else(|| ApiError::not_found(format!("No entry found for lexeme #{id}")));
    }
    if let Some(word) = params
        .word
        .as_ref()
        .map(|w| w.trim())
        .filter(|w| !w.is_empty())
    {
        return LexemeIndex::entry_by_word(word)
            .ok_or_else(|| ApiError::not_found(format!("No entry found for word {word:?}")));
    }
    Err(ApiError::bad_request(
        "Provide either `word` or `id` query parameters.",
    ))
}

fn parse_search_params(
    params: &SearchParams,
) -> Result<(String, usize, SearchModeParam), ApiError> {
    let query = params
        .q
        .as_ref()
        .map(|q| q.trim())
        .filter(|q| !q.is_empty())
        .ok_or_else(|| ApiError::bad_request("Query parameter `q` is required"))?;
    let limit = params.limit.unwrap_or(10).clamp(1, 100);
    let mode = params.mode.unwrap_or_default();
    Ok((query.to_string(), limit, mode))
}

fn render_error_page(theme: WebTheme, message: impl Into<String>) -> String {
    let chrome = Chrome::new(theme);
    let (css_tag, js_tag) = match theme {
        WebTheme::Tailwind => (
            r#"<script src="https://cdn.jsdelivr.net/npm/@tailwindcss/browser@4"></script>"#,
            "",
        ),
        WebTheme::Bootstrap => (
            r#"<link href="https://cdn.jsdelivr.net/npm/bootstrap@5.3.8/dist/css/bootstrap.min.css" rel="stylesheet" integrity="sha384-sRIl4kxILFvY47J16cr9ZwB07vP4J8+LH7qKQnuqkuIAvNWLzeN8tE5YBujZqJLB" crossorigin="anonymous">"#,
            r#"<script src="https://cdn.jsdelivr.net/npm/bootstrap@5.3.8/dist/js/bootstrap.bundle.min.js" integrity="sha384-FKyoEForCGlyvwx9Hj09JcYn3nv7wiPVlz7YYwJrWVcXK/BmnVDxM+D2scQbITxI" crossorigin="anonymous"></script>"#,
        ),
    };
    let message = message.into();
    format!(
        r#"<!DOCTYPE html>
<html lang="en">
  <head>
    <meta charset="utf-8" />
    <meta name="viewport" content="width=device-width, initial-scale=1" />
    <title>OpenGloss • Error</title>
    {css_tag}
    {js_tag}
  </head>
  <body class="{body_class}">
    <main class="{main_class}">
      <div class="{card_class}">
        <h1 class="{headline_class}">Something went wrong</h1>
        <p class="{lede_class}">{message}</p>
        <a href="/" class="{button_class}">Back to home</a>
      </div>
    </main>
  </body>
</html>"#,
        css_tag = css_tag,
        js_tag = js_tag,
        body_class = chrome.body_class,
        main_class = chrome.main_class,
        card_class = chrome.card_class,
        headline_class = chrome.headline_class,
        lede_class = chrome.lede_class,
        button_class = chrome.button_class,
        message = message,
    )
}

#[derive(Template)]
#[template(
    source = r#"<!DOCTYPE html>
<html lang="en">
  <head>
    <meta charset="utf-8" />
    <meta name="viewport" content="width=device-width, initial-scale=1" />
    <title>OpenGloss • {{ payload.word }}</title>
    {% if chrome.use_tailwind %}
    <script src="https://cdn.jsdelivr.net/npm/@tailwindcss/browser@4"></script>
    {% endif %}
    {% if chrome.use_bootstrap %}
    <link href="https://cdn.jsdelivr.net/npm/bootstrap@5.3.8/dist/css/bootstrap.min.css" rel="stylesheet" integrity="sha384-sRIl4kxILFvY47J16cr9ZwB07vP4J8+LH7qKQnuqkuIAvNWLzeN8tE5YBujZqJLB" crossorigin="anonymous">
    <script src="https://cdn.jsdelivr.net/npm/bootstrap@5.3.8/dist/js/bootstrap.bundle.min.js" integrity="sha384-FKyoEForCGlyvwx9Hj09JcYn3nv7wiPVlz7YYwJrWVcXK/BmnVDxM+D2scQbITxI" crossorigin="anonymous"></script>
    {% endif %}
  </head>
  <body class="{{ chrome.body_class }}">
    <main class="{{ chrome.main_class }}">
      <div class="{{ chrome.card_class }} space-y-6">
        <div>
          <p class="{{ chrome.eyebrow_class }}">Lexeme #{{ payload.lexeme_id }}</p>
          <h1 class="{{ chrome.headline_class }}">{{ payload.word }}</h1>
          <p class="{{ chrome.lede_class }}">Entry ID: {{ payload.entry_id }}</p>
        </div>

        {% if payload.parts_of_speech.len() > 0 %}
        <section>
          <h2 class="text-xl font-semibold mb-2">Parts of speech</h2>
          <div class="flex flex-wrap gap-2">
            {% for pos in payload.parts_of_speech %}
            <span class="px-3 py-1 rounded-full bg-slate-200 text-sm">{{ pos }}</span>
            {% endfor %}
          </div>
        </section>
        {% endif %}

        {% if payload.text.is_some() %}
        <section>
          <h2 class="text-xl font-semibold mb-2">Entry Text</h2>
          <div class="bg-white shadow rounded p-4 whitespace-pre-line">{{ payload.text.as_ref().unwrap() }}</div>
        </section>
        {% endif %}

        {% if payload.all_definitions.len() > 0 %}
        <section>
          <h2 class="text-xl font-semibold mb-2">Definitions</h2>
          <ul class="list-disc pl-6 space-y-1">
            {% for definition in payload.all_definitions %}
            <li>{{ definition }}</li>
            {% endfor %}
          </ul>
        </section>
        {% endif %}

        <section>
          <h2 class="text-xl font-semibold mb-2">Senses ({{ payload.senses.len() }})</h2>
          <div class="space-y-4">
            {% for sense in payload.senses %}
            <article class="bg-white shadow rounded p-4">
              <p class="text-sm text-slate-500 mb-1">
                Sense #{{ sense.sense_index }}
                {% if sense.part_of_speech.is_some() %}
                  • {{ sense.part_of_speech.as_ref().unwrap() }}
                {% endif %}
              </p>
              <p class="font-medium mb-2">
                {% if sense.definition.is_some() %}
                  {{ sense.definition.as_ref().unwrap() }}
                {% else %}
                  Definition unavailable
                {% endif %}
              </p>
              {% if sense.synonyms.len() > 0 %}
              <p><strong>Synonyms:</strong>
                {% for syn in sense.synonyms %}
                  {% if loop.first %}
                    {{ syn }}
                  {% else %}
                    , {{ syn }}
                  {% endif %}
                {% endfor %}
              </p>
              {% endif %}
              {% if sense.antonyms.len() > 0 %}
              <p><strong>Antonyms:</strong>
                {% for ant in sense.antonyms %}
                  {% if loop.first %}
                    {{ ant }}
                  {% else %}
                    , {{ ant }}
                  {% endif %}
                {% endfor %}
              </p>
              {% endif %}
              {% if sense.examples.len() > 0 %}
              <p><strong>Examples:</strong>
                {% for example in sense.examples %}
                  {% if loop.first %}
                    {{ example }}
                  {% else %}
                    • {{ example }}
                  {% endif %}
                {% endfor %}
              </p>
              {% endif %}
            </article>
            {% endfor %}
          </div>
        </section>

        {% if payload.encyclopedia_entry.is_some() %}
        <section>
          <h2 class="text-xl font-semibold mb-2">Encyclopedia Entry</h2>
          <div class="bg-white shadow rounded p-4 whitespace-pre-line">{{ payload.encyclopedia_entry.as_ref().unwrap() }}</div>
        </section>
        {% endif %}
      </div>
    </main>
  </body>
</html>"#,
    ext = "html"
)]
struct LexemeTemplate<'a> {
    chrome: Chrome,
    payload: &'a LexemePayload,
}

#[derive(Template)]
#[template(
    source = r#"<!DOCTYPE html>
<html lang="en">
  <head>
    <meta charset="utf-8" />
    <meta name="viewport" content="width=device-width, initial-scale=1" />
    <title>OpenGloss • Search</title>
    {% if chrome.use_tailwind %}
    <script src="https://cdn.jsdelivr.net/npm/@tailwindcss/browser@4"></script>
    {% endif %}
    {% if chrome.use_bootstrap %}
    <link href="https://cdn.jsdelivr.net/npm/bootstrap@5.3.8/dist/css/bootstrap.min.css" rel="stylesheet" integrity="sha384-sRIl4kxILFvY47J16cr9ZwB07vP4J8+LH7qKQnuqkuIAvNWLzeN8tE5YBujZqJLB" crossorigin="anonymous">
    <script src="https://cdn.jsdelivr.net/npm/bootstrap@5.3.8/dist/js/bootstrap.bundle.min.js" integrity="sha384-FKyoEForCGlyvwx9Hj09JcYn3nv7wiPVlz7YYwJrWVcXK/BmnVDxM+D2scQbITxI" crossorigin="anonymous"></script>
    {% endif %}
  </head>
  <body class="{{ chrome.body_class }}">
    <main class="{{ chrome.main_class }}">
      <div class="{{ chrome.card_class }} space-y-4">
        <div>
          <p class="{{ chrome.eyebrow_class }}">Mode: {{ payload.mode }}</p>
          <h1 class="{{ chrome.headline_class }}">Search results for “{{ payload.query }}”</h1>
          <p class="{{ chrome.lede_class }}">{{ payload.results.len() }} matches (limit {{ payload.limit }}).</p>
        </div>
        {% if payload.results.len() == 0 %}
          <p>No results found.</p>
        {% else %}
        <div class="bg-white shadow rounded overflow-hidden">
          <table class="min-w-full">
            <thead class="bg-slate-100 text-left">
              <tr>
                <th class="px-4 py-2">Lexeme</th>
                <th class="px-4 py-2">Score</th>
                <th class="px-4 py-2">ID</th>
              </tr>
            </thead>
            <tbody>
              {% for hit in payload.results %}
              <tr class="{{ chrome.table_row_class }}">
                <td class="px-4 py-2">
                  <a href="/lexeme?word={{ hit.word }}" class="text-blue-700 hover:underline">{{ hit.word }}</a>
                </td>
                <td class="px-4 py-2">
                  {% if hit.score.is_some() %}
                    {{ hit.score.as_ref().unwrap() }}
                  {% else %}
                    —
                  {% endif %}
                </td>
                <td class="px-4 py-2">{{ hit.lexeme_id }}</td>
              </tr>
              {% endfor %}
            </tbody>
          </table>
        </div>
        {% endif %}
      </div>
    </main>
  </body>
</html>"#,
    ext = "html"
)]
struct SearchTemplate<'a> {
    chrome: Chrome,
    payload: &'a SearchResponsePayload,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, Default)]
#[serde(rename_all = "lowercase")]
enum SearchModeParam {
    #[default]
    Fuzzy,
    Substring,
}

impl fmt::Display for SearchModeParam {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SearchModeParam::Fuzzy => write!(f, "Fuzzy"),
            SearchModeParam::Substring => write!(f, "Substring"),
        }
    }
}

#[cfg(all(test, feature = "web"))]
mod tests {
    use super::*;
    use axum::{body, body::Body, http::Request};
    use tower::ServiceExt;

    fn test_router() -> Router {
        let state = Arc::new(AppState {
            default_search: SearchConfig::default(),
            theme: WebTheme::Tailwind,
        });
        build_router(state, false)
    }

    #[tokio::test]
    async fn api_lexeme_dog() {
        let router = test_router();
        let response = router
            .oneshot(
                Request::get("/api/lexeme?word=dog")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert!(response.status().is_success());
        let bytes = body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let payload: LexemePayload = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(payload.word.to_lowercase(), "dog");
    }

    #[tokio::test]
    async fn api_search_dog() {
        let router = test_router();
        let response = router
            .oneshot(
                Request::get("/api/search?q=dog&mode=substring&limit=5")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert!(response.status().is_success());
        let bytes = body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let payload: SearchResponsePayload = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(payload.query, "dog");
        assert!(!payload.results.is_empty());
    }
}
