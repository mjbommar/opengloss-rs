## Highlights
- **Telemetry + engagement**: Added an in-memory telemetry engine with session cookies, streak tracking, per-section votes, issue reports, and relation-click heatmaps. The home page now surfaces Lexeme of the Day, Seven Senses Challenge, relation puzzles, and a community pulse list driven by that data. All votes/reports trickle into `data/telemetry/telemetry-log.jsonl` so you can review them later.
- **Refined discovery UX**: Rebuilt the `/` hero into clean cards that bundle search, navigation, highlights, puzzles, and trending words. Added a `/random` endpoint, surprise button, and a friendlier copy deck so casual visitors understand the project right away.
- **Type-ahead + navigation polish**: The substring-backed trie suggestions are now wired into both the home search bar and the top nav search, defaulting to substring mode so exact matches appear instantly. Every page (except `/`) gets a compact nav bar with a home link and quick search affordance.
- **Lexeme view cleanup**: Markdown now renders without escaping, the redundant “Entry Text” block is gone, example lists use proper line spacing, parts-of-speech chips carry muted color hints, and feedback controls were slimmed down to icon-only buttons.
- **Bug fixes**: Fixed a regression where home-page HTML attributes were double-escaped (e.g., `href=\"/lexeme?...\"`), ensuring links render and behave correctly again.

## Packaging
- `scripts/package_release.sh v0.4.1`
- Upload `dist/opengloss-rs-v0.4.1-<target>.tar.zst` and its `.sha256` to the GitHub release.
