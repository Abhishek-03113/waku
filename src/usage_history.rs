//! Historical token and cost usage for the settings Usage page, scanned from
//! the provider CLIs' own on-disk session transcripts (Claude Code's
//! `~/.claude/projects` and Codex's `~/.codex/sessions`) the way T3 Code and
//! `ccusage` do it, so usage covers turns driven outside Waku too. Costs are
//! priced against LiteLLM's model rate table, fetched at most daily and cached
//! beside the app database.
//!
//! Everything here blocks on the filesystem and (for the rate table) the
//! network, and must run on the background executor. Render reads only the
//! [`UsageHistory`] snapshot the app entity stores. Transcripts are
//! append-only, so parsed records are memoised per file by `(size, mtime)` in
//! a [`ScanCache`] the caller owns; warm rescans only reparse changed files.

use std::collections::{HashMap, HashSet};
use std::io::BufRead as _;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use chrono::{Local, NaiveDate, TimeZone as _, Utc};
use serde_json::Value;

/// The window lengths the page offers, in days.
pub const WINDOW_OPTIONS: [u32; 3] = [7, 30, 90];

/// Files whose mtime predates the window start by more than this are skipped
/// without opening. The slack covers a session whose last write lands just
/// before local midnight on the window's first day.
const MTIME_SLACK: Duration = Duration::from_secs(36 * 3600);

const LITELLM_RATES_URL: &str =
    "https://raw.githubusercontent.com/BerriAI/litellm/main/model_prices_and_context_window.json";
const RATES_CACHE_FILE: &str = "usage-model-rates.json";
/// Rates move rarely; a day-old table keeps the page working offline.
const RATES_TTL: Duration = Duration::from_secs(24 * 3600);

/// Providers with a transcript scanner. Mirrors T3 Code's coverage.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum UsageProvider {
    Claude,
    Codex,
}

impl UsageProvider {
    pub const ALL: [UsageProvider; 2] = [UsageProvider::Claude, UsageProvider::Codex];

    pub fn label(self) -> &'static str {
        match self {
            UsageProvider::Claude => "Claude Code",
            UsageProvider::Codex => "Codex",
        }
    }

    /// Index into the fixed per-provider arrays on [`DaySlice`].
    pub fn index(self) -> usize {
        match self {
            UsageProvider::Claude => 0,
            UsageProvider::Codex => 1,
        }
    }
}

/// Token counts for one record or bucket. `reasoning` is a subset of `output`
/// and is never added into [`TokenTotals::total`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TokenTotals {
    pub uncached_input: u64,
    pub cached_input: u64,
    pub cache_creation: u64,
    pub output: u64,
    pub reasoning: u64,
}

impl TokenTotals {
    pub fn total(&self) -> u64 {
        self.uncached_input + self.cached_input + self.cache_creation + self.output
    }

    fn add(&mut self, other: &TokenTotals) {
        self.uncached_input += other.uncached_input;
        self.cached_input += other.cached_input;
        self.cache_creation += other.cache_creation;
        self.output += other.output;
        self.reasoning += other.reasoning;
    }
}

/// One usage event parsed out of a transcript line.
#[derive(Clone, Debug)]
pub struct UsageRecord {
    pub provider: UsageProvider,
    pub timestamp_ms: i64,
    pub model: String,
    pub session_id: String,
    pub totals: TokenTotals,
    pub reported_cost_usd: Option<f64>,
    /// Key for cross-file de-duplication, or `None` when the record is
    /// inherently unique and needs no dedup.
    pub dedupe_key: Option<String>,
}

/// Cheap substring gate applied before JSON parsing. Transcripts are mostly
/// tool output; only a minority of lines carry usage, and skipping the rest
/// before `serde_json` sees them is worth an order of magnitude.
fn might_carry_usage(line: &str, provider: UsageProvider) -> bool {
    match provider {
        UsageProvider::Claude => line.contains("\"usage\""),
        UsageProvider::Codex => line.contains("\"token_count\""),
    }
}

/// Positive finite number as a token count; anything else is zero.
fn int(value: Option<&Value>) -> u64 {
    value
        .and_then(Value::as_f64)
        .filter(|value| value.is_finite() && *value > 0.0)
        .map(|value| value.trunc() as u64)
        .unwrap_or(0)
}

fn parse_timestamp_ms(value: Option<&Value>) -> Option<i64> {
    let text = value?.as_str()?;
    chrono::DateTime::parse_from_rfc3339(text)
        .ok()
        .map(|date| date.timestamp_millis())
}

