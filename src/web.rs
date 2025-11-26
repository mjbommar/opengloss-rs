use crate::telemetry::{
    ChallengeCard, IssueKind, IssueReportRequest, LexemeFeedbackBundle, RelationPuzzle, SectionKey,
    SectionKind, SessionProgress, SpotlightLexeme, Telemetry, TrendingLexeme, VoteDirection,
    describe_ratio, generate_session_id,
};
use crate::{LexemeEntry, LexemeIndex, RelationKind, SearchConfig};
use askama::Html as HtmlEscaper;
use askama::{MarkupDisplay, Template};
use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{Html, IntoResponse, Redirect, Response},
    routing::{get, post},
};
use cookie::{Cookie, SameSite};
use markdown::{Options as MarkdownOptions, to_html_with_options};
use percent_encoding::{NON_ALPHANUMERIC, utf8_percent_encode};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::BTreeMap;
use std::fmt;
use std::net::SocketAddr;
use std::path::PathBuf;
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
    "a", "b", "c", "d", "e", "f", "g", "h", "i", "j", "k", "l", "m", "n", "o", "p", "q", "r", "s",
    "t", "u", "v", "w", "x", "y", "z", "other",
];
const TYPEAHEAD_DEFAULT_LIMIT: usize = 12;
const TYPEAHEAD_MAX_LIMIT: usize = 50;
const SESSION_COOKIE: &str = "opengloss_session";
type SafeMarkup = MarkupDisplay<HtmlEscaper, String>;
type SafeJson = SafeMarkup;

struct SessionHandle {
    id: String,
    needs_set: bool,
}

impl SessionHandle {
    fn from_headers(headers: &HeaderMap) -> Self {
        if let Some(existing) = cookie_value(headers, SESSION_COOKIE) {
            Self {
                id: existing,
                needs_set: false,
            }
        } else {
            Self {
                id: generate_session_id(),
                needs_set: true,
            }
        }
    }

    fn id(&self) -> &str {
        &self.id
    }

    fn into_response<R: IntoResponse>(self, response: R) -> Response {
        let mut response = response.into_response();
        if self.needs_set {
            if let Some(value) = build_session_cookie_header(&self.id) {
                response.headers_mut().append(header::SET_COOKIE, value);
            }
        }
        response
    }
}

struct HomeHighlights {
    spotlight: Option<SpotlightLexeme>,
    trending: Vec<TrendingLexeme>,
    challenge: Option<ChallengeCard>,
    puzzle: Option<RelationPuzzle>,
}

#[derive(Clone)]
pub struct AppState {
    pub default_search: SearchConfig,
    pub theme: WebTheme,
    pub base_url: String,
    pub telemetry: Telemetry,
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
    pub telemetry_path: Option<PathBuf>,
}

