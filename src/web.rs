use crate::{LexemeEntry, LexemeIndex, SearchConfig};
use askama::Html as HtmlEscaper;
use askama::{MarkupDisplay, Template};
use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::StatusCode,
    response::{Html, IntoResponse, Redirect, Response},
    routing::get,
};
use markdown::{Options as MarkdownOptions, to_html_with_options};
use percent_encoding::{NON_ALPHANUMERIC, utf8_percent_encode};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::BTreeMap;
use std::fmt;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::net::TcpListener;
use tokio::signal;
use tower_http::compression::CompressionLayer;
use tower_http::trace::{DefaultMakeSpan, DefaultOnResponse, TraceLayer};
use tracing::info;

type SharedState = Arc<AppState>;
const MAX_PREFIX_LEVEL: usize = 4;
const MAX_WORDS_DISPLAY: usize = 750;
const SITEMAP_BUCKETS: [&str; 27] = [
    "a", "b", "c", "d", "e", "f", "g", "h", "i", "j", "k", "l", "m", "n", "o", "p", "q", "r",
    "s", "t", "u", "v", "w", "x", "y", "z", "other",
];
const TYPEAHEAD_DEFAULT_LIMIT: usize = 12;
const TYPEAHEAD_MAX_LIMIT: usize = 50;
type SafeMarkup = MarkupDisplay<HtmlEscaper, String>;
type SafeJson = SafeMarkup;

#[derive(Clone)]
pub struct AppState {
    pub default_search: SearchConfig,
    pub theme: WebTheme,
    pub base_url: String,
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
    pub base_url: String,
}

impl Default for WebConfig {
    fn default() -> Self {
        Self {
            addr: SocketAddr::from(([127, 0, 0, 1], 8080)),
            enable_openapi: true,
            theme: WebTheme::default(),
            base_url: "http://127.0.0.1:8080".to_string(),
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
        base_url: config.base_url.clone(),
    });
    let router = build_router(state, config.enable_openapi);
    info!(
        %config.addr,
        theme = ?config.theme,
        openapi = config.enable_openapi,
        base = %config.base_url,
        "Binding HTTP listener"
    );
    let listener = TcpListener::bind(config.addr).await?;
    axum::serve(listener, router)
        .with_graceful_shutdown(shutdown_signal())
        .await?;
    info!("HTTP server exited");
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
        .route("/random", get(random_redirect))
        .route("/index", get(prefix_index_html))
        .route("/lexeme", get(lexeme_html))
        .route("/lexeme/:id", get(lexeme_html_by_id))
        .route("/search", get(search_html))
        .route("/api/lexeme", get(api_lexeme))
        .route("/api/search", get(api_search))
        .route("/api/typeahead", get(api_typeahead))
        .route("/healthz", get(health))
        .route("/sitemap.xml", get(sitemap_index))
        .route("/sitemap-:bucket", get(sitemap_bucket))
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
    Html(render_home(state.theme, &state.base_url))
}

async fn random_redirect() -> impl IntoResponse {
    let target = random_lexeme_path().unwrap_or_else(|| lexeme_path("encyclopedia"));
    Redirect::temporary(&target)
}

fn render_home(theme: WebTheme, base_url: &str) -> String {
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
    let title = "OpenGloss • Friendly Word Explorer";
    let intro = "Find clear definitions, encyclopedia notes, and related words for more than 150,000 modern English entries.";
    let typeahead_script = TYPEAHEAD_WIDGET;
    format!(
        r#"<!DOCTYPE html>
<html lang="en">
  <head>
    <meta charset="utf-8" />
    <meta name="viewport" content="width=device-width, initial-scale=1" />
    <title>{title}</title>
    {css_tag}
    {js_tag}
    <script type="application/ld+json">
{site_json_ld}
    </script>
  </head>
  <body class="{body_class}">
    <main class="{main_class}">
      <div class="{card_class} space-y-6">
        <section class="space-y-3">
          <p class="{eyebrow_class}">OpenGloss v{version}</p>
          <h1 class="{headline_class}">Your dictionary, encyclopedia, and thesaurus in one stop.</h1>
          <p class="{lede_class}">{intro}</p>
        </section>
        <form action="/search" method="get" class="w-full bg-white shadow rounded p-4 flex flex-col gap-3" data-role="typeahead-form">
          <label class="text-sm font-semibold text-slate-600" for="home-search-input">Look up a word or phrase</label>
          <div class="d-flex flex-column flex-md-row gap-3">
            <div class="flex-1 position-relative relative">
              <input id="home-search-input" name="q" data-role="typeahead-input" placeholder="Try “solar eclipse”, “geometric solid”, “gratitude”…" class="w-full form-control px-4 py-2 rounded border border-slate-300 focus:border-slate-500 focus:ring-2 focus:ring-slate-300" autocomplete="off" />
              <div class="typeahead-panel" data-role="typeahead-panel" role="listbox" hidden></div>
            </div>
            <select name="mode" class="form-select w-full md:w-auto px-3 py-2 rounded border border-slate-300">
              <option value="fuzzy">Best match</option>
              <option value="substring">Contains text</option>
            </select>
            <button type="submit" class="{button_class} w-full md:w-auto">Search</button>
          </div>
          <p id="home-search-status" data-role="typeahead-status" class="text-xs text-slate-500 mb-0"></p>
        </form>
        <div class="{cta_group}">
          <a href="/lexeme?word=farm" class="{button_class}">See an example entry</a>
          <a href="/index" class="{button_class}">Browse the word index</a>
          <a href="/random" class="{button_class}">Random word</a>
        </div>
      </div>
      <footer class="mt-12 text-center text-sm text-slate-500 space-y-2">
        <p>Want the geeky bits? Run the bundled CLI or call the JSON API for batch lookups.</p>
        <p class="text-xs">Suggestions come from an offline trie baked into the Rust binary. Power users can hit <code>/api/typeahead</code> or <code>/api/search</code> directly.</p>
        <p>
          Learn why we built OpenGloss in
          <a href="https://www.arxiv.org/abs/2511.18622" class="text-slate-700 underline hover:text-slate-900" target="_blank" rel="noopener noreferrer">
            “OpenGloss: A Synthetic Encyclopedic Dictionary and Semantic Knowledge Graph”
          </a>.
        </p>
      </footer>
    </main>
    {typeahead_script}
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
        site_json_ld = indent_json(&website_json_ld(base_url), 4),
    )
}

const TYPEAHEAD_WIDGET: &str = r#"
<style>
  .typeahead-panel {
    position: absolute;
    top: calc(100% + 0.25rem);
    left: 0;
    right: 0;
    z-index: 20;
    background: #fff;
    border: 1px solid rgba(100, 116, 139, 0.4);
    border-radius: 0.65rem;
    box-shadow: 0 15px 35px rgba(15, 23, 42, 0.15);
    max-height: 16rem;
    overflow-y: auto;
  }
  .typeahead-panel[hidden] {
    display: none;
  }
  .typeahead-option {
    width: 100%;
    text-align: left;
    padding: 0.5rem 0.9rem;
    border: none;
    background: transparent;
    font-size: 0.95rem;
    color: #0f172a;
    cursor: pointer;
  }
  .typeahead-option + .typeahead-option {
    border-top: 1px solid rgba(148, 163, 184, 0.3);
  }
  .typeahead-option.is-active,
  .typeahead-option:hover,
  .typeahead-option:focus {
    background: rgba(148, 163, 184, 0.18);
    outline: none;
  }
