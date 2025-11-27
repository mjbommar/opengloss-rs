use crate::{GraphOptions, LexemeEntry, LexemeIndex, RelationKind};
use parking_lot::RwLock;
use rand::{Rng, SeedableRng, distributions::Alphanumeric, rngs::SmallRng, thread_rng};
use serde::{Deserialize, Serialize};
use std::cmp::Ordering;
use std::collections::{HashMap, HashSet, VecDeque};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering as AtomicOrdering};
use std::time::{SystemTime, UNIX_EPOCH};
use tracing::warn;

const SNAPSHOT_INTERVAL_SECS: u64 = 300;
const SNAPSHOT_RANDOM_CHANCE: f64 = 0.1;
const MAX_SESSION_COUNT: usize = 4096;
const MAX_SESSION_TRACKED_DAILY: usize = 2048;
const MAX_SESSION_TRACKED_TOTALS: usize = 4096;
const MAX_ISSUE_RECORDS: usize = 250;
const MAX_RELATION_CLICK_RECORDS: usize = 10_000;
const MIN_CONFIDENCE_VOTES: u64 = 5;
const MAX_CHALLENGE_ATTEMPTS: usize = 8;
const DEFAULT_CHALLENGE_DEPTH: usize = 4;

#[derive(Clone)]
pub struct Telemetry {
    shared: Arc<TelemetryShared>,
}

impl Telemetry {
    pub fn persistent(path: impl Into<PathBuf>) -> Self {
        Self::with_path(Some(path.into()))
    }

    pub fn ephemeral() -> Self {
        Self::with_path(None)
    }

    fn with_path(path: Option<PathBuf>) -> Self {
        Self {
            shared: Arc::new(TelemetryShared {
                inner: RwLock::new(TelemetryData::default()),
                persistence: TelemetryPersistence::new(path),
            }),
        }
    }

    pub fn record_lexeme_view(&self, lexeme_id: u32, session_id: &str) -> SessionProgress {
        let now = now_ts();
        let mut guard = self.shared.inner.write();
        let progress = guard.record_lexeme_view(lexeme_id, session_id, now);
        let should_snapshot = self.shared.persistence.should_snapshot();
        let snapshot = if should_snapshot {
            Some(guard.snapshot())
        } else {
            None
        };
        drop(guard);
        if let Some(snapshot) = snapshot {
            self.shared.persistence.write_snapshot(snapshot);
        }
        progress
    }

    pub fn session_progress(&self, session_id: &str) -> Option<SessionProgress> {
        let guard = self.shared.inner.read();
        guard
            .sessions
            .get(session_id)
            .map(SessionStats::as_progress)
    }

    pub fn record_section_vote(
        &self,
        section: SectionKey,
        direction: VoteDirection,
    ) -> SectionVoteSummary {
        let mut guard = self.shared.inner.write();
        let summary = guard.record_vote(section, direction, now_ts());
        summary
    }

    pub fn record_issue(&self, request: IssueReportRequest) -> IssueReport {
        let mut guard = self.shared.inner.write();
        guard.record_issue(request, now_ts())
    }

    pub fn record_relation_click(&self, lexeme_id: u32, target_word: &str) {
        let mut guard = self.shared.inner.write();
        guard.record_relation_click(lexeme_id, target_word, now_ts());
    }

    pub fn lexeme_feedback_bundle(&self, lexeme_id: u32) -> LexemeFeedbackBundle {
        let guard = self.shared.inner.read();
        guard.feedback_bundle(lexeme_id)
    }

    pub fn relation_heatmap(&self, lexeme_id: u32, limit: usize) -> Vec<RelationClickStat> {
        let guard = self.shared.inner.read();
        guard.relation_heatmap(lexeme_id, limit)
    }