/// Parses one line of a Claude Code transcript.
///
/// The CLI writes one record per assistant *content block*, and every one of
/// those records repeats the same complete `usage` object for the parent
/// message. Summing them overcounts severely, so callers must drop repeats by
/// `dedupe_key` and keep the first.
fn parse_claude_line(line: &str) -> Option<UsageRecord> {
    let record: Value = serde_json::from_str(line).ok()?;
    if record.get("type").and_then(Value::as_str) != Some("assistant") {
        return None;
    }
    let message = record.get("message")?.as_object()?;
    let usage = message.get("usage")?.as_object()?;
    let timestamp_ms = parse_timestamp_ms(record.get("timestamp"))?;
    let model = message.get("model").and_then(Value::as_str)?;
    if model.is_empty() {
        return None;
    }

    let message_id = message.get("id").and_then(Value::as_str);
    let request_id = record.get("requestId").and_then(Value::as_str);
    // Matches ccusage: prefer the message/request pair, fall back to whichever
    // half exists. Records with neither cannot be de-duplicated.
    let dedupe_key = (message_id.is_some() || request_id.is_some()).then(|| {
        format!(
            "{}:{}",
            message_id.unwrap_or_default(),
            request_id.unwrap_or_default()
        )
    });

    Some(UsageRecord {
        provider: UsageProvider::Claude,
        timestamp_ms,
        model: model.to_owned(),
        session_id: record
            .get("sessionId")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned(),
        totals: TokenTotals {
            uncached_input: int(usage.get("input_tokens")),
            cached_input: int(usage.get("cache_read_input_tokens")),
            cache_creation: int(usage.get("cache_creation_input_tokens")),
            output: int(usage.get("output_tokens")),
            // Anthropic folds thinking tokens into output without a breakout.
            reasoning: 0,
        },
        reported_cost_usd: record
            .get("costUSD")
            .and_then(Value::as_f64)
            .filter(|cost| cost.is_finite()),
        dedupe_key,
    })
}

/// Rolling state for a single Codex rollout file. `token_count` events carry
/// no model, so the model is carried forward from the most recent
/// `turn_context`; sessions that switch models mid-run attribute correctly
/// from the switch onward.
struct CodexScanState {
    model: String,
    session_id: String,
    last_usage_signature: Option<String>,
}

impl CodexScanState {
    fn new() -> Self {
        Self {
            model: String::new(),
            session_id: String::new(),
            last_usage_signature: None,
        }
    }
}

/// Feeds one line of a Codex rollout into `state`, returning a record when the
/// line was a usage event. Deltas come from `last_token_usage`; summing those
/// across a session reconciles with the session's final `total_token_usage`
/// provided consecutive duplicate events are dropped, which this does.
fn parse_codex_line(line: &str, state: &mut CodexScanState) -> Option<UsageRecord> {
    let record: Value = serde_json::from_str(line).ok()?;
    let payload = record.get("payload")?.as_object()?;

    match record.get("type").and_then(Value::as_str) {
        Some("session_meta") => {
            if let Some(id) = payload
                .get("id")
                .or_else(|| payload.get("session_id"))
                .and_then(Value::as_str)
            {
                state.session_id = id.to_owned();
            }
            return None;
        }
        Some("turn_context") => {
            if let Some(model) = payload.get("model").and_then(Value::as_str) {
                state.model = model.to_owned();
            }
            return None;
        }
        _ => {}
    }

    if payload.get("type").and_then(Value::as_str) != Some("token_count") {
        return None;
    }
    let last = payload.get("info")?.get("last_token_usage")?.as_object()?;

    // Only an event that is otherwise eligible may consume the duplicate
    // signature. A token_count arriving before its turn_context (no model yet)
    // must not poison it, or the re-emitted copy after the model is known
    // would be skipped as a duplicate and those tokens never counted.
    let timestamp_ms = parse_timestamp_ms(record.get("timestamp"))?;
    if state.model.is_empty() {
        return None;
    }

    // Codex re-emits an unchanged token_count on some stream boundaries.
    // Summing those would double count, so identical consecutive payloads are
    // skipped.
    let signature = serde_json::to_string(last).ok()?;
    if state.last_usage_signature.as_deref() == Some(signature.as_str()) {
        return None;
    }
    state.last_usage_signature = Some(signature);

    let input = int(last.get("input_tokens"));
    let cached_input = int(last.get("cached_input_tokens"));
    let cache_creation = int(last.get("cache_write_input_tokens"));
    let output = int(last.get("output_tokens"));
    let totals = TokenTotals {
        // Codex reports `input_tokens` inclusive of the cached portion.
        uncached_input: input.saturating_sub(cached_input + cache_creation),
        cached_input,
        cache_creation,
        output,
        // Reported inside output_tokens, surfaced separately for the mix.
        reasoning: int(last.get("reasoning_output_tokens")).min(output),
    };
    if totals.total() == 0 {
        return None;
    }

    Some(UsageRecord {
        provider: UsageProvider::Codex,
        timestamp_ms,
        model: state.model.clone(),
        session_id: state.session_id.clone(),
        totals,
        // Codex does not report cost in the rollout.
        reported_cost_usd: None,
        // Rollout files are unique per session; no global dedup needed.
        dedupe_key: None,
    })
}

/* ------------------------------------------------------------------------- */
/* Pricing                                                                   */
/* ------------------------------------------------------------------------- */

/// USD per token at the base tier. Tiered variants (long-context, flex,
/// batch) are deliberately ignored: transcripts don't record which tier
/// served a request, so anything else would be a guess dressed as precision.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ModelRate {
    pub input: f64,
    pub output: f64,
    pub cache_read: f64,
    pub cache_creation: f64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PricingStatus {
    /// Fetched from LiteLLM within this scan.
    Fresh,
    /// Served from the on-disk snapshot.
    Cached,
    /// No table at all; every model reports as unpriced.
    Unavailable,
}

#[derive(Clone, Debug)]
pub struct RateTable {
    pub rates: HashMap<String, ModelRate>,
    pub status: PricingStatus,
}