</style>
<script>
  (function() {
    if (!window.fetch) return;
    const forms = document.querySelectorAll('[data-role="typeahead-form"]');
    const formatStatus = (count) => {
      if (!count) return 'No quick matches yet.';
      if (count === 1) return 'Showing 1 quick match.';
      return `Showing ${count} quick matches.`;
    };
    forms.forEach((form) => {
      const input = form.querySelector('[data-role="typeahead-input"]');
      const panel = form.querySelector('[data-role="typeahead-panel"]');
      const status = form.querySelector('[data-role="typeahead-status"]');
      if (!input || !panel) return;
      let controller;
      let suggestions = [];
      let activeIndex = -1;
      const hidePanel = () => {
        panel.setAttribute('hidden', 'hidden');
        panel.innerHTML = '';
        activeIndex = -1;
        activeIndex = -1;
      };
      const showPanel = () => {
        panel.removeAttribute('hidden');
      };
      const updateStatus = (message) => {
        if (status) status.textContent = message || '';
      };
      const highlight = (index) => {
        const nodes = panel.querySelectorAll('[data-role="typeahead-option"]');
        nodes.forEach((node, nodeIndex) => {
          if (nodeIndex === index) {
            node.classList.add('is-active');
            node.setAttribute('aria-selected', 'true');
            node.scrollIntoView({ block: 'nearest' });
            activeIndex = index;
          } else {
            node.classList.remove('is-active');
            node.setAttribute('aria-selected', 'false');
          }
        });
      };
      const navigateTo = (word) => {
        if (!word) return;
        window.location.href = `/lexeme?word=${encodeURIComponent(word)}`;
      };
      const renderSuggestions = () => {
        panel.innerHTML = '';
        panel.scrollTop = 0;
        suggestions.forEach((item) => {
          const button = document.createElement('button');
          button.type = 'button';
          button.className = 'typeahead-option';
          button.textContent = item.word;
          button.setAttribute('data-role', 'typeahead-option');
          button.setAttribute('role', 'option');
          button.setAttribute('aria-selected', 'false');
          button.addEventListener('pointerdown', (event) => event.preventDefault());
          button.addEventListener('click', () => {
            navigateTo(item.word);
          });
          panel.appendChild(button);
        });
        if (suggestions.length === 0) {
          hidePanel();
        } else {
          showPanel();
        }
      };
      const fetchSuggestions = async (query) => {
        if (controller) controller.abort();
        controller = new AbortController();
        updateStatus('Loading quick matches…');
        try {
          const response = await fetch(`/api/typeahead?q=${encodeURIComponent(query)}&limit=12&mode=prefix`, { signal: controller.signal });
          if (!response.ok) {
            hidePanel();
            updateStatus('');
            return;
          }
          const payload = await response.json();
          suggestions = payload.suggestions || [];
          renderSuggestions();
          updateStatus(formatStatus(suggestions.length));
        } catch (error) {
          if (error.name === 'AbortError') return;
          hidePanel();
          updateStatus('');
        }
      };
      input.addEventListener('input', (event) => {
        const query = event.target.value.trim();
        if (!query) {
          hidePanel();
          updateStatus('');
          if (controller) controller.abort();
          return;
        }
        fetchSuggestions(query);
      });
      input.addEventListener('keydown', (event) => {
        if (event.key === 'Escape') {
          hidePanel();
          updateStatus('');
          return;
        }
        if (panel.hasAttribute('hidden') || !suggestions.length) return;
        if (event.key === 'ArrowDown') {
          event.preventDefault();
          const next = activeIndex + 1 >= suggestions.length ? 0 : activeIndex + 1;
          highlight(next);
          return;
        }
        if (event.key === 'ArrowUp') {
          event.preventDefault();
          const next = activeIndex - 1 < 0 ? suggestions.length - 1 : activeIndex - 1;
          highlight(next);
          return;
        }
        if (event.key === 'Enter' && activeIndex >= 0 && suggestions[activeIndex]) {
          event.preventDefault();
          navigateTo(suggestions[activeIndex].word);
          return;
        }
      });
      document.addEventListener('click', (event) => {
        if (!form.contains(event.target)) {
          hidePanel();
        }
      });
    });
  })();
</script>
"#;

async fn health() -> impl IntoResponse {
    Json(json!({ "status": "ok", "service": "opengloss-web" }))
}

async fn lexeme_html(
    State(state): State<SharedState>,
    Query(params): Query<LexemeParams>,
) -> impl IntoResponse {
    lexeme_html_inner(state, params).await
}

async fn lexeme_html_by_id(
    State(state): State<SharedState>,
    Path(id): Path<u32>,
) -> impl IntoResponse {
    let params = LexemeParams {
        word: None,
        id: Some(id),
    };
    lexeme_html_inner(state, params).await
}