impl Default for WebConfig {
    fn default() -> Self {
        Self {
            addr: SocketAddr::from(([127, 0, 0, 1], 8080)),
            enable_openapi: true,
            theme: WebTheme::default(),
            base_url: "http://127.0.0.1:8080".to_string(),
            telemetry_path: Some(PathBuf::from("data/telemetry/telemetry-log.jsonl")),
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
    let telemetry = if let Some(path) = config.telemetry_path.clone() {
        Telemetry::persistent(path)
    } else {
        Telemetry::ephemeral()
    };
    let state = Arc::new(AppState {
        default_search: SearchConfig::default(),
        theme: config.theme,
        base_url: config.base_url.clone(),
        telemetry,
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
        .route("/api/feedback/rate", post(api_rate_section))
        .route("/api/feedback/report", post(api_report_issue))
        .route("/api/telemetry/relation-click", post(api_relation_click))
        .route("/api/analytics/trending", get(api_trending))
        .route("/api/fun/seven-senses", get(api_challenge))
        .route("/api/fun/relation-puzzle", get(api_relation_puzzle))
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

async fn home(State(state): State<SharedState>, headers: HeaderMap) -> impl IntoResponse {
    let session = SessionHandle::from_headers(&headers);
    let highlights = HomeHighlights {
        spotlight: state.telemetry.lexeme_of_the_day(),
        trending: state.telemetry.trending(6),
        challenge: state.telemetry.challenge_card(),
        puzzle: state.telemetry.relation_puzzle(),
    };
    let progress = state.telemetry.session_progress(session.id());
    let html = render_home(state.theme, &state.base_url, &highlights, progress.as_ref());
    session.into_response(Html(html))
}

async fn random_redirect() -> impl IntoResponse {
    let target = random_lexeme_path().unwrap_or_else(|| lexeme_path("encyclopedia"));
    Redirect::temporary(&target)
}

fn render_home(
    theme: WebTheme,
    base_url: &str,
    highlights: &HomeHighlights,
    progress: Option<&SessionProgress>,
) -> String {
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
    let intro = "Find kind, plain-language explanations and encyclopedia notes for more than 150,000 modern English entries.";
    let typeahead_script = TYPEAHEAD_WIDGET;
    let streak_note = progress
        .map(|p| {
            format!(
                r#"<p class="text-sm text-slate-600 mb-0">You’ve explored <strong>{today}</strong> new word{plural} today — {streak}-day streak.</p>"#,
                today = p.today_unique_words,
                plural = if p.today_unique_words == 1 { "" } else { "s" },
                streak = p.consecutive_days,
            )
        })
        .unwrap_or_default();
    let highlight_section = render_highlights_card(highlights);
    let challenge_section = render_challenge_section(highlights.challenge.as_ref());
    let trending_section = render_trending_card(&highlights.trending);
    let streak_badge = if streak_note.is_empty() {
        String::new()
    } else {
        format!(r#"<div class="rounded bg-slate-50 px-3 py-2">{streak_note}</div>"#)
    };
    let search_section = render_search_card(&chrome, intro, &streak_badge);
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
        {search_section}
        {highlight_section}
        {challenge_section}
        {trending_section}
      </div>
      <footer class="mt-12 text-center text-sm text-slate-500 space-y-2">
        <p>Need the nerdy knobs? Run the bundled CLI or call the JSON API for batch lookups—see the README for commands and endpoints.</p>
        <p class="text-xs">Type-ahead suggestions come directly from the offline trie baked into the Rust binary. Advanced clients can hit <code>/api/typeahead</code>, <code>/api/search</code>, or <code>/api/lexeme</code> for richer automation.</p>
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
        site_json_ld = indent_json(&website_json_ld(base_url), 4),
        search_section = search_section,
        highlight_section = highlight_section,
        challenge_section = challenge_section,
        trending_section = trending_section,
    )
}

fn render_search_card(chrome: &Chrome, intro: &str, streak_badge: &str) -> String {
    format!(
        r#"<section class="bg-white shadow rounded p-6 space-y-4">
          <div class="space-y-2">
            <p class="{eyebrow_class}">OpenGloss v{version}</p>
            <h1 class="{headline_class}">Your friendly dictionary, encyclopedia, and thesaurus.</h1>
            <p class="{lede_class}">{intro}</p>
          </div>
          <form action="/search" method="get" class="w-full flex flex-col gap-3" data-role="typeahead-form">
            <label class="text-sm font-semibold text-slate-600" for="home-search-input">Search the lexicon</label>
            <div class="flex flex-col md:flex-row gap-3">
              <div class="flex-1 position-relative relative">
                <input id="home-search-input" name="q" data-role="typeahead-input" placeholder="Try “solar eclipse”, “gratitude”, “geometric solid”…" class="w-full form-control px-4 py-2 rounded border border-slate-300 focus:border-slate-500 focus:ring-2 focus:ring-slate-300" autocomplete="off" />
                <div class="typeahead-panel" data-role="typeahead-panel" role="listbox" hidden></div>
              </div>
              <select name="mode" class="form-select w-full md:w-auto px-3 py-2 rounded border border-slate-300">
                <option value="substring" selected>Contains text</option>
                <option value="fuzzy">Best match</option>
              </select>
              <button type="submit" class="{button_class} w-full md:w-auto">Search</button>
            </div>
            <p id="home-search-status" data-role="typeahead-status" class="text-xs text-slate-500 mb-0"></p>
            {streak_badge}
          </form>
          <div class="flex flex-wrap gap-3">
            <a href="/lexeme?word=farm" class="{button_class}">Explore an example</a>
            <a href="/index" class="{button_class}">Browse the index</a>
            <a href="/random" class="{button_class}">Surprise me</a>
          </div>
        </section>"#,
        eyebrow_class = chrome.eyebrow_class,
        headline_class = chrome.headline_class,
        lede_class = chrome.lede_class,
        version = env!("CARGO_PKG_VERSION"),
        intro = intro,
        button_class = chrome.button_class,
        streak_badge = streak_badge,
    )
}

fn render_highlights_card(highlights: &HomeHighlights) -> String {
    let mut cards = Vec::new();
    if let Some(spotlight) = highlights.spotlight.as_ref() {
        cards.push(render_spotlight_card(spotlight));
    }
    if let Some(puzzle) = highlights.puzzle.as_ref() {
        cards.push(render_puzzle_card(puzzle));
    }
    if cards.is_empty() {
        return String::new();
    }
    format!(
        r#"<section class="bg-white shadow rounded p-6 space-y-4">
      <div class="flex items-center justify-between">
        <div>
          <p class="text-xs uppercase tracking-wide text-slate-500 mb-1">Today’s highlights</p>
          <h2 class="text-2xl font-semibold">Fresh entries to explore</h2>
        </div>
      </div>
      <div class="grid gap-4 md:grid-cols-2">{cards}</div>
    </section>"#,
        cards = cards.join("")
    )
}

fn render_challenge_section(challenge: Option<&ChallengeCard>) -> String {
    let Some(card) = challenge else {
        return String::new();
    };
    let hints = if card.hint_relations.is_empty() {
        String::from("<span class=\"text-xs text-slate-500\">Mixed relations</span>")
    } else {
        card
            .hint_relations
            .iter()
            .map(|rel| format!("<span class=\"inline-flex px-2 py-1 rounded-full bg-slate-100 text-xs text-slate-600\">{}</span>", rel.label()))
            .collect::<Vec<_>>()
            .join(" ")
    };
    let steps = card
        .path
        .iter()
        .map(|step| {
            let via = step
                .via
                .map(|rel| format!("<span class=\"text-xs text-slate-500 me-2\">{}</span>", rel.label()))
                .unwrap_or_default();
            format!(
                "<li>{via}<a href=\"{href}\" class=\"text-slate-900 hover:underline\">{label}</a></li>",
                href = lexeme_path(&step.word),
                label = xml_escape(&step.word),
            )
        })
        .collect::<Vec<_>>()
        .join("");
    format!(
        r#"<section class="bg-white shadow rounded p-6 space-y-3">
      <div class="flex flex-col gap-1">
        <p class="text-xs uppercase tracking-wide text-slate-500 mb-0">Seven Senses Challenge</p>
        <h2 class="text-2xl font-semibold">{start} → {target}</h2>
        <p class="text-sm text-slate-600 mb-0">Can you connect these lexemes in {hops} hop{plural}? Follow the relation hints, then reveal the answer.</p>
      </div>
      <div class="flex flex-wrap gap-2">{hints}</div>
      <details class="bg-slate-50 rounded p-4 text-sm">
        <summary class="cursor-pointer font-semibold">Reveal the path</summary>
        <ol class="list-decimal ps-5 space-y-1 mt-2">{steps}</ol>
      </details>
    </section>"#,
        start = xml_escape(&card.start.word),
        target = xml_escape(&card.target.word),
        hops = card.hop_count,
        plural = if card.hop_count == 1 { "" } else { "s" },
        hints = hints,
        steps = steps,
    )
}

fn render_trending_card(trending: &[TrendingLexeme]) -> String {
    let content = if trending.is_empty() {
        "<p class=\"text-sm text-slate-500 mb-0\">Peek at a few entries to seed the trending list.</p>"
            .to_string()
    } else {
        let items = trending
            .iter()
            .take(8)
            .map(|row| {
                format!(
                    "<li class=\"flex justify-between items-center\"><a href=\"{href}\" class=\"text-blue-700 hover:underline\">{word}</a><span class=\"text-xs text-slate-500\">{views} visits</span></li>",
                    href = lexeme_path(&row.word),
                    word = xml_escape(&row.word),
                    views = row.total_views,
                )
            })
            .collect::<Vec<_>>()
            .join("");
        format!("<ol class=\"space-y-1 ps-4\">{items}</ol>")
    };
    format!(
        r#"<section class="bg-white shadow rounded p-6 space-y-3">
      <div>
        <p class="text-xs uppercase tracking-wide text-slate-500 mb-1">Community pulse</p>
        <h2 class="text-2xl font-semibold">Popular words right now</h2>
      </div>
      {content}
    </section>"#,
        content = content
    )
}

fn render_spotlight_card(spot: &SpotlightLexeme) -> String {
    format!(
        r#"<article class="space-y-2">
      <p class="text-xs uppercase tracking-wide text-slate-500">Lexeme of the day</p>
      <h3 class="text-xl font-semibold"><a href="{href}" class="text-slate-900 hover:underline">{word}</a></h3>
      <p class="text-sm text-slate-600">{summary}</p>
    </article>"#,
        href = lexeme_path(&spot.word),
        word = xml_escape(&spot.word),
        summary = xml_escape(&spot.summary),
    )
}

fn render_puzzle_card(puzzle: &RelationPuzzle) -> String {
    format!(
        r#"<article class="space-y-2">
      <p class="text-xs uppercase tracking-wide text-slate-500">Relation puzzle</p>
      <h3 class="text-xl font-semibold">{word}</h3>
      <p class="text-sm text-slate-600">Find the missing {relation} that {clue}.</p>
      <details class="bg-slate-50 rounded p-3 text-sm">
        <summary class="cursor-pointer font-semibold">Reveal the answer</summary>
        <p class="mt-2">{answer}</p>
        <a href="{href}" class="inline-flex items-center text-blue-700 hover:underline text-sm">Jump to entry</a>
      </details>
    </article>"#,
        word = xml_escape(&puzzle.word),
        relation = puzzle.relation.label(),
        clue = xml_escape(&puzzle.clue),
        answer = xml_escape(&puzzle.answer),
        href = lexeme_path(&puzzle.word),
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

const FEEDBACK_WIDGET: &str = r#"
<script>
  (function() {
    const sections = document.querySelectorAll('[data-feedback-target]');
    sections.forEach((section) => {
      const buttons = section.querySelectorAll('[data-feedback-vote]');
      const status = section.querySelector('[data-feedback-status]');
      buttons.forEach((button) => {
        button.addEventListener('click', async () => {
          const direction = button.dataset.feedbackVote || 'up';
          const payload = buildPayload(section, direction);
          if (!payload) {
            setStatus(status, 'Unable to send feedback right now.');
            return;
          }
          setStatus(status, 'Sending…');
          try {
            const response = await fetch('/api/feedback/rate', {
              method: 'POST',
              headers: { 'Content-Type': 'application/json' },
              body: JSON.stringify(payload),
            });
            if (response.ok) {
              setStatus(status, 'Thanks for helping improve this entry!');
            } else {
              setStatus(status, 'Unable to save feedback right now.');
            }
          } catch (error) {
            setStatus(status, 'Unable to save feedback right now.');
          }
        });
      });
    });

    document.querySelectorAll('[data-relation-click]').forEach((link) => {
      link.addEventListener(
        'click',
        () => {
          const payload = {
            lexeme_id: Number(link.dataset.source),
            target_word: link.dataset.targetWord || '',
          };
          if (!payload.lexeme_id || !payload.target_word.trim()) {
            return;
          }
          const blob = new Blob([JSON.stringify(payload)], { type: 'application/json' });
          if (navigator.sendBeacon) {
            navigator.sendBeacon('/api/telemetry/relation-click', blob);
          } else {
            fetch('/api/telemetry/relation-click', {
              method: 'POST',
              headers: { 'Content-Type': 'application/json' },
              body: JSON.stringify(payload),
              keepalive: true,
            });
          }
        },
        { passive: true }
      );
    });

    const issueForm = document.querySelector('[data-issue-form]');
    if (issueForm) {
      const status = issueForm.querySelector('[data-issue-status]');
      issueForm.addEventListener('submit', async (event) => {
        event.preventDefault();
        const formData = new FormData(issueForm);
        const noteRaw = (formData.get('note') || '').toString().trim();
        const payload = {
          lexeme_id: Number(formData.get('lexeme_id')),
          reason: (formData.get('reason') || '').toString(),
        };
        if (noteRaw.length) {
          payload.note = noteRaw;
        }
        if (!payload.lexeme_id) {
          setStatus(status, 'Please reload and try again.');
          return;
        }
        setStatus(status, 'Sending…');
        try {
          const response = await fetch('/api/feedback/report', {
            method: 'POST',
            headers: { 'Content-Type': 'application/json' },
            body: JSON.stringify(payload),
          });
          if (response.ok) {
            issueForm.reset();
            setStatus(status, 'Thanks! Your note is in the review queue.');
          } else {
            setStatus(status, 'Unable to send report right now.');
          }
        } catch (error) {
          setStatus(status, 'Unable to send report right now.');
        }
      });
    }

    function buildPayload(section, vote) {
      const lexemeId = Number(section.dataset.lexemeId);
      if (!lexemeId) return null;
      const kind = section.dataset.feedbackKind;
      const senseIndex = Number(section.dataset.senseIndex);
      const relationKind = section.dataset.relationKind;
      let target = null;
      if (kind === 'sense-definition' && Number.isFinite(senseIndex)) {
        target = { type: 'sense_definition', sense_index: senseIndex };
      } else if (kind === 'sense-relations' && Number.isFinite(senseIndex) && relationKind) {
        target = { type: 'sense_relations', sense_index: senseIndex, relation: relationKind };
      } else if (kind === 'encyclopedia') {
        target = { type: 'encyclopedia' };
      }
      if (!target) return null;
      return {
        lexeme_id: lexemeId,
        vote: vote === 'down' ? 'down' : 'up',
        target,
      };
    }

    function setStatus(node, text) {
      if (node) {
        node.textContent = text || '';
      }
    }
  })();
</script>
"#;

async fn health() -> impl IntoResponse {
    Json(json!({ "status": "ok", "service": "opengloss-web" }))
}

async fn lexeme_html(
    State(state): State<SharedState>,
    headers: HeaderMap,
    Query(params): Query<LexemeParams>,
) -> impl IntoResponse {
    let session = SessionHandle::from_headers(&headers);
    let html = lexeme_html_inner(state, session.id(), params).await;
    session.into_response(html)
}

async fn lexeme_html_by_id(
    State(state): State<SharedState>,
    headers: HeaderMap,
    Path(id): Path<u32>,
) -> impl IntoResponse {
    let params = LexemeParams {
        word: None,
        id: Some(id),
    };
    let session = SessionHandle::from_headers(&headers);
    let html = lexeme_html_inner(state, session.id(), params).await;
    session.into_response(html)
}

async fn lexeme_html_inner(
    state: SharedState,
    session_id: &str,
    params: LexemeParams,
) -> Html<String> {
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
            let sense_count = payload.senses.len();
            let feedback = state.telemetry.lexeme_feedback_bundle(entry.lexeme_id());
            let relation_heatmap = state
                .telemetry
                .relation_heatmap(entry.lexeme_id(), 6)
                .into_iter()
                .map(|row| RelationHeatmapRow {
                    label: row.target_word.clone(),
                    href: LexemeIndex::entry_by_word(&row.target_word)
                        .map(|_| lexeme_path(&row.target_word)),
                    count: row.count,
                })
                .collect();
            let session_progress = state
                .telemetry
                .record_lexeme_view(entry.lexeme_id(), session_id);
            let encyclopedia_confidence = feedback
                .encyclopedia
                .as_ref()
                .and_then(|summary| describe_ratio(summary, "for this encyclopedia entry"));
            let senses = payload
                .senses
                .iter()
                .map(|sense| build_sense_block(sense, &feedback))
                .collect();
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
                session_progress: Some(session_progress),
                encyclopedia_confidence,
                relation_heatmap,
                feedback_script: FEEDBACK_WIDGET,
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
    headers: HeaderMap,
    Query(params): Query<SearchParams>,
) -> impl IntoResponse {
    let session = SessionHandle::from_headers(&headers);
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
            let html = template
                .render()
                .unwrap_or_else(|err| render_error_page(state.theme, err.to_string()));
            session.into_response(Html(html))
        }
        Err(err) => {
            let html = render_error_page(state.theme, err.message);
            session.into_response(Html(html))
        }
    }
}

async fn prefix_index_html(
    State(state): State<SharedState>,
    headers: HeaderMap,
    Query(params): Query<IndexParams>,
) -> impl IntoResponse {
    let session = SessionHandle::from_headers(&headers);
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
    let html = template
        .render()
        .unwrap_or_else(|err| render_error_page(state.theme, err.to_string()));
    session.into_response(Html(html))
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

async fn api_rate_section(
    State(state): State<SharedState>,
    Json(payload): Json<RateSectionPayload>,
) -> Result<Json<RateSectionResponse>, ApiError> {
    let section = payload.target.into_section_kind();
    let summary = state
        .telemetry
        .record_section_vote(SectionKey::new(payload.lexeme_id, section), payload.vote);
    Ok(Json(RateSectionResponse {
        up: summary.up,
        down: summary.down,
        total: summary.total(),
        confidence: summary.confidence_ratio(),
    }))
}

async fn api_report_issue(
    State(state): State<SharedState>,
    headers: HeaderMap,
    Json(payload): Json<IssueReportPayload>,
) -> Result<Response, ApiError> {
    let session = SessionHandle::from_headers(&headers);
    let lexeme_id = payload
        .lexeme_id
        .ok_or_else(|| ApiError::bad_request("lexeme_id is required"))?;
    let report = state.telemetry.record_issue(IssueReportRequest {
        lexeme_id: Some(lexeme_id),
        section: payload.target.map(|target| target.into_section_kind()),
        reason: payload.reason,
        note: payload.note,
        session_id: Some(session.id().to_string()),
    });
    let body = Json(IssueReportResponse {
        id: report.id,
        queued: true,
    });
    Ok(session.into_response(body))
}

async fn api_relation_click(
    State(state): State<SharedState>,
    Json(payload): Json<RelationClickPayload>,
) -> impl IntoResponse {
    let target = payload.target_word.trim();
    if !target.is_empty() {
        state
            .telemetry
            .record_relation_click(payload.lexeme_id, target);
    }
    StatusCode::NO_CONTENT
}

async fn api_trending(State(state): State<SharedState>) -> impl IntoResponse {
    let entries = state.telemetry.trending(12);
    Json(TrendingResponse {
        generated_at: unix_seconds(),
        entries,
    })
}

async fn api_challenge(State(state): State<SharedState>) -> impl IntoResponse {
    Json(ChallengeResponse {
        challenge: state.telemetry.challenge_card(),
    })
}

async fn api_relation_puzzle(State(state): State<SharedState>) -> impl IntoResponse {
    Json(PuzzleResponse {
        puzzle: state.telemetry.relation_puzzle(),
    })
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
    definition_confidence: Option<String>,
    relation_groups: Vec<RelationGroup>,
}

struct RelationGroup {
    title: &'static str,
    title_lower: String,
    kind: RelationKind,
    links: Vec<RelationLink>,
    confidence: Option<String>,
}

#[derive(Debug, Clone)]
struct RelationLink {
    label: String,
    href: Option<String>,
}

struct RelationHeatmapRow {
    label: String,
    href: Option<String>,
    count: u64,
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

#[derive(Debug, Deserialize)]
struct RateSectionPayload {
    lexeme_id: u32,
    target: FeedbackTargetPayload,
    vote: VoteDirection,
}

#[derive(Debug, Serialize)]
struct RateSectionResponse {
    up: u64,
    down: u64,
    total: u64,
    confidence: Option<f32>,
}

#[derive(Debug, Deserialize)]
struct IssueReportPayload {
    lexeme_id: Option<u32>,
    target: Option<FeedbackTargetPayload>,
    reason: IssueKind,
    note: Option<String>,
}

#[derive(Debug, Serialize)]
struct IssueReportResponse {
    id: u64,
    queued: bool,
}

#[derive(Debug, Deserialize)]
struct RelationClickPayload {
    lexeme_id: u32,
    target_word: String,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum FeedbackTargetPayload {
    SenseDefinition {
        sense_index: i32,
    },
    SenseRelations {
        sense_index: i32,
        relation: RelationKindParam,
    },
    Encyclopedia,
}

#[derive(Debug, Deserialize, Copy, Clone)]
#[serde(rename_all = "lowercase")]
enum RelationKindParam {
    Synonym,
    Antonym,
    Hypernym,
    Hyponym,
}

impl From<RelationKindParam> for RelationKind {
    fn from(value: RelationKindParam) -> Self {
        match value {
            RelationKindParam::Synonym => RelationKind::Synonym,
            RelationKindParam::Antonym => RelationKind::Antonym,
            RelationKindParam::Hypernym => RelationKind::Hypernym,
            RelationKindParam::Hyponym => RelationKind::Hyponym,
        }
    }
}

impl FeedbackTargetPayload {
    fn into_section_kind(self) -> SectionKind {
        match self {
            FeedbackTargetPayload::SenseDefinition { sense_index } => {
                SectionKind::SenseDefinition { sense_index }
            }
            FeedbackTargetPayload::SenseRelations {
                sense_index,
                relation,
            } => SectionKind::SenseRelations {
                sense_index,
                relation: relation.into(),
            },
            FeedbackTargetPayload::Encyclopedia => SectionKind::Encyclopedia,
        }
    }
}

#[derive(Debug, Serialize)]
struct TrendingResponse {
    generated_at: u64,
    entries: Vec<TrendingLexeme>,
}

#[derive(Debug, Serialize)]
struct ChallengeResponse {
    challenge: Option<ChallengeCard>,
}

#[derive(Debug, Serialize)]
struct PuzzleResponse {
    puzzle: Option<RelationPuzzle>,
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

fn cookie_value(headers: &HeaderMap, name: &str) -> Option<String> {
    for value in headers.get_all(header::COOKIE).iter() {
        if let Ok(text) = value.to_str() {
            for pair in text.split(';') {
                let mut parts = pair.trim().splitn(2, '=');
                let key = parts.next()?.trim();
                if key == name {
                    if let Some(val) = parts.next() {
                        return Some(val.trim().to_string());
                    }
                }
            }
        }
    }
    None
}

fn build_session_cookie_header(id: &str) -> Option<HeaderValue> {
    let cookie = Cookie::build((SESSION_COOKIE, id.to_string()))
        .path("/")
        .http_only(true)
        .same_site(SameSite::Lax)
        .build();
    HeaderValue::from_str(&cookie.to_string()).ok()
}

fn unix_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
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
            <option value="substring" selected>Contains text</option>
            <option value="fuzzy">Best match</option>
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
            let href = LexemeIndex::entry_by_word(term).map(|_| lexeme_path(term));
            RelationLink {
                label: term.clone(),
                href,
            }
        })
        .collect()
}

fn build_sense_block<'a>(
    sense: &'a SensePayload,
    feedback: &LexemeFeedbackBundle,
) -> SenseBlock<'a> {
    let definition_html = render_markdown(sense.definition.as_deref());
    let definition_confidence = feedback
        .definitions
        .get(&sense.sense_index)
        .and_then(|summary| describe_ratio(summary, "for this definition"));
    let mut relation_groups = Vec::new();
    if let Some(group) = relation_group(
        "Synonyms",
        RelationKind::Synonym,
        &sense.synonyms,
        sense.sense_index,
        feedback,
    ) {
        relation_groups.push(group);
    }
    if let Some(group) = relation_group(
        "Antonyms",
        RelationKind::Antonym,
        &sense.antonyms,
        sense.sense_index,
        feedback,
    ) {
        relation_groups.push(group);
    }
    if let Some(group) = relation_group(
        "Hypernyms",
        RelationKind::Hypernym,
        &sense.hypernyms,
        sense.sense_index,
        feedback,
    ) {
        relation_groups.push(group);
    }
    if let Some(group) = relation_group(
        "Hyponyms",
        RelationKind::Hyponym,
        &sense.hyponyms,
        sense.sense_index,
        feedback,
    ) {
        relation_groups.push(group);
    }

    SenseBlock {
        payload: sense,
        definition_html,
        definition_confidence,
        relation_groups,
    }
}

fn relation_group(
    title: &'static str,
    kind: RelationKind,
    terms: &[String],
    sense_index: i32,
    feedback: &LexemeFeedbackBundle,
) -> Option<RelationGroup> {
    if terms.is_empty() {
        return None;
    }
    let confidence = feedback
        .relations
        .get(&(sense_index, kind))
        .and_then(|summary| {
            let subject = match kind {
                RelationKind::Synonym => "for these synonyms",
                RelationKind::Antonym => "for these antonyms",
                RelationKind::Hypernym => "for these hypernyms",
                RelationKind::Hyponym => "for these hyponyms",
            };
            describe_ratio(summary, subject)
        });
    Some(RelationGroup {
        title,
        title_lower: title.to_lowercase(),
        kind,
        links: relation_links(terms),
        confidence,
    })
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
      .feedback-row {
        margin-top: 0.5rem;
        padding-top: 0.5rem;
        border-top: 1px dashed rgba(15, 23, 42, 0.15);
        display: flex;
        flex-direction: column;
        gap: 0.35rem;
      }
      .feedback-buttons {
        display: inline-flex;
        gap: 0.4rem;
        flex-wrap: wrap;
      }
      .feedback-button {
        width: 2rem;
        height: 2rem;
        border-radius: 999px;
        border: 1px solid rgba(15, 23, 42, 0.25);
        background: rgba(15, 23, 42, 0.02);
        display: inline-flex;
        align-items: center;
        justify-content: center;
        font-size: 0.95rem;
        cursor: pointer;
        transition: border-color 120ms ease, background-color 120ms ease;
      }
      .feedback-button:hover {
        border-color: rgba(15, 23, 42, 0.45);
        background-color: rgba(15, 23, 42, 0.06);
      }
      .confidence-pill {
        display: inline-flex;
        align-items: center;
        padding: 0.2rem 0.8rem;
        border-radius: 999px;
        background-color: rgba(34, 197, 94, 0.12);
        color: #15803d;
        font-size: 0.75rem;
        font-weight: 600;
        width: fit-content;
      }
      .heatmap-list {
        list-style: none;
        padding: 0;
        margin: 0;
        display: flex;
        flex-direction: column;
        gap: 0.4rem;
      }
      .heatmap-list li {
        display: flex;
        justify-content: space-between;
        align-items: center;
        padding: 0.4rem 0.2rem;
        border-bottom: 1px dashed rgba(15, 23, 42, 0.08);
      }
      .issue-form {
        display: flex;
        flex-direction: column;
        gap: 0.6rem;
      }
      .issue-form textarea,
      .issue-form select {
        width: 100%;
        border: 1px solid rgba(15, 23, 42, 0.15);
        border-radius: 0.5rem;
        padding: 0.5rem 0.75rem;
        font-size: 0.9rem;
      }
      .issue-form button {
        align-self: flex-start;
        border-radius: 999px;
        background-color: #0f172a;
        color: white;
        font-weight: 600;
        padding: 0.45rem 1.2rem;
        border: none;
        cursor: pointer;
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
          <div class="grid gap-3 md:grid-cols-2 lg:grid-cols-3 xl:grid-cols-4 overview-grid">
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
                {% if encyclopedia_confidence.is_some() %}
                <span class="confidence-pill">{{ encyclopedia_confidence.as_ref().unwrap() }}</span>
                {% endif %}
                {% else %}
                <p class="overview-detail">No encyclopedia article available.</p>
                {% endif %}
              </div>
              {% if encyclopedia_html.is_some() %}
              <a href='#encyclopedia' class="overview-link">Jump</a>
              {% endif %}
            </div>
            {% if session_progress.is_some() %}
            <div class="overview-card">
              <div>
                <p class="overview-title">Your streak</p>
                <p class="overview-detail">{{ session_progress.as_ref().unwrap().consecutive_days }}-day streak</p>
                <p class="overview-detail text-xs text-slate-500">{{ session_progress.as_ref().unwrap().total_unique_words }} total words explored</p>
              </div>
              <p class="overview-value">{{ session_progress.as_ref().unwrap().today_unique_words }}</p>
            </div>
            {% endif %}
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
              <div class="feedback-row" data-feedback-target data-feedback-kind="sense-definition" data-lexeme-id="{{ payload.lexeme_id }}" data-sense-index="{{ sense.payload.sense_index }}">
                <p class="text-xs uppercase tracking-wide text-slate-500">Was this definition helpful?</p>
                <div class="feedback-buttons">
                  <button type="button" class="feedback-button" data-feedback-vote="up" aria-label="Mark this definition helpful" title="Mark this definition helpful">👍</button>
                  <button type="button" class="feedback-button" data-feedback-vote="down" aria-label="Flag this definition" title="Flag this definition">👎</button>
                </div>
                {% if sense.definition_confidence.is_some() %}
                <span class="confidence-pill">{{ sense.definition_confidence.as_ref().unwrap() }}</span>
                {% endif %}
                <p class="text-xs text-slate-500 feedback-status" data-feedback-status></p>
              </div>
              {% for group in sense.relation_groups %}
              <div class="mt-3">
                <div class="d-flex flex-column flex-md-row justify-content-between align-items-center gap-2">
                  <p class="font-semibold mb-0">{{ group.title }}</p>
                  {% if group.confidence.is_some() %}
                  <span class="confidence-pill">{{ group.confidence.as_ref().unwrap() }}</span>
                  {% endif %}
                </div>
                <div class="relation-chip-group mt-2">
                  {% for rel in group.links %}
                  {% if rel.href.is_some() %}
                  <a href="{{ rel.href.as_ref().unwrap() }}" class="relation-chip" data-relation-click data-source="{{ payload.lexeme_id }}" data-target-word="{{ rel.label }}">{{ rel.label }}</a>
                  {% else %}
                  <span class="relation-chip relation-chip-disabled">{{ rel.label }}</span>
                  {% endif %}
                  {% endfor %}
                </div>
                <div class="feedback-row" data-feedback-target data-feedback-kind="sense-relations" data-lexeme-id="{{ payload.lexeme_id }}" data-sense-index="{{ sense.payload.sense_index }}" data-relation-kind="{{ group.kind.label() }}">
                  <p class="text-xs uppercase tracking-wide text-slate-500">Are these {{ group.title_lower }} useful?</p>
                  <div class="feedback-buttons">
                    <button type="button" class="feedback-button" data-feedback-vote="up" aria-label="Mark these relations helpful" title="Mark these relations helpful">👍</button>
                    <button type="button" class="feedback-button" data-feedback-vote="down" aria-label="Flag these relations" title="Flag these relations">👎</button>
                  </div>
                  <p class="text-xs text-slate-500 feedback-status" data-feedback-status></p>
                </div>
              </div>
              {% endfor %}
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

        {% if relation_heatmap.len() > 0 %}
        <section id="community">
          <h2 class="text-xl font-semibold mb-2">Community explorer</h2>
          <p class="text-sm text-slate-600 mb-2">Readers who opened this entry also clicked:</p>
          <ul class="heatmap-list">
            {% for row in relation_heatmap %}
            <li>
              {% if row.href.is_some() %}
              <a href="{{ row.href.as_ref().unwrap() }}" class="text-blue-700 hover:underline">{{ row.label }}</a>
              {% else %}
              <span>{{ row.label }}</span>
              {% endif %}
              <span class="text-xs text-slate-500">{{ row.count }} jumps</span>
            </li>
            {% endfor %}
          </ul>
        </section>
        {% endif %}

        <section id="quality">
          <h2 class="text-xl font-semibold mb-2">Quality &amp; feedback</h2>
          <p class="text-sm text-slate-600">Rate specific sections or flag anything that feels off. We review every note.</p>
          <form class="issue-form" data-issue-form>
            <input type="hidden" name="lexeme_id" value="{{ payload.lexeme_id }}" />
            <label class="text-sm text-slate-600" for="issue-reason">What should we look at?</label>
            <select name="reason" id="issue-reason">
              <option value="duplicate_word">Duplicate word</option>
              <option value="offensive_content">Offensive content</option>
              <option value="broken_relation">Broken relation</option>
              <option value="formatting_issue">Formatting issue</option>
              <option value="other">Other</option>
            </select>
            <label class="text-sm text-slate-600" for="issue-note">Details (optional)</label>
            <textarea id="issue-note" name="note" rows="3" placeholder="Tell us what you noticed…"></textarea>
            <button type="submit">Send report</button>
            <p class="text-xs text-slate-500 feedback-status" data-issue-status></p>
          </form>
        </section>

        {% if encyclopedia_html.is_some() %}
        <section id="encyclopedia">
          <h2 class="text-xl font-semibold mb-2">Encyclopedia Entry</h2>
          <div class="bg-white shadow rounded p-4 prose prose-slate max-w-none rich-text">{{ encyclopedia_html.as_ref().unwrap()|safe }}</div>
          <div class="feedback-row" data-feedback-target data-feedback-kind="encyclopedia" data-lexeme-id="{{ payload.lexeme_id }}">
            <p class="text-xs uppercase tracking-wide text-slate-500">Is this article helpful?</p>
            <div class="feedback-buttons">
              <button type="button" class="feedback-button" data-feedback-vote="up" aria-label="Mark this article helpful" title="Mark this article helpful">👍</button>
              <button type="button" class="feedback-button" data-feedback-vote="down" aria-label="Flag this article" title="Flag this article">👎</button>
            </div>
            {% if encyclopedia_confidence.is_some() %}
            <span class="confidence-pill">{{ encyclopedia_confidence.as_ref().unwrap() }}</span>
            {% endif %}
            <p class="text-xs text-slate-500 feedback-status" data-feedback-status></p>
          </div>
        </section>
        {% endif %}
      </div>
    </main>
    {{ feedback_script|safe }}
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
    session_progress: Option<SessionProgress>,
    encyclopedia_confidence: Option<String>,
    relation_heatmap: Vec<RelationHeatmapRow>,
    feedback_script: &'static str,
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
    Substring,
    Fuzzy,
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
    use axum::{
        body,
        body::Body,
        http::{Request, header},
    };
    use tower::ServiceExt;

    fn test_router() -> Router {
        let state = Arc::new(AppState {
            default_search: SearchConfig::default(),
            theme: WebTheme::Tailwind,
            base_url: "http://127.0.0.1:8080".to_string(),
            telemetry: Telemetry::ephemeral(),
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
            .oneshot(Request::get("/lexeme?word=3d").body(Body::empty()).unwrap())
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
        let html =
            render_markdown_str("<iframe src=\"https://example.com\"></iframe>").expect("rendered");
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