impl RateTable {
    pub fn unavailable() -> Self {
        Self {
            rates: HashMap::new(),
            status: PricingStatus::Unavailable,
        }
    }
}

/// Models never priced regardless of the table: `<synthetic>` marks locally
/// generated messages that were never billed, and bare family names are
/// ambiguous across generations, so they report as unpriced instead of
/// guessing one.
const UNPRICEABLE_MODELS: [&str; 6] = [
    "<synthetic>",
    "synthetic",
    "opus",
    "sonnet",
    "haiku",
    "fable",
];

/// Canonicalises a model name for lookup: strips a `provider/` prefix
/// (LiteLLM publishes both `claude-opus-5` and `anthropic/claude-opus-5`) and
/// lowercases, since transcripts are inconsistent about casing.
fn normalize_model_name(model: &str) -> String {
    let trimmed = model.trim().to_ascii_lowercase();
    match trimmed.rfind('/') {
        Some(slash) => trimmed[slash + 1..].to_owned(),
        None => trimmed,
    }
}

fn lookup_rate<'a>(table: &'a RateTable, model: &str) -> Option<&'a ModelRate> {
    let normalized = normalize_model_name(model);
    if normalized.is_empty() || UNPRICEABLE_MODELS.contains(&normalized.as_str()) {
        return None;
    }
    table.rates.get(&normalized)
}

/// Projects the LiteLLM document into a rate table. Entries without both an
/// input and an output rate are dropped: a half-priced model would silently
/// under-report cost, which is worse than reporting it as unpriced.
fn parse_rate_table(document: &Value) -> HashMap<String, ModelRate> {
    let mut table = HashMap::new();
    let Some(entries) = document.as_object() else {
        return table;
    };
    let finite = |value: Option<&Value>| value.and_then(Value::as_f64).filter(|v| v.is_finite());
    for (name, entry) in entries {
        let Some(entry) = entry.as_object() else {
            continue;
        };
        let Some(input) = finite(entry.get("input_cost_per_token")) else {
            continue;
        };
        let Some(output) = finite(entry.get("output_cost_per_token")) else {
            continue;
        };
        table.insert(
            normalize_model_name(name),
            ModelRate {
                input,
                output,
                // Anthropic bills cache reads at a discount and writes at a
                // premium. When a model omits them, cached input is priced as
                // plain input rather than as free.
                cache_read: finite(entry.get("cache_read_input_token_cost")).unwrap_or(input),
                cache_creation: finite(entry.get("cache_creation_input_token_cost"))
                    .unwrap_or(input),
            },
        );
    }
    table
}

fn unix_time_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as i64)
        .unwrap_or(0)
}

fn read_rates_cache(path: &Path) -> Option<(i64, HashMap<String, ModelRate>)> {
    let document: Value = serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()?;
    let fetched_at_ms = document.get("fetched_at_ms")?.as_i64()?;
    let mut rates = HashMap::new();
    for (name, entry) in document.get("rates")?.as_object()? {
        let values = entry.as_array()?;
        let field = |index: usize| values.get(index).and_then(Value::as_f64);
        rates.insert(
            name.clone(),
            ModelRate {
                input: field(0)?,
                output: field(1)?,
                cache_read: field(2)?,
                cache_creation: field(3)?,
            },
        );
    }
    Some((fetched_at_ms, rates))
}

fn write_rates_cache(path: &Path, fetched_at_ms: i64, rates: &HashMap<String, ModelRate>) {
    let entries: serde_json::Map<String, Value> = rates
        .iter()
        .map(|(name, rate)| {
            (
                name.clone(),
                serde_json::json!([
                    rate.input,
                    rate.output,
                    rate.cache_read,
                    rate.cache_creation
                ]),
            )
        })
        .collect();
    let document = serde_json::json!({ "fetched_at_ms": fetched_at_ms, "rates": entries });
    // Best effort: a cache that fails to write only costs the next scan a
    // refetch.
    let _ = std::fs::write(path, document.to_string());
}

/// Load the LiteLLM rate table, preferring the on-disk snapshot while it is
/// within TTL, refetching otherwise, and degrading to the stale snapshot or an
/// empty table rather than failing the scan. Blocking: disk plus up to one
/// HTTPS round trip. Never call from the UI thread.
pub fn load_rate_table(cache_dir: &Path) -> RateTable {
    let cache_path = cache_dir.join(RATES_CACHE_FILE);
    let disk = read_rates_cache(&cache_path).filter(|(_, rates)| !rates.is_empty());
    let now_ms = unix_time_ms();
    if let Some((fetched_at_ms, rates)) = &disk
        && now_ms.saturating_sub(*fetched_at_ms) < RATES_TTL.as_millis() as i64
    {
        return RateTable {
            rates: rates.clone(),
            status: PricingStatus::Cached,
        };
    }

    let fetched =
        crate::usage::http_get(LITELLM_RATES_URL, &["Accept: application/json".to_owned()])
            .ok()
            .filter(|(status, _)| *status == 200)
            .and_then(|(_, body)| serde_json::from_str::<Value>(&body).ok())
            .map(|document| parse_rate_table(&document))
            .filter(|rates| !rates.is_empty());

    match (fetched, disk) {
        (Some(rates), _) => {
            write_rates_cache(&cache_path, now_ms, &rates);
            RateTable {
                rates,
                status: PricingStatus::Fresh,
            }
        }
        (None, Some((_, rates))) => RateTable {
            rates,
            status: PricingStatus::Cached,
        },
        (None, None) => RateTable::unavailable(),
    }
}