async fn lexeme_html_inner(state: SharedState, params: LexemeParams) -> Html<String> {
    match entry_from_params(&params) {
        Ok(entry) => {
            let chrome = Chrome::new(state.theme);
            let payload = LexemePayload::from_entry(&entry);
            let json_ld =
                MarkupDisplay::new_safe(lexeme_json_ld(&entry, &state.base_url), HtmlEscaper);
            let encyclopedia_html = render_markdown(payload.encyclopedia_entry.as_deref());
            let pos_chips = payload
                .parts_of_speech
                .iter()
                .map(|label| PosChip {
                    label: label.as_str(),
                    css_class: pos_chip_class(label),
                })
                .collect();
            let senses = payload
                .senses
                .iter()
                .map(|sense| SenseBlock {
                    payload: sense,
                    definition_html: render_markdown(sense.definition.as_deref()),
                    synonyms: relation_links(&sense.synonyms),
                    antonyms: relation_links(&sense.antonyms),
                    hypernyms: relation_links(&sense.hypernyms),
                    hyponyms: relation_links(&sense.hyponyms),
                })
                .collect();
            let sense_count = payload.senses.len();
    let template = LexemeTemplate {
        chrome,
        payload: &payload,
        canonical_url: absolute_lexeme_url(&state.base_url, entry.word()),
        json_ld,
        encyclopedia_html,
        pos_chips,
        senses,
        sense_count,
        typeahead_header: typeahead_header_html(),
    };
            Html(template.render().unwrap_or_else(|err| render_error_page(state.theme, err.to_string())))
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
            let json_ld = MarkupDisplay::new_safe(
                search_page_json_ld(&payload, &state.base_url),
                HtmlEscaper,
            );
    let template = SearchTemplate {
        chrome,
        payload: &payload,
        json_ld,
        typeahead_header: typeahead_header_html(),
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

async fn prefix_index_html(
    State(state): State<SharedState>,
    Query(params): Query<IndexParams>,
) -> impl IntoResponse {
    let letters = params.letters.unwrap_or(1).clamp(1, MAX_PREFIX_LEVEL);
    let display_prefix = params
        .prefix
        .unwrap_or_default()
        .trim()
        .chars()
        .take(letters)
        .collect::<String>();
    let normalized = display_prefix.to_lowercase();
    let mut payload = build_index_payload(LexemeIndex::all_words(), letters, &normalized);
    payload.prefix = display_prefix;
    let chrome = Chrome::new(state.theme);
    let json_ld = MarkupDisplay::new_safe(defined_term_set_json_ld(&state.base_url), HtmlEscaper);
    let template = IndexTemplate {
        chrome,
        payload: &payload,
        json_ld,
        base_url: &state.base_url,
        typeahead_header: typeahead_header_html(),
    };
    Html(
        template
            .render()
            .unwrap_or_else(|err| render_error_page(state.theme, err.to_string())),
    )
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

async fn api_typeahead(
    Query(params): Query<TypeaheadParams>,
) -> Result<Json<TypeaheadResponse>, ApiError> {
    let query = params
        .q
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| ApiError::bad_request("missing q"))?;
    let limit = params
        .limit
        .unwrap_or(TYPEAHEAD_DEFAULT_LIMIT)
        .clamp(1, TYPEAHEAD_MAX_LIMIT);
    let mode = params.mode.unwrap_or(TypeaheadMode::Prefix);
    let mut suggestions = match mode {
        TypeaheadMode::Prefix => LexemeIndex::prefix(query, limit),
        TypeaheadMode::Substring => LexemeIndex::search_contains(query, limit),
    };
    if mode == TypeaheadMode::Prefix && suggestions.len() < limit && query.len() >= 3 {
        let fallback = LexemeIndex::search_contains(query, limit);
        for (word, lexeme_id) in fallback {
            if !suggestions.iter().any(|(existing, _)| existing == &word) {
                suggestions.push((word, lexeme_id));
                if suggestions.len() >= limit {
                    break;
                }
            }
        }
    }
    let suggestions = suggestions
        .into_iter()
        .map(|(word, lexeme_id)| TypeaheadSuggestion { word, lexeme_id })
        .collect();
    Ok(Json(TypeaheadResponse {
        query: query.to_string(),
        mode,
        suggestions,
    }))
}

async fn sitemap_index(State(state): State<SharedState>) -> impl IntoResponse {
    let mut body = String::with_capacity(2048);
    body.push_str(r#"<?xml version="1.0" encoding="UTF-8"?>"#);
    body.push_str(r#"<sitemapindex xmlns="http://www.sitemaps.org/schemas/sitemap/0.9">"#);
    for bucket in sitemap_bucket_names() {
        let loc = format!("{}/sitemap-{}.xml", state.base_url, bucket);
        body.push_str("<sitemap><loc>");
        body.push_str(&xml_escape(&loc));
        body.push_str("</loc></sitemap>");
    }
    body.push_str("</sitemapindex>");
    xml_response(body)
}

async fn sitemap_bucket(
    State(state): State<SharedState>,
    Path(bucket): Path<String>,
) -> impl IntoResponse {
    let bucket_normalized = bucket.trim_end_matches(".xml").to_ascii_lowercase();
    if !sitemap_bucket_names()
        .iter()
        .any(|candidate| *candidate == bucket_normalized)
    {
        return Response::builder()
            .status(StatusCode::NOT_FOUND)
            .body("bucket not found".into())
            .unwrap();
    }
    let words = words_for_bucket(&bucket_normalized);
    let mut body = String::with_capacity(2048);
    body.push_str(r#"<?xml version="1.0" encoding="UTF-8"?>"#);
    body.push_str(r#"<urlset xmlns="http://www.sitemaps.org/schemas/sitemap/0.9">"#);
    for word in words {
        let loc = absolute_lexeme_url(&state.base_url, &word);
        body.push_str("<url><loc>");
        body.push_str(&xml_escape(&loc));
        body.push_str("</loc><changefreq>weekly</changefreq><priority>0.5</priority></url>");
    }
    body.push_str("</urlset>");
    xml_response(body)
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

#[derive(Debug, Deserialize)]
struct IndexParams {
    letters: Option<usize>,
    prefix: Option<String>,
}

#[derive(Debug, Deserialize)]
struct TypeaheadParams {
    q: Option<String>,
    limit: Option<usize>,
    mode: Option<TypeaheadMode>,
}

#[derive(Debug, Deserialize, Serialize, Copy, Clone, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
enum TypeaheadMode {
    Prefix,
    Substring,
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
struct PartOfSpeechFrequencyPayload {
    label: String,
    count: usize,
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
    pos_frequency: Vec<PartOfSpeechFrequencyPayload>,
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

#[derive(Debug, Clone)]
struct PrefixLevelPayload {
    length: usize,
    prefixes: Vec<PrefixOptionPayload>,
}

#[derive(Debug, Clone)]
struct PrefixOptionPayload {
    prefix: String,
    count: usize,
    link: String,
    active: bool,
}

#[derive(Debug, Clone)]
struct WordLinkPayload<'a> {
    word: &'a str,
    lexeme_id: u32,
    href: String,
}

#[derive(Debug, Clone)]
struct IndexPagePayload<'a> {
    letters: usize,
    prefix: String,
    total_matches: usize,
    max_display: usize,
    levels: Vec<PrefixLevelPayload>,
    words: Vec<WordLinkPayload<'a>>,
}

struct SenseBlock<'a> {
    payload: &'a SensePayload,
    definition_html: Option<String>,
    synonyms: Vec<RelationLink>,
    antonyms: Vec<RelationLink>,
    hypernyms: Vec<RelationLink>,
    hyponyms: Vec<RelationLink>,
}

#[derive(Debug, Clone)]
struct RelationLink {
    label: String,
    href: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
struct TypeaheadResponse {
    query: String,
    mode: TypeaheadMode,
    suggestions: Vec<TypeaheadSuggestion>,
}

#[derive(Debug, Serialize, Deserialize)]
struct TypeaheadSuggestion {
    word: String,
    lexeme_id: u32,
}

#[derive(Debug, Clone)]
struct PosChip<'a> {
    label: &'a str,
    css_class: &'static str,
}

impl LexemePayload {
    fn from_entry(entry: &LexemeEntry<'_>) -> Self {
        let mut pos_counts: BTreeMap<String, usize> = BTreeMap::new();
        let senses = entry
            .senses()
            .map(|sense| SensePayload {
                lexeme_id: sense.lexeme_id(),
                sense_index: sense.sense_index(),
                part_of_speech: sense.part_of_speech().map(|s| {
                    let value = s.to_string();
                    *pos_counts.entry(value.clone()).or_insert(0) += 1;
                    value
                }),
                definition: sense.definition().map(|s| s.to_string()),
                synonyms: collect_iter(sense.synonyms()),
                antonyms: collect_iter(sense.antonyms()),
                hypernyms: collect_iter(sense.hypernyms()),
                hyponyms: collect_iter(sense.hyponyms()),
                examples: collect_iter(sense.examples()),
            })
            .collect::<Vec<_>>();

        let unspecified = senses
            .iter()
            .filter(|sense| sense.part_of_speech.is_none())
            .count();
        if unspecified > 0 {
            pos_counts.insert("Unspecified".to_string(), unspecified);
        }

        let mut pos_frequency = pos_counts
            .into_iter()
            .map(|(label, count)| PartOfSpeechFrequencyPayload { label, count })
            .collect::<Vec<_>>();
        pos_frequency.sort_by(|a, b| b.count.cmp(&a.count).then_with(|| a.label.cmp(&b.label)));

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
            pos_frequency,
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

fn build_index_payload<'a>(
    words: &'a [(String, u32)],
    letters: usize,
    prefix: &str,
) -> IndexPagePayload<'a> {
    let levels = build_prefix_levels(words, letters, prefix);
    let (word_rows, total_matches) = filter_words_by_prefix(words, prefix);
    IndexPagePayload {
        letters,
        prefix: prefix.to_string(),
        total_matches,
        max_display: MAX_WORDS_DISPLAY,
        levels,
        words: word_rows,
    }
}

fn build_prefix_levels(
    words: &[(String, u32)],
    max_length: usize,
    selected_prefix: &str,
) -> Vec<PrefixLevelPayload> {
    let selected_chars: Vec<char> = selected_prefix.chars().collect();
    (1..=max_length)
        .map(|length| {
            let active = if selected_chars.len() >= length {
                Some(
                    selected_chars
                        .iter()
                        .take(length)
                        .collect::<String>()
                        .to_lowercase(),
                )
            } else {
                None
            };
            let mut counts = BTreeMap::new();
            for (word, _) in words {
                if let Some(prefix) = take_prefix(word, length) {
                    *counts.entry(prefix).or_insert(0) += 1;
                }
            }
            let prefixes = counts
                .into_iter()
                .map(|(prefix, count)| {
                    let link = format!(
                        "/index?letters={}&prefix={}",
                        max_length,
                        encode_component(&prefix)
                    );
                    let active_flag = active.as_deref() == Some(prefix.as_str());
                    PrefixOptionPayload {
                        prefix,
                        count,
                        link,
                        active: active_flag,
                    }
                })
                .collect();
            PrefixLevelPayload { length, prefixes }
        })
        .collect()
}

fn take_prefix(word: &str, length: usize) -> Option<String> {
    if length == 0 {
        return Some(String::new());
    }
    let mut prefix = String::new();
    let mut chars = word.chars();
    for _ in 0..length {
        match chars.next() {
            Some(ch) => prefix.push(ch),
            None => return None,
        }
    }
    Some(prefix.to_lowercase())
}

fn filter_words_by_prefix<'a>(
    words: &'a [(String, u32)],
    prefix: &str,
) -> (Vec<WordLinkPayload<'a>>, usize) {
    let mut rows = Vec::new();
    let mut total = 0;
    if prefix.is_empty() {
        for (word, lexeme_id) in words.iter().take(MAX_WORDS_DISPLAY) {
            rows.push(WordLinkPayload {
                word: word.as_str(),
                lexeme_id: *lexeme_id,
                href: lexeme_path(word),
            });
        }
        total = words.len();
        return (rows, total);
    }
    for (word, lexeme_id) in words {
        if word.to_lowercase().starts_with(prefix) {
            total += 1;
            if rows.len() < MAX_WORDS_DISPLAY {
                rows.push(WordLinkPayload {
                    word: word.as_str(),
                    lexeme_id: *lexeme_id,
                    href: lexeme_path(word),
                });
            }
        }
    }
    (rows, total)
}

fn encode_component(value: &str) -> String {
    utf8_percent_encode(value, NON_ALPHANUMERIC).to_string()
}

fn lexeme_path(word: &str) -> String {
    format!("/lexeme?word={}", encode_component(word))
}

fn random_lexeme_path() -> Option<String> {
    let words = LexemeIndex::all_words();
    if words.is_empty() {
        return None;
    }
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    let idx = (nanos % words.len() as u128) as usize;
    Some(lexeme_path(words[idx].0.as_str()))
}

fn absolute_lexeme_url(base_url: &str, word: &str) -> String {
    format!("{}{}", base_url, lexeme_path(word))
}

fn sitemap_bucket_names() -> &'static [&'static str] {
    &SITEMAP_BUCKETS
}

fn bucket_for_word(word: &str) -> &'static str {
    if let Some(ch) = word.chars().next() {
        if ch.is_ascii_alphabetic() {
            let lower = ch.to_ascii_lowercase() as u8;
            if (b'a'..=b'z').contains(&lower) {
                let idx = (lower - b'a') as usize;
                return SITEMAP_BUCKETS[idx];
            }
        }
    }
    SITEMAP_BUCKETS[SITEMAP_BUCKETS.len() - 1]
}

fn words_for_bucket(bucket: &str) -> Vec<String> {
    LexemeIndex::all_words()
        .iter()
        .filter_map(|(word, _)| {
            if bucket_for_word(word) == bucket {
                Some(word.clone())
            } else {
                None
            }
        })
        .collect()
}

fn typeahead_header_html() -> String {
    format!(
        r#"
    <header class="w-full max-w-5xl mb-6">
      <div class="flex flex-col md:flex-row gap-3 items-start md:items-center justify-between">
        <a href="/" class="text-sm font-semibold text-slate-600 hover:text-slate-900 flex items-center gap-2">
          <span aria-hidden="true">←</span> Home
        </a>
        <form action="/search" method="get" class="flex flex-col md:flex-row gap-2 w-full md:w-auto" data-role="typeahead-form">
          <div class="relative position-relative flex-1">
            <input type="text" name="q" data-role="typeahead-input" placeholder="Search lexemes…" class="w-full px-3 py-2 rounded border border-slate-300 focus:border-slate-500 focus:ring-2 focus:ring-slate-300" autocomplete="off" />
            <div class="typeahead-panel" data-role="typeahead-panel" role="listbox" hidden></div>
          </div>
          <select name="mode" class="px-3 py-2 rounded border border-slate-300">
            <option value="fuzzy">Fuzzy</option>
            <option value="substring">Substring</option>
          </select>
          <button type="submit" class="inline-flex items-center justify-center rounded-full bg-slate-900 text-white px-4 py-2 font-semibold shadow hover:bg-slate-800 transition">🔍</button>
        </form>
      </div>
    </header>
    {widget}
    "#,
        widget = TYPEAHEAD_WIDGET
    )
}


fn defined_term_set_json_ld(base_url: &str) -> String {
    let index_url = format!("{base}/index", base = base_url);
    serde_json::to_string_pretty(&json!({
        "@context": "https://schema.org",
        "@type": "DefinedTermSet",
        "@id": index_url,
        "name": "OpenGloss Lexicon",
        "url": index_url,
        "numberOfItems": LexemeIndex::all_words().len(),
    }))
    .unwrap_or_else(|_| "{}".to_string())
}

fn lexeme_json_ld(entry: &LexemeEntry<'_>, base_url: &str) -> String {
    let word_url = absolute_lexeme_url(base_url, entry.word());
    let index_url = format!("{}/index", base_url);
    let mut graph = vec![json!({
        "@type": "DefinedTermSet",
        "@id": index_url,
        "name": "OpenGloss Lexicon",
        "url": index_url,
    })];
    let mut defined_term = json!({
        "@type": "DefinedTerm",
        "@id": word_url,
        "url": word_url,
        "name": entry.word(),
        "inDefinedTermSet": index_url,
        "termCode": entry.lexeme_id(),
        "mainEntityOfPage": word_url,
    });
    if let Some(definition) = entry.all_definitions().next() {
        defined_term["description"] = json!(definition);
    }
    let synonyms: Vec<_> = entry.all_synonyms().collect();
    if !synonyms.is_empty() {
        defined_term["alternateName"] = json!(synonyms);
    }
    let parts: Vec<_> = entry.parts_of_speech().collect();
    if !parts.is_empty() {
        defined_term["lexicalCategory"] = json!(parts);
    }
    if let Some(article) = entry.encyclopedia_entry() {
        defined_term["articleBody"] = json!(article);
    }
    graph.push(defined_term);
    graph.push(breadcrumb_json_ld(base_url, entry.word()));
    serde_json::to_string_pretty(&json!({
        "@context": "https://schema.org",
        "@graph": graph
    }))
    .unwrap_or_else(|_| "{}".to_string())
}

fn breadcrumb_json_ld(base_url: &str, word: &str) -> serde_json::Value {
    json!({
        "@type": "BreadcrumbList",
        "itemListElement": [
            { "@type": "ListItem", "position": 1, "name": "Home", "item": base_url },
            { "@type": "ListItem", "position": 2, "name": "Lexeme Index", "item": format!("{}/index", base_url) },
            { "@type": "ListItem", "position": 3, "name": word, "item": absolute_lexeme_url(base_url, word) }
        ]
    })
}

fn search_page_json_ld(payload: &SearchResponsePayload, base_url: &str) -> String {
    let results = payload
        .results
        .iter()
        .take(20)
        .enumerate()
        .map(|(idx, hit)| {
            json!({
                "@type": "ListItem",
                "position": idx as i32 + 1,
                "name": hit.word,
                "url": absolute_lexeme_url(base_url, &hit.word),
            })
        })
        .collect::<Vec<_>>();
    let page_url = format!(
        "{}/search?q={}&mode={}",
        base_url,
        encode_component(&payload.query),
        payload.mode.query_value()
    );
    serde_json::to_string_pretty(&json!({
        "@context": "https://schema.org",
        "@type": "SearchResultsPage",
        "name": format!("Search results for {}", payload.query),
        "url": page_url,
        "mainEntity": {
            "@type": "ItemList",
            "itemListElement": results,
        },
        "potentialAction": {
            "@type": "SearchAction",
            "target": format!("{}/search?q={{search_term_string}}&mode=fuzzy", base_url),
            "query-input": "required name=search_term_string"
        }
    }))
    .unwrap_or_else(|_| "{}".to_string())
}

fn website_json_ld(base_url: &str) -> String {
    serde_json::to_string_pretty(&json!({
        "@context": "https://schema.org",
        "@type": "WebSite",
        "url": base_url,
        "potentialAction": {
            "@type": "SearchAction",
            "target": format!("{}/search?q={{search_term_string}}&mode=fuzzy", base_url),
            "query-input": "required name=search_term_string"
        }
    }))
    .unwrap_or_else(|_| "{}".to_string())
}

fn indent_json(content: &str, spaces: usize) -> String {
    let padding = " ".repeat(spaces);
    content
        .lines()
        .map(|line| format!("{padding}{line}"))
        .collect::<Vec<_>>()
        .join("\n")
}

fn xml_response(body: String) -> Response {
    (
        [(axum::http::header::CONTENT_TYPE, "application/xml")],
        body,
    )
        .into_response()
}

fn xml_escape(input: &str) -> String {
    input
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

fn markdown_options() -> MarkdownOptions {
    let mut options = MarkdownOptions::gfm();
    // Dataset entries embed trusted HTML (headings, tables, iframes), so allow it through.
    options.compile.allow_dangerous_html = true;
    options.compile.allow_dangerous_protocol = true;
    options.compile.gfm_tagfilter = false;
    options
}

fn render_markdown(input: Option<&str>) -> Option<String> {
    input.and_then(render_markdown_str)
}

fn render_markdown_str(input: &str) -> Option<String> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return None;
    }
    let options = markdown_options();
    let html = to_html_with_options(trimmed, &options).unwrap_or_else(|_| trimmed.to_string());
    Some(html)
}

fn relation_links(terms: &[String]) -> Vec<RelationLink> {
    terms
        .iter()
        .map(|term| {
            let href = LexemeIndex::entry_by_word(term)
                .map(|_| lexeme_path(term));
            RelationLink {
                label: term.clone(),
                href,
            }
        })
        .collect()
}

fn pos_chip_class(label: &str) -> &'static str {
    let normalized = label.to_ascii_lowercase();
    let text = normalized.as_str();
    if text.contains("noun") {
        "pos-chip-noun"
    } else if text.contains("verb") {
        "pos-chip-verb"
    } else if text.contains("adjective") || text.contains("adj") {
        "pos-chip-adjective"
    } else if text.contains("adverb") {
        "pos-chip-adverb"
    } else if text.contains("pronoun") {
        "pos-chip-pronoun"
    } else if text.contains("determiner") || text.contains("det") {
        "pos-chip-determiner"
    } else if text.contains("preposition") {
        "pos-chip-preposition"
    } else if text.contains("conjunction") {
        "pos-chip-conjunction"
    } else if text.contains("interjection") {
        "pos-chip-interjection"
    } else if text.contains("numeral") || text.contains("number") {
        "pos-chip-numeral"
    } else {
        ""
    }
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
    <link rel="canonical" href="{{ canonical_url }}">
    <style>
      .rich-text {
        line-height: 1.65;
      }
      .rich-text p {
        margin-bottom: 1rem;
      }
      .rich-text ul,
      .rich-text ol {
        margin-bottom: 1rem;
        padding-left: 1.5rem;
      }
      .rich-text li + li {
        margin-top: 0.35rem;
      }
      .rich-text code {
        background-color: rgba(15, 23, 42, 0.08);
        padding: 0.15rem 0.35rem;
        border-radius: 0.25rem;
      }
      .rich-text pre {
        padding: 0.75rem;
        border-radius: 0.5rem;
        background-color: rgba(15, 23, 42, 0.08);
        overflow-x: auto;
        margin-bottom: 1rem;
      }
      .rich-text > :last-child {
        margin-bottom: 0;
      }
      .pos-chip {
        display: inline-flex;
        align-items: center;
        padding: 0.35rem 0.9rem;
        border-radius: 9999px;
        background-color: rgba(15, 23, 42, 0.05);
        border: 1px solid rgba(15, 23, 42, 0.08);
        color: #334155;
        font-size: 0.875rem;
        font-weight: 600;
      }
      .pos-chip-noun {
        background-color: #eef2ff;
        border-color: #c7d2fe;
        color: #312e81;
      }
      .pos-chip-verb {
        background-color: #ecfdf5;
        border-color: #a7f3d0;
        color: #065f46;
      }
      .pos-chip-adjective {
        background-color: #fff7ed;
        border-color: #fed7aa;
        color: #92400e;
      }
      .pos-chip-adverb {
        background-color: #f4f3ff;
        border-color: #c4b5fd;
        color: #4c1d95;
      }
      .pos-chip-pronoun {
        background-color: #f0fdfa;
        border-color: #99f6e4;
        color: #115e59;
      }
      .pos-chip-determiner {
        background-color: #fef2f2;
        border-color: #fecaca;
        color: #991b1b;
      }
      .pos-chip-preposition {
        background-color: #eff6ff;
        border-color: #bfdbfe;
        color: #1d4ed8;
      }
      .pos-chip-conjunction {
        background-color: #fdf2f8;
        border-color: #fbcfe8;
        color: #9d174d;
      }
      .pos-chip-interjection {
        background-color: #faf5ff;
        border-color: #e9d5ff;
        color: #6b21a8;
      }
      .pos-chip-numeral {
        background-color: #f5f5f4;
        border-color: #e7e5e4;
        color: #44403c;
      }
      .relation-chip-group {
        display: flex;
        flex-wrap: wrap;
        gap: 0.45rem;
      }
      .relation-chip {
        display: inline-flex;
        align-items: center;
        padding: 0.25rem 0.85rem;
        border-radius: 9999px;
        background-color: rgba(15, 23, 42, 0.07);
        color: #0f172a;
        border: 1px solid rgba(15, 23, 42, 0.12);
        font-size: 0.85rem;
        text-decoration: none;
        transition: background-color 150ms ease, color 150ms ease;
      }
      .relation-chip:hover {
        background-color: rgba(15, 23, 42, 0.12);
        color: #020617;
        text-decoration: none;
      }
      .relation-chip-disabled {
        cursor: not-allowed;
        opacity: 0.6;
        background-color: rgba(15, 23, 42, 0.04);
        border-style: dashed;
      }
      .overview-grid {
        align-items: stretch;
      }
      .overview-card {
        display: flex;
        align-items: center;
        justify-content: space-between;
        gap: 0.75rem;
        padding: 0.65rem 1rem;
        border-radius: 0.9rem;
        background-color: #fff;
        box-shadow: 0 8px 20px rgba(15, 23, 42, 0.08);
        min-height: 0;
      }
      .overview-title {
        font-size: 0.7rem;
        letter-spacing: 0.08em;
        text-transform: uppercase;
        color: #64748b;
        margin-bottom: 0.15rem;
      }
      .overview-detail {
        font-size: 0.9rem;
        color: #334155;
        margin: 0;
      }
      .overview-value {
        font-size: 1.8rem;
        font-weight: 600;
        color: #0f172a;
        margin: 0;
        white-space: nowrap;
      }
      .overview-link {
        font-size: 0.85rem;
        font-weight: 600;
        color: #0f172a;
        text-decoration: none;
        padding: 0.3rem 0.75rem;
        border-radius: 999px;
        border: 1px solid rgba(15, 23, 42, 0.15);
      }
      .overview-link:hover {
        background-color: rgba(15, 23, 42, 0.08);
      }
      .overview-pos-list {
        display: flex;
        flex-wrap: wrap;
        gap: 0.3rem;
        margin: 0;
        padding: 0;
      }
      .overview-pos-chip {
        font-size: 0.85rem;
        color: #0f172a;
        background-color: rgba(15, 23, 42, 0.06);
        padding: 0.15rem 0.5rem;
        border-radius: 999px;
      }
    </style>
    <script type="application/ld+json">
    {{ json_ld }}
    </script>
  </head>
  <body class="{{ chrome.body_class }}">
    <main class="{{ chrome.main_class }}">
      {{ typeahead_header|safe }}
      <div class="{{ chrome.card_class }} space-y-6">
        <div>
          <p class="{{ chrome.eyebrow_class }}">Lexeme #{{ payload.lexeme_id }}</p>
          <h1 class="{{ chrome.headline_class }}">{{ payload.word }}</h1>
          <p class="{{ chrome.lede_class }}">Entry ID: {{ payload.entry_id }}</p>
        </div>

        <nav class="flex flex-wrap gap-3 nav nav-pills d-flex align-items-center text-sm font-semibold text-slate-600 mb-2" aria-label="Lexeme navigation">
          <a href='#overview' class="nav-link px-3 py-1 rounded-full bg-slate-200 hover:bg-slate-300 text-slate-700">Overview</a>
          {% if payload.parts_of_speech.len() > 0 %}
          <a href='#parts-of-speech' class="nav-link px-3 py-1 rounded-full bg-slate-200 hover:bg-slate-300 text-slate-700">Parts of speech</a>
          {% endif %}
          {% if sense_count > 0 %}
          <a href='#senses' class="nav-link px-3 py-1 rounded-full bg-slate-200 hover:bg-slate-300 text-slate-700">Senses</a>
          {% endif %}
          {% if encyclopedia_html.is_some() %}
          <a href='#encyclopedia' class="nav-link px-3 py-1 rounded-full bg-slate-200 hover:bg-slate-300 text-slate-700">Encyclopedia</a>
          {% endif %}
        </nav>

        <section id="overview">
          <div class="grid gap-3 md:grid-cols-3 row row-cols-1 row-cols-md-3 g-2 overview-grid">
            <div class="overview-card">
              <div>
                <p class="overview-title">Sense coverage</p>
                <p class="overview-detail">documented sense{% if sense_count != 1 %}s{% endif %}</p>
              </div>
              <p class="overview-value">{{ sense_count }}</p>
            </div>
            <div class="overview-card">
              <div>
                <p class="overview-title">Parts of speech</p>
                {% if payload.pos_frequency.len() > 0 %}
                <div class="overview-pos-list">
                  {% for pos in payload.pos_frequency %}
                  <span class="overview-pos-chip">{{ pos.label }} ({{ pos.count }})</span>
                  {% endfor %}
                </div>
                {% else %}
                <p class="overview-detail">Part-of-speech tags not available.</p>
                {% endif %}
              </div>
            </div>
            <div class="overview-card">
              <div>
                <p class="overview-title">Encyclopedia</p>
                {% if encyclopedia_html.is_some() %}
                <p class="overview-detail">Includes a long-form article.</p>
                {% else %}
                <p class="overview-detail">No encyclopedia article available.</p>
                {% endif %}
              </div>
              {% if encyclopedia_html.is_some() %}
              <a href='#encyclopedia' class="overview-link">Jump</a>
              {% endif %}
            </div>
          </div>
        </section>

        {% if pos_chips.len() > 0 %}
        <section id="parts-of-speech">
          <h2 class="text-xl font-semibold mb-2">Parts of speech</h2>
          <div class="flex flex-wrap gap-2 d-flex">
            {% for chip in pos_chips %}
            <span class="pos-chip {{ chip.css_class }}">{{ chip.label }}</span>
            {% endfor %}
          </div>
        </section>
        {% endif %}

        <section id="senses">
          <h2 class="text-xl font-semibold mb-2">Senses ({{ sense_count }})</h2>
          <div class="space-y-4">
            {% for sense in senses %}
            <article class="bg-white shadow rounded p-4">
              <p class="text-sm text-slate-500 mb-1">
                Sense #{{ sense.payload.sense_index }}
                {% if sense.payload.part_of_speech.is_some() %}
                  • {{ sense.payload.part_of_speech.as_ref().unwrap() }}
                {% endif %}
              </p>
              <div class="font-medium mb-2 prose prose-slate max-w-none rich-text">
                {% if sense.definition_html.is_some() %}
                  {{ sense.definition_html.as_ref().unwrap()|safe }}
                {% else %}
                  <p>Definition unavailable</p>
                {% endif %}
              </div>
              {% if sense.synonyms.len() > 0 %}
              <div class="mt-3">
                <p class="font-semibold mb-1">Synonyms</p>
                <div class="relation-chip-group">
                  {% for syn in sense.synonyms %}
                  {% if syn.href.is_some() %}
                  <a href="{{ syn.href.as_ref().unwrap() }}" class="relation-chip">{{ syn.label }}</a>
                  {% else %}
                  <span class="relation-chip relation-chip-disabled">{{ syn.label }}</span>
                  {% endif %}
                  {% endfor %}
                </div>
              </div>
              {% endif %}
              {% if sense.antonyms.len() > 0 %}
              <div class="mt-3">
                <p class="font-semibold mb-1">Antonyms</p>
                <div class="relation-chip-group">
                  {% for ant in sense.antonyms %}
                  {% if ant.href.is_some() %}
                  <a href="{{ ant.href.as_ref().unwrap() }}" class="relation-chip">{{ ant.label }}</a>
                  {% else %}
                  <span class="relation-chip relation-chip-disabled">{{ ant.label }}</span>
                  {% endif %}
                  {% endfor %}
                </div>
              </div>
              {% endif %}
              {% if sense.hypernyms.len() > 0 %}
              <div class="mt-3">
                <p class="font-semibold mb-1">Hypernyms</p>
                <div class="relation-chip-group">
                  {% for hyper in sense.hypernyms %}
                  {% if hyper.href.is_some() %}
                  <a href="{{ hyper.href.as_ref().unwrap() }}" class="relation-chip">{{ hyper.label }}</a>
                  {% else %}
                  <span class="relation-chip relation-chip-disabled">{{ hyper.label }}</span>
                  {% endif %}
                  {% endfor %}
                </div>
              </div>
              {% endif %}
              {% if sense.hyponyms.len() > 0 %}
              <div class="mt-3">
                <p class="font-semibold mb-1">Hyponyms</p>
                <div class="relation-chip-group">
                  {% for hypo in sense.hyponyms %}
                  {% if hypo.href.is_some() %}
                  <a href="{{ hypo.href.as_ref().unwrap() }}" class="relation-chip">{{ hypo.label }}</a>
                  {% else %}
                  <span class="relation-chip relation-chip-disabled">{{ hypo.label }}</span>
                  {% endif %}
                  {% endfor %}
                </div>
              </div>
              {% endif %}
              {% if sense.payload.examples.len() > 0 %}
              <div class="mt-3">
                <p class="font-semibold mb-1">Examples</p>
                <ul class="list-disc pl-6 space-y-1">
                  {% for example in sense.payload.examples %}
                  <li>{{ example }}</li>
                  {% endfor %}
                </ul>
              </div>
              {% endif %}
            </article>
            {% endfor %}
          </div>
        </section>

        {% if encyclopedia_html.is_some() %}
        <section id="encyclopedia">
          <h2 class="text-xl font-semibold mb-2">Encyclopedia Entry</h2>
          <div class="bg-white shadow rounded p-4 prose prose-slate max-w-none rich-text">{{ encyclopedia_html.as_ref().unwrap()|safe }}</div>
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
    canonical_url: String,
    json_ld: SafeJson,
    encyclopedia_html: Option<String>,
    pos_chips: Vec<PosChip<'a>>,
    senses: Vec<SenseBlock<'a>>,
    sense_count: usize,
    typeahead_header: String,
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
    <script type="application/ld+json">
    {{ json_ld }}
    </script>
  </head>
  <body class="{{ chrome.body_class }}">
    <main class="{{ chrome.main_class }}">
      {{ typeahead_header|safe }}
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
    json_ld: SafeJson,
    typeahead_header: String,
}

#[derive(Template)]
#[template(
    source = r#"<!DOCTYPE html>
<html lang="en">
  <head>
    <meta charset="utf-8" />
    <meta name="viewport" content="width=device-width, initial-scale=1" />
    <title>OpenGloss • Lexeme Index</title>
    {% if chrome.use_tailwind %}
    <script src="https://cdn.jsdelivr.net/npm/@tailwindcss/browser@4"></script>
    {% endif %}
    {% if chrome.use_bootstrap %}
    <link href="https://cdn.jsdelivr.net/npm/bootstrap@5.3.8/dist/css/bootstrap.min.css" rel="stylesheet" integrity="sha384-sRIl4kxILFvY47J16cr9ZwB07vP4J8+LH7qKQnuqkuIAvNWLzeN8tE5YBujZqJLB" crossorigin="anonymous">
    <script src="https://cdn.jsdelivr.net/npm/bootstrap@5.3.8/dist/js/bootstrap.bundle.min.js" integrity="sha384-FKyoEForCGlyvwx9Hj09JcYn3nv7wiPVlz7YYwJrWVcXK/BmnVDxM+D2scQbITxI" crossorigin="anonymous"></script>
    {% endif %}
    <link rel="canonical" href="{{ base_url }}/index">
    <script type="application/ld+json">
    {{ json_ld }}
    </script>
  </head>
  <body class="{{ chrome.body_class }}">
    <main class="{{ chrome.main_class }}">
      {{ typeahead_header|safe }}
      <div class="{{ chrome.card_class }} space-y-6">
        <div>
          <p class="{{ chrome.eyebrow_class }}">Prefix depth ≤ {{ payload.letters }}</p>
          <h1 class="{{ chrome.headline_class }}">Lexeme index</h1>
          <p class="{{ chrome.lede_class }}">Browse {{ payload.total_matches }} entries{% if payload.prefix.len() > 0 %} starting with “{{ payload.prefix }}”{% endif %}. Click a prefix group to filter the word list below.</p>
        </div>

        {% for level in payload.levels %}
        <section>
          <h2 class="text-xl font-semibold mb-2">Prefixes of length {{ level.length }}</h2>
          <div class="flex flex-wrap gap-2">
            {% for pref in level.prefixes %}
            <a href="{{ pref.link }}" class="px-3 py-2 rounded border {% if pref.active %}bg-slate-900 text-white{% else %}bg-white text-slate-900{% endif %} shadow-sm hover:shadow">{{
              pref.prefix
            }} <span class="text-xs text-slate-500">({{ pref.count }})</span></a>
            {% endfor %}
          </div>
        </section>
        {% endfor %}

        <section id="words">
          <h2 class="text-xl font-semibold mb-2">Words{% if payload.prefix.len() > 0 %} matching “{{ payload.prefix }}”{% endif %}</h2>
          <p class="text-sm text-slate-500">
            Showing {{ payload.words.len() }} of {{ payload.total_matches }} results{% if payload.total_matches > payload.max_display %} (first {{ payload.max_display }} shown){% endif %}.
          </p>
          {% if payload.words.len() == 0 %}
            <p>No words matched this prefix.</p>
          {% else %}
          <div class="grid gap-2 md:grid-cols-3">
            {% for word in payload.words %}
            <a href="{{ word.href }}" class="block px-3 py-2 bg-white rounded shadow hover:shadow-md transition">
              <p class="font-semibold">{{ word.word }}</p>
              <p class="text-xs text-slate-500">Lexeme #{{ word.lexeme_id }}</p>
            </a>
            {% endfor %}
          </div>
          {% endif %}
        </section>
      </div>
    </main>
  </body>
</html>"#,
    ext = "html"
)]
struct IndexTemplate<'a> {
    chrome: Chrome,
    payload: &'a IndexPagePayload<'a>,
    json_ld: SafeJson,
    base_url: &'a str,
    typeahead_header: String,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, Default)]
