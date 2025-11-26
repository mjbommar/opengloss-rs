- [x] **Feature gates & dependencies**
  - [x] Add `web` feature pulling `axum 0.7`, `tokio`, `tower-http`, `askama`, `utoipa`, `utoipa-axum`, `utoipa-swagger-ui`, `serde`, `serde_json`, `include_dir`/`rust-embed`.
  - [x] Add `web-openapi` feature (depends on `web`) and document new flags.

- [x] **Shared web state & CLI entry**
  - [x] Introduce `AppState` (Arc) with search config defaults, caches, markdown renderer, etc.
  - [x] Extend CLI with `serve` subcommand (feature-gated) configuring bind addr, theme, cache sizes.

- [x] **HTTP server scaffolding**
  - [x] Build Axum router with `TraceLayer`, `CompressionLayer`, health endpoints, and graceful shutdown.

- [ ] **JSON/OpenAPI endpoints**
  - [x] Define DTOs (`Serialize`, `Deserialize`) and implement `/api/lexeme`, `/api/search`, and `/api/typeahead`.
  - [ ] Add graph/encyclopedia APIs and derive `ToSchema` for those payloads.
  - [ ] Generate/serve OpenAPI (`/openapi.json`, Swagger UI) behind the `web-openapi` feature flag.

- [ ] **HTML rendering pipeline**
  - [x] Add `askama` templates for home, search, lexeme, and index pages with Tailwind/Bootstrap chrome plus markdown rendering via `markdown-rs`.
  - [ ] Add templates for graph visualizations / encyclopedia explorer views.
  - [ ] Integrate richer content negotiation and theme customization (per-request CSS framework, JSON/HTML auto-switching).

- [ ] **Static assets**
  - [ ] Create `static/` directory for CSS/JS/logo; serve via `ServeDir` and embed for release builds.

- [ ] **Graph & visualization HTML**
  - [ ] Build Askama partials for neighbor trees, DOT download links, minimal JS enhancements.

- [ ] **OpenAPI + documentation**
  - [x] Document CLI, web server, and API usage in README/RELEASING.
  - [ ] Ensure OpenAPI schema coverage matches shipped endpoints; publish docs alongside releases.

- [ ] **Testing & benchmarking**
  - [x] Add integration tests for `/api/*`, `/lexeme`, `/index`, and markdown rendering.
  - [ ] Extend benches / add perf tests for trie/typeahead + web handlers if regressions appear.

- [ ] **Deployment polish**
  - [x] Update release packaging for web builds (full-feature tarball + checksum).
  - [ ] Add docker/systemd guidance and sample configs.