/* ------------------------------------------------------------------------- */
/* Scanning                                                                  */
/* ------------------------------------------------------------------------- */

/// Parsed records for one transcript, memoised by `(size, mtime, provider)`.
/// Records are stored unfiltered by window; the aggregation applies the day
/// filter, so one cache serves every window length.
pub struct FileCacheEntry {
    size: u64,
    mtime_ms: i64,
    provider: UsageProvider,
    records: Vec<UsageRecord>,
}

pub type ScanCache = HashMap<PathBuf, FileCacheEntry>;

/// The transcript root scanned for one provider.
fn provider_root(provider: UsageProvider) -> Option<PathBuf> {
    match provider {
        UsageProvider::Claude => match std::env::var_os("CLAUDE_CONFIG_DIR") {
            Some(dir) if !dir.is_empty() => Some(PathBuf::from(dir).join("projects")),
            _ => dirs::home_dir().map(|home| home.join(".claude/projects")),
        },
        UsageProvider::Codex => match std::env::var_os("CODEX_HOME") {
            Some(dir) if !dir.is_empty() => Some(PathBuf::from(dir).join("sessions")),
            _ => dirs::home_dir().map(|home| home.join(".codex/sessions")),
        },
    }
}

/// Lists `.jsonl` transcripts under `root` modified at or after `since_ms`.
/// Errors on individual entries are swallowed: session files rotate and get
/// removed while the walk is in flight, and a partial listing beats failing
/// the page. Returns the number of files skipped by the mtime prefilter.
fn list_transcript_files(
    root: &Path,
    since_ms: i64,
    found: &mut Vec<(PathBuf, u64, i64)>,
) -> usize {
    let mut skipped = 0;
    let Ok(entries) = std::fs::read_dir(root) else {
        return 0;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if file_type.is_dir() {
            skipped += list_transcript_files(&path, since_ms, found);
            continue;
        }
        if path.extension().and_then(|extension| extension.to_str()) != Some("jsonl") {
            continue;
        }
        let Ok(metadata) = entry.metadata() else {
            continue;
        };
        let mtime_ms = metadata
            .modified()
            .ok()
            .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
            .map(|elapsed| elapsed.as_millis() as i64)
            .unwrap_or(0);
        if mtime_ms < since_ms {
            skipped += 1;
            continue;
        }
        found.push((path, metadata.len(), mtime_ms));
    }
    skipped
}

/// Streams one transcript and returns the usage records it contains, already
/// de-duplicated within the file, or `None` when it could not be read. The
/// distinction matters to the cache: an empty transcript is a stable fact
/// worth memoising, while a transient read failure memoised under the same
/// `(size, mtime)` would silently drop the file's usage until it changes.
fn read_transcript_records(path: &Path, provider: UsageProvider) -> Option<Vec<UsageRecord>> {
    let file = std::fs::File::open(path).ok()?;
    let mut reader = std::io::BufReader::new(file);
    let mut line = String::new();
    let mut records = Vec::new();
    let mut codex_state = CodexScanState::new();
    let mut seen_in_file = HashSet::new();

    loop {
        line.clear();
        match reader.read_line(&mut line) {
            Ok(0) => break,
            Ok(_) => {}
            Err(_) => return None,
        }
        match provider {
            UsageProvider::Codex => {
                // Codex carries the active model on `turn_context` lines that
                // hold no usage of their own, so those still pass through the
                // reducer to keep model attribution correct.
                if !might_carry_usage(&line, provider)
                    && !line.contains("\"turn_context\"")
                    && !line.contains("\"session_meta\"")
                {
                    continue;
                }
                if let Some(record) = parse_codex_line(&line, &mut codex_state) {
                    records.push(record);
                }
            }
            UsageProvider::Claude => {
                if !might_carry_usage(&line, provider) {
                    continue;
                }
                if let Some(record) = parse_claude_line(&line) {
                    // Every assistant content block repeats the parent
                    // message's usage; the first record wins. Cross-file
                    // repeats (resumed or forked sessions) are handled by the
                    // aggregator's global pass.
                    if let Some(key) = &record.dedupe_key {
                        if !seen_in_file.insert(key.clone()) {
                            continue;
                        }
                    }
                    records.push(record);
                }
            }
        }
    }
    Some(records)
}

/* ------------------------------------------------------------------------- */
/* Aggregation                                                               */
/* ------------------------------------------------------------------------- */

#[derive(Clone, Copy, PartialEq)]
enum CostSource {
    Reported,
    Priced,
    Unpriced,
}

#[derive(Default)]
struct Bucket {
    totals: TokenTotals,
    cost_usd: f64,
    cache_savings_usd: f64,
    records: u64,
    unpriced_records: u64,
    reported_records: u64,
}

/// Folds records into `(day, provider, model)` buckets with global cross-file
/// de-duplication, then derives the page's view.
struct Aggregator {
    since_day: NaiveDate,
    until_day: NaiveDate,
    buckets: HashMap<(NaiveDate, UsageProvider, String), Bucket>,
    seen: HashSet<String>,
    sessions: HashSet<(UsageProvider, String)>,
}