    pub fn trending(&self, limit: usize) -> Vec<TrendingLexeme> {
        let guard = self.shared.inner.read();
        let mut rows: Vec<_> = guard
            .lexeme_views
            .iter()
            .map(|(&lexeme_id, stats)| TrendingCandidate {
                lexeme_id,
                score: stats.rolling_score,
                total: stats.total_views,
            })
            .collect();
        drop(guard);
        rows.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(Ordering::Equal)
                .then_with(|| b.total.cmp(&a.total))
        });
        rows.into_iter()
            .filter_map(|candidate| {
                LexemeIndex::entry_by_id(candidate.lexeme_id).map(|entry| TrendingLexeme {
                    lexeme_id: candidate.lexeme_id,
                    word: entry.word().to_string(),
                    total_views: candidate.total,
                    trend_score: candidate.score,
                })
            })
            .take(limit)
            .collect()
    }

    pub fn lexeme_of_the_day(&self) -> Option<SpotlightLexeme> {
        let words = LexemeIndex::all_words();
        if words.is_empty() {
            return None;
        }
        let day = day_code(now_ts()) as usize;
        let index = day % words.len();
        let (word, lexeme_id) = &words[index];
        LexemeIndex::entry_by_id(*lexeme_id).map(|entry| SpotlightLexeme {
            lexeme_id: *lexeme_id,
            word: word.clone(),
            summary: entry
                .all_definitions()
                .next()
                .map(|s| s.to_string())
                .or_else(|| entry.encyclopedia_entry().map(|text| snippet(&text, 220)))
                .unwrap_or_else(|| {
                    "Jump in to explore definitions, relations, and encyclopedia notes.".to_string()
                }),
        })
    }

    pub fn challenge_card(&self) -> Option<ChallengeCard> {
        let words = LexemeIndex::all_words();
        if words.is_empty() {
            return None;
        }
        let mut rng = thread_rng();
        for _ in 0..MAX_CHALLENGE_ATTEMPTS {
            let lexeme_id = words[rng.gen_range(0..words.len())].1;
            let traversal = LexemeIndex::traverse_graph(
                lexeme_id,
                &GraphOptions {
                    max_depth: DEFAULT_CHALLENGE_DEPTH,
                    max_nodes: 256,
                    max_edges: 512,
                    relations: Vec::new(),
                },
            )?;
            if traversal.nodes.len() < 2 {
                continue;
            }
            if let Some(card) = build_challenge(&traversal) {
                if card.hop_count > 1 && challenge_is_noun_only(&card) {
                    return Some(card);
                }
            }
        }
        None
    }

    pub fn relation_puzzle(&self) -> Option<RelationPuzzle> {
        let words = LexemeIndex::all_words();
        if words.is_empty() {
            return None;
        }
        let mut rng = thread_rng();
        for _ in 0..MAX_CHALLENGE_ATTEMPTS {
            let (_, lexeme_id) = words[rng.gen_range(0..words.len())].clone();
            let entry = LexemeIndex::entry_by_id(lexeme_id)?;
            if let Some(puzzle) = build_relation_puzzle(&entry) {
                return Some(puzzle);
            }
        }
        None
    }
}

struct TelemetryShared {
    inner: RwLock<TelemetryData>,
    persistence: TelemetryPersistence,
}

#[derive(Default)]
struct TelemetryData {
    lexeme_views: HashMap<u32, LexemeViewStats>,
    section_votes: HashMap<SectionKey, VoteStats>,
    issue_reports: VecDeque<IssueReport>,
    relation_clicks: HashMap<RelationClickKey, RelationClickStats>,
    sessions: HashMap<String, SessionStats>,
    next_issue_id: u64,
}

