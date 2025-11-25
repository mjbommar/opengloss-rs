- [x] **Feature gates & dependencies**
  - [x] Add `web` feature pulling `axum 0.7`, `tokio`, `tower-http`, `askama`, `utoipa`, `utoipa-axum`, `utoipa-swagger-ui`, `serde`, `serde_json`, `include_dir`/`rust-embed`.
  - [x] Add `web-openapi` feature (depends on `web`) and document new flags.

- [x] **Shared web state & CLI entry**
  - [x] Introduce `AppState` (Arc) with search config defaults, caches, markdown renderer, etc.
  - [x] Extend CLI with `serve` subcommand (feature-gated) configuring bind addr, theme, cache sizes.

- [x] **HTTP server scaffolding**
  - [x] Build Axum router with `TraceLayer`, `CompressionLayer`, health endpoints, and graceful shutdown.

- [ ] **JSON/OpenAPI endpoints**
  - [x] Define DTOs (`Serialize`, `Deserialize`) and implement `/api/lexeme` + `/api/search`.
  - [ ] Add graph/encyclopedia APIs and derive `ToSchema`.
  - [ ] Generate OpenAPI doc + serve `/openapi.json` + `/docs` (Swagger UI) behind `web-openapi`.

- [ ] **HTML rendering pipeline**
  - [x] Add `askama` templates for search + lexeme pages with Tailwind/Bootstrap chrome.
  - [ ] Add templates for graph/encyclopedia and markdown rendering.
  - [ ] Integrate richer content negotiation and theme customization.

- [ ] **Static assets**
  - [ ] Create `static/` directory for CSS/JS/logo; serve via `ServeDir` and embed for release builds.

- [ ] **Graph & visualization HTML**
  - [ ] Build Askama partials for neighbor trees, DOT download links, minimal JS enhancements.

- [ ] **OpenAPI + documentation**
  - [ ] Document endpoints and features in README/RELEASING; ensure schema coverage.

- [ ] **Testing & benchmarking**
  - [ ] Add integration tests for JSON/HTML responses; extend benches if needed.

- [ ] **Deployment polish**
  - [ ] Update release packaging for `web` builds; add docker/systemd guidance.