impl Aggregator {
    fn new(since_day: NaiveDate, until_day: NaiveDate) -> Self {
        Self {
            since_day,
            until_day,
            buckets: HashMap::new(),
            seen: HashSet::new(),
            sessions: HashSet::new(),
        }
    }

    fn add(&mut self, record: &UsageRecord, rates: &RateTable) {
        if let Some(key) = &record.dedupe_key {
            if !self.seen.insert(key.clone()) {
                return;
            }
        }
        let Some(timestamp) = Utc.timestamp_millis_opt(record.timestamp_ms).single() else {
            return;
        };
        let day = timestamp.with_timezone(&Local).date_naive();
        if day < self.since_day || day > self.until_day {
            return;
        }

        let (cost_usd, source) = match record.reported_cost_usd {
            Some(cost) => (cost, CostSource::Reported),
            None => match lookup_rate(rates, &record.model) {
                Some(rate) => (
                    record.totals.uncached_input as f64 * rate.input
                        + record.totals.cached_input as f64 * rate.cache_read
                        + record.totals.cache_creation as f64 * rate.cache_creation
                        + record.totals.output as f64 * rate.output,
                    CostSource::Priced,
                ),
                None => (0.0, CostSource::Unpriced),
            },
        };
        // What the cached input would have cost at full input rates, minus
        // what it actually cost. Drives the "cache savings" figure.
        let cache_savings_usd = lookup_rate(rates, &record.model)
            .map(|rate| record.totals.cached_input as f64 * (rate.input - rate.cache_read))
            .unwrap_or(0.0);

        let bucket = self
            .buckets
            .entry((day, record.provider, record.model.clone()))
            .or_default();
        bucket.totals.add(&record.totals);
        bucket.cost_usd += cost_usd;
        bucket.cache_savings_usd += cache_savings_usd;
        bucket.records += 1;
        match source {
            CostSource::Unpriced => bucket.unpriced_records += 1,
            CostSource::Reported => bucket.reported_records += 1,
            CostSource::Priced => {}
        }
        if !record.session_id.is_empty() {
            self.sessions
                .insert((record.provider, record.session_id.clone()));
        }
    }
}

/* ------------------------------------------------------------------------- */
/* Derived view                                                              */
/* ------------------------------------------------------------------------- */

#[derive(Clone, Debug)]
pub struct ProviderSlice {
    pub provider: UsageProvider,
    pub cost_usd: f64,
    pub total_tokens: u64,
    pub cost_share: f64,
    pub token_share: f64,
}