#[serde(rename_all = "lowercase")]
enum SearchModeParam {
    #[default]
    Fuzzy,
    Substring,
}

impl SearchModeParam {
    fn query_value(&self) -> &'static str {
        match self {
            SearchModeParam::Fuzzy => "fuzzy",
            SearchModeParam::Substring => "substring",
        }
    }
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
    use axum::{body, body::Body, http::{header, Request}};
    use tower::ServiceExt;

    fn test_router() -> Router {
        let state = Arc::new(AppState {
            default_search: SearchConfig::default(),
            theme: WebTheme::Tailwind,
            base_url: "http://127.0.0.1:8080".to_string(),
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

    #[tokio::test]
    async fn api_typeahead_prefix() {
        let router = test_router();
        let response = router
            .oneshot(
                Request::get("/api/typeahead?q=do&mode=prefix&limit=5")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert!(response.status().is_success());
        let bytes = body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let payload: TypeaheadResponse = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(payload.query, "do");
        assert!(!payload.suggestions.is_empty());
    }

    #[tokio::test]
    async fn api_typeahead_prefix_falls_back_to_substring() {
        let router = test_router();
        // "object" does not start any lexeme directly but appears in compounds such as "3d object".
        let response = router
            .oneshot(
                Request::get("/api/typeahead?q=object&mode=prefix&limit=5")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert!(response.status().is_success());
        let bytes = body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let payload: TypeaheadResponse = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(payload.query, "object");
        assert!(
            !payload.suggestions.is_empty(),
            "substring fallback should populate suggestions when prefix misses"
        );
    }

    #[tokio::test]
    async fn index_page_renders() {
        let router = test_router();
        let response = router
            .oneshot(
                Request::get("/index?letters=2&prefix=ab")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert!(response.status().is_success());
    }

    #[tokio::test]
    async fn sitemap_index_lists_bucket_files() {
        let router = test_router();
        let response = router
            .oneshot(Request::get("/sitemap.xml").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert!(response.status().is_success());
        let bytes = body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let text = String::from_utf8(bytes.to_vec()).unwrap();
        assert!(text.contains("<sitemapindex"));
        assert!(text.contains("sitemap-a.xml"));
    }

    #[tokio::test]
    async fn sitemap_bucket_contains_words() {
        let router = test_router();
        let response = router
            .oneshot(Request::get("/sitemap-d.xml").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert!(response.status().is_success());
        let bytes = body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let text = String::from_utf8(bytes.to_vec()).unwrap();
        assert!(text.contains("/lexeme?word=dog"));
    }

    #[tokio::test]
    async fn lexeme_page_has_jsonld() {
        let router = test_router();
        let response = router
            .oneshot(
                Request::get("/lexeme?word=dog")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert!(response.status().is_success());
        let body = body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let html = String::from_utf8(body.to_vec()).unwrap();
        assert!(html.contains("application/ld+json"));
        assert!(html.contains("<section id=\"senses\">"));
    }

    #[tokio::test]
    async fn lexeme_markdown_renders_html() {
        let router = test_router();
        let response = router
            .oneshot(
                Request::get("/lexeme?word=3d")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert!(response.status().is_success());
        let body = body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let html = String::from_utf8(body.to_vec()).unwrap();
        assert!(
            html.contains("<h1"),
            "markdown content should render as HTML headings"
        );
        assert!(
            html.contains("<section id=\"senses\">"),
            "senses section should be present"
        );
        assert!(
            !html.contains("&lt;h1"),
            "markdown markup must not be HTML-escaped"
        );
    }

    #[test]
    fn render_markdown_str_allows_raw_html() {
        let html = render_markdown_str("<h2>Inline</h2>").expect("rendered");
        assert!(
            html.contains("<h2>Inline</h2>"),
            "raw HTML blocks should not be escaped"
        );
    }

    #[test]
    fn render_markdown_str_preserves_iframe_when_allowed() {
        let html = render_markdown_str("<iframe src=\"https://example.com\"></iframe>").expect("rendered");
        assert!(
            html.contains("<iframe src=\"https://example.com\"></iframe>"),
            "GFM tag filter must be disabled so embeddable HTML survives"
        );
    }

    #[tokio::test]
    async fn lexeme_route_accepts_id_path() {
        let router = test_router();
        let entry = LexemeIndex::entry_by_word("dog").expect("dog lexeme");
        let uri = format!("/lexeme/{}", entry.lexeme_id());
        let response = router
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(uri)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert!(response.status().is_success());
        let body = body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let html = String::from_utf8(body.to_vec()).unwrap();
        assert!(
            html.to_lowercase().contains("dog"),
            "lexeme page should mention the resolved word"
        );
    }

    #[tokio::test]
    async fn relation_terms_link_to_lexemes() {
        let router = test_router();
        let word = "3d object";
        let uri = format!("/lexeme?word={}", encode_component(word));
        let response = router
            .oneshot(Request::get(&uri).body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert!(response.status().is_success());
        let body = body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let html = String::from_utf8(body.to_vec()).unwrap();
        let entry = LexemeIndex::entry_by_word(word).expect("lexeme present");
        if let Some(linkable) = entry
            .all_synonyms()
            .find(|term| LexemeIndex::entry_by_word(term).is_some())
        {
            let expected_href = format!("href=\"{}\"", lexeme_path(linkable));
            assert!(
                html.contains(&expected_href),
                "synonym {linkable} should link to its lexeme page"
            );
        } else {
            assert!(
                html.contains("relation-chip-disabled"),
                "fallback chips should render when synonyms are missing"
            );
        }
    }

    #[tokio::test]
    async fn random_route_redirects_to_lexeme() {
        let router = test_router();
        let response = router
            .oneshot(Request::get("/random").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::TEMPORARY_REDIRECT);
        let location = response
            .headers()
            .get(header::LOCATION)
            .expect("redirect location header");
        let target = location.to_str().expect("valid utf-8");
        assert!(
            target.starts_with("/lexeme?word="),
            "random redirect should land on a lexeme query, got {target}"
        );
    }

    #[test]
    fn relation_links_skip_missing_words() {
        let links = relation_links(&[String::from("this-word-should-not-exist")]);
        assert_eq!(links.len(), 1);
        assert!(
            links[0].href.is_none(),
            "missing lexemes should not produce hyperlinks"
        );
    }
}