impl TelemetryData {
    fn record_lexeme_view(
        &mut self,
        lexeme_id: u32,
        session_id: &str,
        now: u64,
    ) -> SessionProgress {
        let stats = self
            .lexeme_views
            .entry(lexeme_id)
            .or_insert_with(LexemeViewStats::default);
        stats.total_views = stats.total_views.saturating_add(1);
        stats.last_view_ts = now;
        stats.rolling_score = stats.rolling_score * 0.92 + 1.0;

        if self.sessions.len() >= MAX_SESSION_COUNT && !self.sessions.contains_key(session_id) {
            if let Some(oldest) = oldest_session_key(&self.sessions) {
                self.sessions.remove(&oldest);
            }
        }

        let entry = self
            .sessions
            .entry(session_id.to_string())
            .or_insert_with(|| SessionStats::new(now));
        entry.mark_visit(now, lexeme_id);
        entry.as_progress()
    }

    fn record_vote(
        &mut self,
        section: SectionKey,
        direction: VoteDirection,
        now: u64,
    ) -> SectionVoteSummary {
        let stats = self
            .section_votes
            .entry(section)
            .or_insert_with(VoteStats::default);
        match direction {
            VoteDirection::Up => stats.up = stats.up.saturating_add(1),
            VoteDirection::Down => stats.down = stats.down.saturating_add(1),
        }
        stats.last_vote_ts = now;
        stats.as_summary()
    }

    fn record_issue(&mut self, request: IssueReportRequest, now: u64) -> IssueReport {
        let id = self.next_issue_id;
        self.next_issue_id = self.next_issue_id.saturating_add(1);
        let report = IssueReport {
            id,
            lexeme_id: request.lexeme_id,
            section: request.section,
            reason: request.reason,
            note: request.note,
            session_id: request.session_id,
            created_at: now,
        };
        self.issue_reports.push_back(report.clone());
        while self.issue_reports.len() > MAX_ISSUE_RECORDS {
            self.issue_reports.pop_front();
        }
        report
    }

    fn record_relation_click(&mut self, lexeme_id: u32, target_word: &str, now: u64) {
        if target_word.is_empty() {
            return;
        }
        let key = RelationClickKey {
            source_lexeme: lexeme_id,
            target_word: target_word.to_string(),
        };
        let stats = self
            .relation_clicks
            .entry(key)
            .or_insert_with(RelationClickStats::default);
        stats.count = stats.count.saturating_add(1);
        stats.last_clicked_ts = now;
        if self.relation_clicks.len() > MAX_RELATION_CLICK_RECORDS {
            prune_relation_clicks(&mut self.relation_clicks);
        }
    }

    fn feedback_bundle(&self, lexeme_id: u32) -> LexemeFeedbackBundle {
        let mut definitions = HashMap::new();
        let mut relations = HashMap::new();
        let mut encyclopedia = None;
        for (key, stats) in &self.section_votes {
            if key.lexeme_id != lexeme_id {
                continue;
            }
            match &key.kind {
                SectionKind::SenseDefinition { sense_index } => {
                    definitions.insert(*sense_index, stats.as_summary());
                }
                SectionKind::SenseRelations {
                    sense_index,
                    relation,
                } => {
                    relations.insert((*sense_index, *relation), stats.as_summary());
                }
                SectionKind::Encyclopedia => {
                    encyclopedia = Some(stats.as_summary());
                }
            }
        }
        LexemeFeedbackBundle {
            definitions,
            relations,
            encyclopedia,
        }
    }

    fn relation_heatmap(&self, lexeme_id: u32, limit: usize) -> Vec<RelationClickStat> {
        let mut rows: Vec<_> = self
            .relation_clicks
            .iter()
            .filter(|(key, _)| key.source_lexeme == lexeme_id)
            .map(|(key, stats)| RelationClickStat {
                target_word: key.target_word.clone(),
                count: stats.count,
            })
            .collect();
        rows.sort_by(|a, b| {
            b.count
                .cmp(&a.count)
                .then_with(|| a.target_word.cmp(&b.target_word))
        });
        rows.truncate(limit);
        rows
    }