#[derive(Clone, Debug)]
pub struct ModelSlice {
    pub provider: UsageProvider,
    pub model: String,
    pub cost_usd: f64,
    pub total_tokens: u64,
    pub cost_share: f64,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct ProviderDay {
    pub cost_usd: f64,
    pub total_tokens: u64,
}

/// One calendar day with activity, ascending by day in
/// [`UsageHistory::daily`]. `by_provider` is indexed by
/// [`UsageProvider::index`].
#[derive(Clone, Debug)]
pub struct DaySlice {
    pub day: NaiveDate,
    pub cost_usd: f64,
    pub total_tokens: u64,
    pub by_provider: [ProviderDay; 2],
}

#[derive(Clone, Copy, Debug, Default)]
pub struct CostQuality {
    pub provider_reported_share: f64,
    pub model_priced_share: f64,
    pub unpriced_share: f64,
    pub cache_savings_usd: f64,
}

/// The fully derived snapshot the Usage page renders. Everything a frame
/// needs is precomputed here so render does no aggregation of its own.
#[derive(Clone, Debug)]
pub struct UsageHistory {
    pub window_days: u32,
    pub since_day: NaiveDate,
    pub until_day: NaiveDate,
    pub totals: TokenTotals,
    pub total_tokens: u64,
    pub cost_usd: f64,
    pub records: u64,
    pub sessions: u64,
    /// Sorted by cost descending.
    pub providers: Vec<ProviderSlice>,
    /// Sorted by cost descending, then tokens.
    pub models: Vec<ModelSlice>,
    /// Days with activity, ascending.
    pub daily: Vec<DaySlice>,
    pub quality: CostQuality,
    pub pricing: PricingStatus,
    pub scanned_files: usize,
    pub skipped_files: usize,
    /// Provider roots that exist but could not be read.
    pub errors: Vec<String>,
    pub scan_duration: Duration,
}

impl UsageHistory {
    /// The day's slice, if that day had activity. `daily` is sorted, so this
    /// is a binary search.
    pub fn day(&self, day: NaiveDate) -> Option<&DaySlice> {
        self.daily
            .binary_search_by_key(&day, |slice| slice.day)
            .ok()
            .map(|index| &self.daily[index])
    }
}

/// Scan every provider's transcripts for the trailing `window_days` window
/// ending today and derive the page snapshot. Blocking; run on the background
/// executor with the caller-owned `cache` and a loaded rate table.
pub fn scan(cache: &mut ScanCache, rates: &RateTable, window_days: u32) -> UsageHistory {
    let started = Instant::now();
    let until_day = Local::now().date_naive();
    let since_day = until_day - chrono::Days::new(u64::from(window_days.saturating_sub(1)));
    // Local midnight on the window's first day; a DST gap falls back to the
    // day boundary in UTC, which the mtime slack absorbs anyway.
    let window_start_ms = since_day
        .and_hms_opt(0, 0, 0)
        .and_then(|midnight| Local.from_local_datetime(&midnight).earliest())
        .map(|midnight| midnight.timestamp_millis())
        .unwrap_or_else(|| unix_time_ms() - i64::from(window_days) * 86_400_000);
    let mtime_cutoff_ms = window_start_ms - MTIME_SLACK.as_millis() as i64;

    let mut aggregator = Aggregator::new(since_day, until_day);
    let mut scanned_files = 0;
    let mut skipped_files = 0;
    let mut errors = Vec::new();

    for provider in UsageProvider::ALL {
        let Some(root) = provider_root(provider) else {
            continue;
        };
        if !root.is_dir() {
            // Provider never used on this machine; zero usage, not an error.
            continue;
        }
        let mut files = Vec::new();
        skipped_files += list_transcript_files(&root, mtime_cutoff_ms, &mut files);
        if files.is_empty() && std::fs::read_dir(&root).is_err() {
            errors.push(format!(
                "{} transcripts at {} could not be read.",
                provider.label(),
                root.display()
            ));
            continue;
        }
        for (path, size, mtime_ms) in files {
            scanned_files += 1;
            let cached = cache.get(&path);
            // Provider is part of the identity: if both providers were ever
            // pointed at one directory, a hit parsed by the other parser must
            // not be reused.
            let records = match cached {
                Some(entry)
                    if entry.size == size
                        && entry.mtime_ms == mtime_ms
                        && entry.provider == provider =>
                {
                    &entry.records
                }
                _ => match read_transcript_records(&path, provider) {
                    Some(records) => {
                        &cache
                            .entry(path)
                            .insert_entry(FileCacheEntry {
                                size,
                                mtime_ms,
                                provider,
                                records,
                            })
                            .into_mut()
                            .records
                    }
                    // A read failure is not an empty transcript: caching it
                    // under this (size, mtime) would silently drop the file's
                    // usage until it changes.
                    None => continue,
                },
            };
            for record in records {
                aggregator.add(record, rates);
            }
        }
    }

    derive_history(
        aggregator,
        window_days,
        since_day,
        until_day,
        rates.status,
        scanned_files,
        skipped_files,
        errors,
        started.elapsed(),
    )
}

#[allow(clippy::too_many_arguments)]
fn derive_history(
    aggregator: Aggregator,
    window_days: u32,
    since_day: NaiveDate,
    until_day: NaiveDate,
    pricing: PricingStatus,
    scanned_files: usize,
    skipped_files: usize,
    errors: Vec<String>,
    scan_duration: Duration,
) -> UsageHistory {
    let mut totals = TokenTotals::default();
    let mut cost_usd = 0.0;
    let mut cache_savings_usd = 0.0;
    let mut records = 0;
    let mut unpriced_records = 0;
    let mut reported_records = 0;
    let mut providers: HashMap<UsageProvider, (f64, u64)> = HashMap::new();
    let mut models: HashMap<(UsageProvider, String), (f64, u64)> = HashMap::new();
    let mut daily: HashMap<NaiveDate, DaySlice> = HashMap::new();

    for ((day, provider, model), bucket) in &aggregator.buckets {
        let tokens = bucket.totals.total();
        totals.add(&bucket.totals);
        cost_usd += bucket.cost_usd;
        cache_savings_usd += bucket.cache_savings_usd;
        records += bucket.records;
        unpriced_records += bucket.unpriced_records;
        reported_records += bucket.reported_records;

        let provider_entry = providers.entry(*provider).or_default();
        provider_entry.0 += bucket.cost_usd;
        provider_entry.1 += tokens;

        let model_entry = models.entry((*provider, model.clone())).or_default();
        model_entry.0 += bucket.cost_usd;
        model_entry.1 += tokens;

        let day_entry = daily.entry(*day).or_insert_with(|| DaySlice {
            day: *day,
            cost_usd: 0.0,
            total_tokens: 0,
            by_provider: [ProviderDay::default(); 2],
        });
        day_entry.cost_usd += bucket.cost_usd;
        day_entry.total_tokens += tokens;
        day_entry.by_provider[provider.index()].cost_usd += bucket.cost_usd;
        day_entry.by_provider[provider.index()].total_tokens += tokens;
    }

    let total_tokens = totals.total();
    let share = |part: f64, whole: f64| if whole == 0.0 { 0.0 } else { part / whole };

    let mut provider_slices: Vec<ProviderSlice> = providers
        .into_iter()
        .map(
            |(provider, (provider_cost, provider_tokens))| ProviderSlice {
                provider,
                cost_usd: provider_cost,
                total_tokens: provider_tokens,
                cost_share: share(provider_cost, cost_usd),
                token_share: share(provider_tokens as f64, total_tokens as f64),
            },
        )
        .collect();
    provider_slices.sort_by(|a, b| b.cost_usd.total_cmp(&a.cost_usd));

    let mut model_slices: Vec<ModelSlice> = models
        .into_iter()
        .map(
            |((provider, model), (model_cost, model_tokens))| ModelSlice {
                provider,
                model,
                cost_usd: model_cost,
                total_tokens: model_tokens,
                cost_share: share(model_cost, cost_usd),
            },
        )
        .collect();
    model_slices.sort_by(|a, b| {
        b.cost_usd
            .total_cmp(&a.cost_usd)
            .then(b.total_tokens.cmp(&a.total_tokens))
    });

    let mut day_slices: Vec<DaySlice> = daily.into_values().collect();
    day_slices.sort_by_key(|slice| slice.day);

    let record_share = |part: u64| share(part as f64, records as f64);
    UsageHistory {
        window_days,
        since_day,
        until_day,
        totals,
        total_tokens,
        cost_usd,
        records,
        sessions: aggregator.sessions.len() as u64,
        providers: provider_slices,
        models: model_slices,
        daily: day_slices,
        quality: CostQuality {
            provider_reported_share: record_share(reported_records),
            model_priced_share: record_share(records - reported_records - unpriced_records),
            unpriced_share: record_share(unpriced_records),
            cache_savings_usd,
        },
        pricing,
        scanned_files,
        skipped_files,
        errors,
        scan_duration,
    }
}

/// Inclusive day list between the window bounds, oldest first — the chart's
/// x-axis, including days with no activity.
pub fn enumerate_days(since_day: NaiveDate, until_day: NaiveDate) -> Vec<NaiveDate> {
    let mut days = Vec::new();
    let mut cursor = since_day;
    while cursor <= until_day {
        days.push(cursor);
        cursor = cursor + chrono::Days::new(1);
    }
    days
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rate_table(entries: &[(&str, ModelRate)]) -> RateTable {
        RateTable {
            rates: entries
                .iter()
                .map(|(name, rate)| ((*name).to_owned(), *rate))
                .collect(),
            status: PricingStatus::Fresh,
        }
    }

    const FLAT_RATE: ModelRate = ModelRate {
        input: 1e-6,
        output: 2e-6,
        cache_read: 1e-7,
        cache_creation: 1.25e-6,
    };

    #[test]
    fn claude_lines_parse_usage_and_dedupe_key() {
        // Shape captured from a live transcript on 2026-08-08.
        let line = r#"{"type":"assistant","timestamp":"2026-08-08T15:18:37.487Z",
            "requestId":"req_1","sessionId":"session-1","costUSD":null,
            "message":{"id":"msg_1","model":"claude-fable-5",
            "usage":{"input_tokens":2,"cache_creation_input_tokens":50700,
            "cache_read_input_tokens":0,"output_tokens":1238}}}"#
            .replace('\n', " ");
        let record = parse_claude_line(&line).expect("the line carries usage");
        assert_eq!(record.provider, UsageProvider::Claude);
        assert_eq!(record.model, "claude-fable-5");
        assert_eq!(record.session_id, "session-1");
        assert_eq!(record.dedupe_key.as_deref(), Some("msg_1:req_1"));
        assert_eq!(record.reported_cost_usd, None);
        assert_eq!(record.totals.uncached_input, 2);
        assert_eq!(record.totals.cache_creation, 50700);
        assert_eq!(record.totals.output, 1238);
        assert_eq!(record.totals.total(), 2 + 50700 + 1238);

        // Non-assistant lines and lines without usage parse to nothing.
        assert!(parse_claude_line(r#"{"type":"user","message":{}}"#).is_none());
    }

    #[test]
    fn codex_lines_carry_model_forward_and_skip_duplicates() {
        let mut state = CodexScanState::new();
        let meta = r#"{"timestamp":"2026-08-06T16:31:19.166Z","type":"session_meta",
            "payload":{"id":"codex-session"}}"#
            .replace('\n', " ");
        let context = r#"{"timestamp":"2026-08-06T16:31:20.000Z","type":"turn_context",
            "payload":{"model":"gpt-5.3-codex"}}"#
            .replace('\n', " ");
        let count = r#"{"timestamp":"2026-08-06T16:31:25.000Z","type":"event_msg",
            "payload":{"type":"token_count","info":{"last_token_usage":{
            "input_tokens":21047,"cached_input_tokens":1000,"cache_write_input_tokens":47,
            "output_tokens":280,"reasoning_output_tokens":165}}}}"#
            .replace('\n', " ");

        // A token_count before any turn_context has no model and is dropped
        // without consuming the duplicate signature.
        assert!(parse_codex_line(&count, &mut state).is_none());
        assert!(parse_codex_line(&meta, &mut state).is_none());
        assert!(parse_codex_line(&context, &mut state).is_none());

        let record = parse_codex_line(&count, &mut state).expect("model is known now");
        assert_eq!(record.model, "gpt-5.3-codex");
        assert_eq!(record.session_id, "codex-session");
        // input_tokens includes the cached portion.
        assert_eq!(record.totals.uncached_input, 21047 - 1000 - 47);
        assert_eq!(record.totals.cached_input, 1000);
        assert_eq!(record.totals.cache_creation, 47);
        assert_eq!(record.totals.reasoning, 165);

        // The identical re-emitted event is a duplicate, not more usage.
        assert!(parse_codex_line(&count, &mut state).is_none());
    }

    #[test]
    fn aggregation_dedupes_across_files_and_prices_records() {
        let rates = rate_table(&[("claude-fable-5", FLAT_RATE)]);
        let record = UsageRecord {
            provider: UsageProvider::Claude,
            timestamp_ms: Local::now().timestamp_millis(),
            model: "claude-fable-5".to_owned(),
            session_id: "session-1".to_owned(),
            totals: TokenTotals {
                uncached_input: 1_000,
                cached_input: 10_000,
                cache_creation: 2_000,
                output: 500,
                reasoning: 0,
            },
            reported_cost_usd: None,
            dedupe_key: Some("msg:req".to_owned()),
        };
        let today = Local::now().date_naive();
        let mut aggregator = Aggregator::new(today - chrono::Days::new(29), today);
        aggregator.add(&record, &rates);
        // The same message copied into a forked session's transcript.
        aggregator.add(&record, &rates);

        let history = derive_history(
            aggregator,
            30,
            today - chrono::Days::new(29),
            today,
            PricingStatus::Fresh,
            1,
            0,
            Vec::new(),
            Duration::ZERO,
        );
        assert_eq!(history.records, 1);
        assert_eq!(history.sessions, 1);
        assert_eq!(history.total_tokens, 13_500);
        let expected_cost = 1_000.0 * 1e-6 + 10_000.0 * 1e-7 + 2_000.0 * 1.25e-6 + 500.0 * 2e-6;
        assert!((history.cost_usd - expected_cost).abs() < 1e-9);
        // Savings: cached input at full rate minus the cache-read rate.
        let expected_savings = 10_000.0 * (1e-6 - 1e-7);
        assert!((history.quality.cache_savings_usd - expected_savings).abs() < 1e-9);
        assert_eq!(history.daily.len(), 1);
        assert_eq!(history.providers.len(), 1);
        assert!((history.quality.model_priced_share - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn out_of_window_and_unpriced_records_are_classified() {
        let rates = RateTable::unavailable();
        let today = Local::now().date_naive();
        let mut aggregator = Aggregator::new(today, today);
        let mut record = UsageRecord {
            provider: UsageProvider::Codex,
            timestamp_ms: Local::now().timestamp_millis(),
            model: "gpt-5.3-codex".to_owned(),
            session_id: String::new(),
            totals: TokenTotals {
                uncached_input: 100,
                ..TokenTotals::default()
            },
            reported_cost_usd: None,
            dedupe_key: None,
        };
        aggregator.add(&record, &rates);
        // Ten days ago falls outside a one-day window.
        record.timestamp_ms -= 10 * 86_400_000;
        aggregator.add(&record, &rates);

        let history = derive_history(
            aggregator,
            1,
            today,
            today,
            PricingStatus::Unavailable,
            0,
            0,
            Vec::new(),
            Duration::ZERO,
        );
        assert_eq!(history.records, 1);
        assert_eq!(history.sessions, 0, "an empty session id is not a session");
        assert!((history.quality.unpriced_share - 1.0).abs() < f64::EPSILON);
        assert_eq!(history.cost_usd, 0.0);
    }

    #[test]
    fn model_names_normalize_and_family_names_stay_unpriced() {
        assert_eq!(
            normalize_model_name("anthropic/Claude-Fable-5"),
            "claude-fable-5"
        );
        assert_eq!(normalize_model_name("  gpt-5.3-codex "), "gpt-5.3-codex");
        let rates = rate_table(&[("fable", FLAT_RATE), ("claude-fable-5", FLAT_RATE)]);
        assert!(
            lookup_rate(&rates, "Fable").is_none(),
            "bare family names are ambiguous"
        );
        assert!(lookup_rate(&rates, "<synthetic>").is_none());
        assert!(lookup_rate(&rates, "anthropic/claude-fable-5").is_some());
    }

    #[test]
    fn rate_tables_parse_and_round_trip_through_the_disk_cache() {
        let document: Value = serde_json::from_str(
            r#"{
                "claude-fable-5": {"input_cost_per_token": 1e-6, "output_cost_per_token": 2e-6,
                    "cache_read_input_token_cost": 1e-7, "cache_creation_input_token_cost": 1.25e-6},
                "no-output-rate": {"input_cost_per_token": 1e-6},
                "gpt-5.3-codex": {"input_cost_per_token": 3e-6, "output_cost_per_token": 9e-6}
            }"#,
        )
        .unwrap();
        let rates = parse_rate_table(&document);
        assert_eq!(rates.len(), 2, "half-priced models are dropped");
        // Missing cache rates fall back to the input rate, not to free.
        assert_eq!(rates["gpt-5.3-codex"].cache_read, 3e-6);

        let dir = std::env::temp_dir().join(format!("waku-usage-rates-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(RATES_CACHE_FILE);
        write_rates_cache(&path, 12345, &rates);
        let (fetched_at_ms, restored) = read_rates_cache(&path).expect("cache round-trips");
        assert_eq!(fetched_at_ms, 12345);
        assert_eq!(restored.len(), rates.len());
        assert_eq!(restored["claude-fable-5"], rates["claude-fable-5"]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn day_enumeration_is_inclusive() {
        let since = NaiveDate::from_ymd_opt(2026, 8, 1).unwrap();
        let until = NaiveDate::from_ymd_opt(2026, 8, 3).unwrap();
        let days = enumerate_days(since, until);
        assert_eq!(days.len(), 3);
        assert_eq!(days[0], since);
        assert_eq!(days[2], until);
        assert_eq!(enumerate_days(until, since), Vec::<NaiveDate>::new());
    }
}