    fn snapshot(&self) -> TelemetrySnapshot {
        TelemetrySnapshot {
            captured_at: now_ts(),
            lexeme_views: self
                .lexeme_views
                .iter()
                .map(|(&lexeme_id, stats)| LexemeViewSnapshot {
                    lexeme_id,
                    total_views: stats.total_views,
                    rolling_score: stats.rolling_score,
                    last_view_ts: stats.last_view_ts,
                })
                .collect(),
            section_votes: self
                .section_votes
                .iter()
                .map(|(key, stats)| SectionVoteSnapshot {
                    lexeme_id: key.lexeme_id,
                    section: key.kind.clone(),
                    up: stats.up,
                    down: stats.down,
                    last_vote_ts: stats.last_vote_ts,
                })
                .collect(),
            issues: self.issue_reports.iter().cloned().collect(),
            relation_clicks: self
                .relation_clicks
                .iter()
                .map(|(key, stats)| RelationClickSnapshot {
                    lexeme_id: key.source_lexeme,
                    target_word: key.target_word.clone(),
                    count: stats.count,
                    last_clicked_ts: stats.last_clicked_ts,
                })
                .collect(),
            sessions: self
                .sessions
                .iter()
                .map(|(session_id, stats)| SessionSnapshot {
                    session_id: session_id.clone(),
                    today_unique: stats.today_unique_count(),
                    total_unique: stats.total_unique_count,
                    consecutive_days: stats.consecutive_days,
                })
                .collect(),
        }
    }
}

#[derive(Default, Clone, Serialize)]
struct LexemeViewStats {
    total_views: u64,
    rolling_score: f64,
    last_view_ts: u64,
}

#[derive(Default, Clone, Serialize)]
struct VoteStats {
    up: u64,
    down: u64,
    last_vote_ts: u64,
}

impl VoteStats {
    fn as_summary(&self) -> SectionVoteSummary {
        SectionVoteSummary {
            up: self.up,
            down: self.down,
            last_vote_ts: self.last_vote_ts,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct SectionKey {
    pub lexeme_id: u32,
    pub kind: SectionKind,
}

impl SectionKey {
    pub fn new(lexeme_id: u32, kind: SectionKind) -> Self {
        Self { lexeme_id, kind }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SectionKind {
    SenseDefinition {
        sense_index: i32,
    },
    SenseRelations {
        sense_index: i32,
        relation: RelationKind,
    },
    Encyclopedia,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum VoteDirection {
    Up,
    Down,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SectionVoteSummary {
    pub up: u64,
    pub down: u64,
    pub last_vote_ts: u64,
}

impl SectionVoteSummary {
    pub fn total(&self) -> u64 {
        self.up.saturating_add(self.down)
    }

    pub fn confidence_ratio(&self) -> Option<f32> {
        let total = self.total();
        if total < MIN_CONFIDENCE_VOTES {
            return None;
        }
        Some(self.up as f32 / total as f32)
    }
}

#[derive(Debug, Clone, Default)]
pub struct LexemeFeedbackBundle {
    pub definitions: HashMap<i32, SectionVoteSummary>,
    pub relations: HashMap<(i32, RelationKind), SectionVoteSummary>,
    pub encyclopedia: Option<SectionVoteSummary>,
}

#[derive(Debug, Clone, Serialize)]
pub struct IssueReport {
    pub id: u64,
    pub lexeme_id: Option<u32>,
    pub section: Option<SectionKind>,
    pub reason: IssueKind,
    pub note: Option<String>,
    pub session_id: Option<String>,
    pub created_at: u64,
}

#[derive(Debug, Clone)]
pub struct IssueReportRequest {
    pub lexeme_id: Option<u32>,
    pub section: Option<SectionKind>,
    pub reason: IssueKind,
    pub note: Option<String>,
    pub session_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IssueKind {
    DuplicateWord,
    OffensiveContent,
    BrokenRelation,
    FormattingIssue,
    Other,
}

impl IssueKind {
    pub fn label(&self) -> &'static str {
        match self {
            IssueKind::DuplicateWord => "Duplicate word",
            IssueKind::OffensiveContent => "Offensive content",
            IssueKind::BrokenRelation => "Broken relation",
            IssueKind::FormattingIssue => "Formatting issue",
            IssueKind::Other => "Other",
        }
    }
}

#[derive(Default)]
struct RelationClickStats {
    count: u64,
    last_clicked_ts: u64,
}

#[derive(Clone, Hash, PartialEq, Eq)]
struct RelationClickKey {
    source_lexeme: u32,
    target_word: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct RelationClickStat {
    pub target_word: String,
    pub count: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct TrendingLexeme {
    pub lexeme_id: u32,
    pub word: String,
    pub total_views: u64,
    pub trend_score: f64,
}

struct TrendingCandidate {
    lexeme_id: u32,
    score: f64,
    total: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct SpotlightLexeme {
    pub lexeme_id: u32,
    pub word: String,
    pub summary: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ChallengeCard {
    pub start: ChallengeNode,
    pub target: ChallengeNode,
    pub hop_count: usize,
    pub hint_relations: Vec<RelationKind>,
    pub path: Vec<ChallengeStep>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ChallengeNode {
    pub lexeme_id: u32,
    pub word: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ChallengeStep {
    pub word: String,
    pub lexeme_id: u32,
    pub via: Option<RelationKind>,
}

#[derive(Debug, Clone, Serialize)]
pub struct RelationPuzzle {
    pub lexeme_id: u32,
    pub word: String,
    pub relation: RelationKind,
    pub clue: String,
    pub answer: String,
}

#[derive(Debug, Clone, Copy, Serialize)]
pub struct SessionProgress {
    pub today_unique_words: usize,
    pub consecutive_days: u32,
    pub total_unique_words: u64,
}

#[derive(Clone)]
struct SessionStats {
    last_seen_ts: u64,
    current_day: u32,
    consecutive_days: u32,
    today_words: HashSet<u32>,
    all_time_words: HashSet<u32>,
    total_unique_count: u64,
}

impl SessionStats {
    fn new(now: u64) -> Self {
        let day = day_code(now);
        Self {
            last_seen_ts: now,
            current_day: day,
            consecutive_days: 1,
            today_words: HashSet::with_capacity(32),
            all_time_words: HashSet::with_capacity(64),
            total_unique_count: 0,
        }
    }

    fn mark_visit(&mut self, now: u64, lexeme_id: u32) {
        let day = day_code(now);
        if day != self.current_day {
            if day == self.current_day + 1 {
                self.consecutive_days = self.consecutive_days.saturating_add(1);
            } else {
                self.consecutive_days = 1;
            }
            self.current_day = day;
            self.today_words.clear();
        }
        self.last_seen_ts = now;
        if self.today_words.len() < MAX_SESSION_TRACKED_DAILY {
            self.today_words.insert(lexeme_id);
        } else if !self.today_words.contains(&lexeme_id) {
            self.today_words.insert(lexeme_id);
        }
        if !self.all_time_words.contains(&lexeme_id) {
            self.total_unique_count = self.total_unique_count.saturating_add(1);
            if self.all_time_words.len() < MAX_SESSION_TRACKED_TOTALS {
                self.all_time_words.insert(lexeme_id);
            }
        }
    }

    fn today_unique_count(&self) -> usize {
        self.today_words.len()
    }

    fn as_progress(&self) -> SessionProgress {
        SessionProgress {
            today_unique_words: self.today_unique_count(),
            consecutive_days: self.consecutive_days,
            total_unique_words: self.total_unique_count,
        }
    }
}

struct TelemetryPersistence {
    path: Option<PathBuf>,
    last_flush: AtomicU64,
}

impl TelemetryPersistence {
    fn new(path: Option<PathBuf>) -> Self {
        Self {
            path,
            last_flush: AtomicU64::new(0),
        }
    }

    fn should_snapshot(&self) -> bool {
        if self.path.is_none() {
            return false;
        }
        let now = now_ts();
        let last = self.last_flush.load(AtomicOrdering::Relaxed);
        if now.saturating_sub(last) >= SNAPSHOT_INTERVAL_SECS {
            return true;
        }
        thread_rng().gen_bool(SNAPSHOT_RANDOM_CHANCE)
    }

    fn write_snapshot(&self, snapshot: TelemetrySnapshot) {
        let Some(path) = &self.path else {
            return;
        };
        if let Some(parent) = path.parent() {
            if let Err(err) = fs::create_dir_all(parent) {
                warn!(error = %err, "failed to create telemetry directory");
                return;
            }
        }
        match OpenOptions::new().create(true).append(true).open(path) {
            Ok(mut file) => {
                let line = match serde_json::to_vec(&snapshot) {
                    Ok(bytes) => bytes,
                    Err(err) => {
                        warn!(error = %err, "failed to serialize telemetry snapshot");
                        return;
                    }
                };
                if let Err(err) = file.write_all(&line) {
                    warn!(error = %err, "failed to write telemetry snapshot");
                    return;
                }
                if let Err(err) = file.write_all(b"\n") {
                    warn!(error = %err, "failed to terminate telemetry snapshot line");
                }
                self.last_flush.store(now_ts(), AtomicOrdering::Release);
            }
            Err(err) => warn!(error = %err, "failed to open telemetry snapshot file"),
        }
    }
}

#[derive(Serialize)]
struct TelemetrySnapshot {
    captured_at: u64,
    lexeme_views: Vec<LexemeViewSnapshot>,
    section_votes: Vec<SectionVoteSnapshot>,
    issues: Vec<IssueReport>,
    relation_clicks: Vec<RelationClickSnapshot>,
    sessions: Vec<SessionSnapshot>,
}

#[derive(Serialize)]
struct LexemeViewSnapshot {
    lexeme_id: u32,
    total_views: u64,
    rolling_score: f64,
    last_view_ts: u64,
}

#[derive(Serialize)]
struct SectionVoteSnapshot {
    lexeme_id: u32,
    section: SectionKind,
    up: u64,
    down: u64,
    last_vote_ts: u64,
}

#[derive(Serialize)]
struct RelationClickSnapshot {
    lexeme_id: u32,
    target_word: String,
    count: u64,
    last_clicked_ts: u64,
}

#[derive(Serialize)]
struct SessionSnapshot {
    session_id: String,
    today_unique: usize,
    total_unique: u64,
    consecutive_days: u32,
}

fn now_ts() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn day_code(ts: u64) -> u32 {
    (ts / 86_400) as u32
}

fn oldest_session_key(sessions: &HashMap<String, SessionStats>) -> Option<String> {
    sessions
        .iter()
        .min_by_key(|(_, stats)| stats.last_seen_ts)
        .map(|(key, _)| key.clone())
}

fn prune_relation_clicks(map: &mut HashMap<RelationClickKey, RelationClickStats>) {
    let candidate = map
        .iter()
        .min_by_key(|(_, stats)| stats.count)
        .map(|(key, _)| key.clone());
    if let Some(key) = candidate {
        map.remove(&key);
    }
}

fn build_challenge(traversal: &crate::GraphTraversal) -> Option<ChallengeCard> {
    let mut rng = SmallRng::from_entropy();
    let mut nodes_by_id = HashMap::new();
    for node in &traversal.nodes {
        nodes_by_id.insert(node.lexeme_id, node);
    }
    let candidates: Vec<_> = traversal
        .nodes
        .iter()
        .filter(|node| node.depth >= 2)
        .collect();
    if candidates.is_empty() {
        return None;
    }
    let target_node = candidates[rng.gen_range(0..candidates.len())];
    let mut path = Vec::new();
    let mut cursor = Some(target_node.lexeme_id);
    while let Some(id) = cursor {
        if let Some(node) = nodes_by_id.get(&id) {
            path.push(ChallengeStep {
                word: node.word.clone(),
                lexeme_id: node.lexeme_id,
                via: node.via,
            });
            cursor = node.parent;
        } else {
            break;
        }
    }
    path.reverse();
    let start = path.first()?.clone();
    let target = path.last()?.clone();
    let mut hints = Vec::new();
    for step in &path {
        if let Some(relation) = step.via {
            if !hints.contains(&relation) {
                hints.push(relation);
            }
        }
    }
    Some(ChallengeCard {
        start: ChallengeNode {
            lexeme_id: start.lexeme_id,
            word: start.word.clone(),
        },
        target: ChallengeNode {
            lexeme_id: target.lexeme_id,
            word: target.word.clone(),
        },
        hop_count: path.len().saturating_sub(1),
        hint_relations: hints,
        path,
    })
}

fn build_relation_puzzle(entry: &LexemeEntry<'_>) -> Option<RelationPuzzle> {
    let synonyms: Vec<_> = entry.all_synonyms().collect();
    if synonyms.len() < 2 {
        return None;
    }
    let source_word = entry.word();
    let filtered: Vec<_> = synonyms
        .into_iter()
        .filter(|syn| is_valid_puzzle_answer(source_word, syn))
        .collect();
    if filtered.is_empty() {
        return None;
    }
    let mut rng = thread_rng();
    let answer = filtered[rng.gen_range(0..filtered.len())]
        .trim()
        .to_string();
    let prefix: String = answer.chars().take(5).collect();
    Some(RelationPuzzle {
        lexeme_id: entry.lexeme_id(),
        word: source_word.to_string(),
        relation: RelationKind::Synonym,
        clue: format!("Starts with \"{}\"", prefix),
        answer,
    })
}

fn is_valid_puzzle_answer(source: &str, candidate: &str) -> bool {
    let source = source.trim();
    let candidate = candidate.trim();
    if source.is_empty() || candidate.is_empty() {
        return false;
    }
    let source_lower = source.to_lowercase();
    let candidate_lower = candidate.to_lowercase();
    if candidate_lower == source_lower {
        return false;
    }
    if candidate_lower.contains(&source_lower) {
        return false;
    }
    let prefix: String = source_lower.chars().take(3).collect();
    if !prefix.is_empty() && candidate_lower.starts_with(&prefix) {
        return false;
    }
    true
}

fn challenge_is_noun_only(card: &ChallengeCard) -> bool {
    card.path.iter().all(|step| lexeme_is_noun(step.lexeme_id))
}

fn lexeme_is_noun(lexeme_id: u32) -> bool {
    LexemeIndex::entry_by_id(lexeme_id)
        .map(|entry| {
            entry
                .parts_of_speech()
                .any(|pos| pos.eq_ignore_ascii_case("noun"))
        })
        .unwrap_or(false)
}

fn snippet(text: &str, max_chars: usize) -> String {
    let mut out = String::new();
    for (idx, ch) in text.chars().enumerate() {
        if idx >= max_chars {
            out.push('…');
            break;
        }
        out.push(ch);
    }
    if out.is_empty() {
        text.chars().take(max_chars).collect()
    } else {
        out
    }
}

pub fn generate_session_id() -> String {
    thread_rng()
        .sample_iter(&Alphanumeric)
        .take(24)
        .map(char::from)
        .collect()
}

pub fn describe_ratio(summary: &SectionVoteSummary, label: &str) -> Option<String> {
    summary.confidence_ratio().map(|ratio| {
        let percent = (ratio * 100.0).round() as i64;
        format!(
            "Community confidence: {percent}% positive {label} ({votes} votes)",
            votes = summary.total()
        )
    })
}
