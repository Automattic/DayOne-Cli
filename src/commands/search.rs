use std::collections::{HashMap, HashSet};
use std::sync::LazyLock;

use anyhow::{Context, Result, anyhow};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

#[cfg(test)]
use crate::entry_embeddings::ENTRY_EMBEDDING_DIMENSION;
use crate::entry_embeddings::{embeddings_enabled, entry_searchable_text, query_embedding};
use crate::store::sqlite::Store;

const DEFAULT_THRESHOLD: f64 = 0.3;
const KEYWORD_MATCH_SCORE: f64 = 0.51;
static YYYY_MM_DD_DATE_FORMAT: LazyLock<Vec<time::format_description::FormatItem<'static>>> =
    LazyLock::new(|| {
        time::format_description::parse("[year]-[month]-[day]").expect("date format must be valid")
    });

#[derive(Debug)]
pub struct SearchContextItemsArgs {
    pub query: String,
    pub threshold: f64,
    pub limit: Option<usize>,
    pub category: Option<String>,
}

#[derive(Debug)]
pub struct SearchEntriesArgs {
    pub query: String,
    pub threshold: f64,
    pub limit: Option<usize>,
    pub journal_ids: Vec<String>,
    pub tags: Vec<String>,
    pub date_from: Option<String>,
    pub date_to: Option<String>,
    pub favorite: bool,
    pub has_checklist: bool,
    pub places: Vec<String>,
    pub media: Vec<SearchMediaType>,
    pub prompt_ids: Vec<String>,
    pub template_ids: Vec<String>,
    pub creation_devices: Vec<String>,
    pub weather_codes: Vec<String>,
    pub music_artists: Vec<String>,
    pub activities: Vec<String>,
    pub sort_by: SearchEntriesSortBy,
    pub direction: SearchSortDirection,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "camelCase")]
pub enum SearchMediaType {
    Image,
    Video,
    Audio,
    PdfAttachment,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum SearchEntriesSortBy {
    Relevancy,
    EntryDate,
    EditDate,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum SearchSortDirection {
    Asc,
    Desc,
}

#[derive(Debug, Serialize)]
pub struct SearchContextItemsOutput {
    pub ok: bool,
    pub query: String,
    pub threshold: f64,
    pub limit: Option<usize>,
    pub category: Option<String>,
    pub count: usize,
    pub items: Vec<SearchResultItem>,
}

#[derive(Debug, Serialize)]
pub struct SearchEntriesOutput {
    pub ok: bool,
    pub query: String,
    pub threshold: f64,
    pub limit: Option<usize>,
    pub journal_id: Option<String>,
    pub effective_params: SearchEntriesEffectiveParams,
    pub count: usize,
    pub items: Vec<SearchResultItem>,
}

#[derive(Debug, Serialize)]
pub struct SearchEntriesEffectiveParams {
    pub query: String,
    pub threshold: f64,
    pub limit: Option<usize>,
    pub journal_ids: Vec<String>,
    pub tags: Vec<String>,
    pub date_from: Option<String>,
    pub date_to: Option<String>,
    pub date_from_epoch_ms: Option<i64>,
    pub date_to_epoch_ms: Option<i64>,
    pub favorite: bool,
    pub has_checklist: bool,
    pub places: Vec<String>,
    pub media: Vec<SearchMediaType>,
    pub prompt_ids: Vec<String>,
    pub template_ids: Vec<String>,
    pub creation_devices: Vec<String>,
    pub weather_codes: Vec<String>,
    pub music_artists: Vec<String>,
    pub activities: Vec<String>,
    pub sort_by: SearchEntriesSortBy,
    pub direction: SearchSortDirection,
    pub wildcard_query: bool,
}

#[derive(Debug)]
struct EntrySearchFilters {
    journal_ids: HashSet<String>,
    tags: HashSet<String>,
    date_from_ms: Option<i64>,
    date_to_ms: Option<i64>,
    favorite: bool,
    has_checklist: bool,
    places: HashSet<String>,
    media: HashSet<SearchMediaType>,
    prompt_ids: HashSet<String>,
    template_ids: HashSet<String>,
    creation_devices: HashSet<String>,
    weather_codes: HashSet<String>,
    music_artists: HashSet<String>,
    activities: HashSet<String>,
}

impl EntrySearchFilters {
    fn has_non_query_filter(&self) -> bool {
        !self.journal_ids.is_empty()
            || !self.tags.is_empty()
            || self.date_from_ms.is_some()
            || self.date_to_ms.is_some()
            || self.favorite
            || self.has_checklist
            || !self.places.is_empty()
            || !self.media.is_empty()
            || !self.prompt_ids.is_empty()
            || !self.template_ids.is_empty()
            || !self.creation_devices.is_empty()
            || !self.weather_codes.is_empty()
            || !self.music_artists.is_empty()
            || !self.activities.is_empty()
    }
}

#[derive(Debug)]
struct NormalizedEntrySearch {
    query: String,
    threshold: f64,
    limit: Option<usize>,
    sort_by: SearchEntriesSortBy,
    direction: SearchSortDirection,
    wildcard_query: bool,
    filters: EntrySearchFilters,
    effective_params: SearchEntriesEffectiveParams,
}

#[derive(Debug, Clone, Copy)]
enum ParsedBoundKind {
    LowerInclusive,
    UpperInclusive,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EntryDateSortMode {
    EntryDate,
    EditDate,
}

#[derive(Debug, Serialize)]
pub struct SearchResultItem {
    pub id: String,
    pub similarity: f64,
    pub item: Value,
}

#[derive(Debug, Deserialize, Clone)]
struct ContextItem {
    id: String,
    content: String,
    category: String,
    #[serde(default)]
    embedding: Option<Vec<f64>>,
    #[serde(default)]
    deleted_at: Option<String>,
}

#[derive(Debug, Clone)]
struct ScoredItem {
    id: String,
    similarity: f64,
}

#[derive(Debug, Clone)]
struct EntryCandidate {
    id: String,
    text: String,
    value: Value,
}

pub fn context_items(
    store: &Store,
    args: SearchContextItemsArgs,
) -> Result<SearchContextItemsOutput> {
    let threshold = normalize_threshold(args.threshold);
    let query = args.query.trim().to_owned();
    if query.is_empty() {
        return Err(anyhow!("query must not be empty"));
    }

    let rows = store.list_json_rows("context_library_items")?;
    let parsed_rows = parse_context_items(rows)?;
    let candidates = filter_candidates(parsed_rows, args.category.as_deref());

    let keyword_matches = find_items_by_keyword_match(&query, &candidates);
    let semantic_matches = find_items_by_semantic_similarity(&query, threshold, &candidates);
    let mut merged = merge_results(keyword_matches, semantic_matches);

    if let Some(limit) = args.limit {
        merged.truncate(limit);
    }

    let mut row_by_id: HashMap<String, Value> = HashMap::new();
    for (_, row) in candidates {
        row_by_id.insert(value_id(&row).unwrap_or_default(), row);
    }

    let mut items = Vec::with_capacity(merged.len());
    for scored in merged {
        if let Some(item) = row_by_id.get(&scored.id) {
            items.push(SearchResultItem {
                id: scored.id,
                similarity: scored.similarity,
                item: remove_embedding_field(item.clone()),
            });
        }
    }

    Ok(SearchContextItemsOutput {
        ok: true,
        query,
        threshold,
        limit: args.limit,
        category: args.category,
        count: items.len(),
        items,
    })
}

pub fn entries(store: &Store, args: SearchEntriesArgs) -> Result<SearchEntriesOutput> {
    let search = normalize_entry_search(args)?;
    let query = search.query.clone();
    let threshold = search.threshold;

    let semantic_journal_hint = if search.filters.journal_ids.len() == 1 {
        search.filters.journal_ids.iter().next().map(String::as_str)
    } else {
        None
    };
    let rows = if search.effective_params.journal_ids.is_empty() {
        store.list_entries_with_embeddings(None, None)?
    } else if let Some(journal_id) = semantic_journal_hint {
        store.list_entries_with_embeddings(Some(journal_id), None)?
    } else {
        let mut scoped_rows = Vec::new();
        for journal_id in &search.effective_params.journal_ids {
            scoped_rows.extend(store.list_entries_with_embeddings(Some(journal_id), None)?);
        }
        scoped_rows
    };
    let mut candidates = Vec::with_capacity(rows.len());
    for (row, _) in rows {
        let value: Value =
            serde_json::from_str(&row).context("failed to decode stored entry row as JSON")?;
        if value.get("deleted_at").and_then(Value::as_str).is_some() {
            continue;
        }
        let Some(id) = value_id(&value) else {
            continue;
        };
        let text = entry_searchable_text(&value);
        if !search.wildcard_query && text.trim().is_empty() {
            continue;
        }
        if !entry_matches_filters(&value, &id, &text, &search.filters) {
            continue;
        }
        candidates.push(EntryCandidate { id, text, value });
    }

    let mut row_by_id: HashMap<String, Value> = HashMap::new();
    for candidate in &candidates {
        row_by_id.insert(candidate.id.clone(), candidate.value.clone());
    }

    let mut merged = if search.wildcard_query {
        candidates
            .iter()
            .map(|candidate| ScoredItem {
                id: candidate.id.clone(),
                similarity: 0.0,
            })
            .collect::<Vec<_>>()
    } else {
        let keyword_matches = find_entry_keyword_matches(&query, &candidates);
        let semantic_matches = find_entry_semantic_matches_for_journals(
            store,
            &query,
            threshold,
            &search.effective_params.journal_ids,
        )?;
        merge_results(keyword_matches, semantic_matches)
    };
    merged.retain(|scored| row_by_id.contains_key(&scored.id));
    sort_entry_results(
        &mut merged,
        &row_by_id,
        search.sort_by,
        search.direction,
        search.wildcard_query,
    );
    if let Some(limit) = search.limit {
        merged.truncate(limit);
    }

    let mut items = Vec::with_capacity(merged.len());
    for scored in merged {
        if let Some(item) = row_by_id.get(&scored.id) {
            items.push(SearchResultItem {
                id: scored.id,
                similarity: scored.similarity,
                item: remove_embedding_field(item.clone()),
            });
        }
    }

    Ok(SearchEntriesOutput {
        ok: true,
        query,
        threshold,
        limit: search.limit,
        journal_id: (search.effective_params.journal_ids.len() == 1)
            .then(|| search.effective_params.journal_ids[0].clone()),
        effective_params: search.effective_params,
        count: items.len(),
        items,
    })
}

fn normalize_entry_search(args: SearchEntriesArgs) -> Result<NormalizedEntrySearch> {
    let query = args.query.trim().to_owned();
    let threshold = normalize_threshold(args.threshold);
    let limit = args.limit;
    let sort_by = args.sort_by;
    let direction = args.direction;

    let journal_ids = normalize_string_values(args.journal_ids, false);
    let tags = normalize_string_values(args.tags, true);
    let places = normalize_string_values(args.places, true);
    let prompt_ids = normalize_string_values(args.prompt_ids, false);
    let template_ids = normalize_string_values(args.template_ids, false);
    let creation_devices = normalize_string_values(args.creation_devices, true);
    let weather_codes = normalize_string_values(args.weather_codes, true);
    let music_artists = normalize_string_values(args.music_artists, true);
    let activities = normalize_string_values(args.activities, true);
    let media = normalize_media_values(args.media);

    let date_from = normalize_optional_string(args.date_from);
    let date_to = normalize_optional_string(args.date_to);
    let date_from_ms = parse_search_bound(date_from.as_deref(), ParsedBoundKind::LowerInclusive)?;
    let date_to_ms = parse_search_bound(date_to.as_deref(), ParsedBoundKind::UpperInclusive)?;
    if let (Some(from), Some(to)) = (date_from_ms, date_to_ms)
        && from > to
    {
        return Err(anyhow!(
            "`--date-from` must be less than or equal to `--date-to`"
        ));
    }

    let filters = EntrySearchFilters {
        journal_ids: journal_ids.iter().cloned().collect(),
        tags: tags.iter().cloned().collect(),
        date_from_ms,
        date_to_ms,
        favorite: args.favorite,
        has_checklist: args.has_checklist,
        places: places.iter().cloned().collect(),
        media: media.iter().copied().collect(),
        prompt_ids: prompt_ids.iter().cloned().collect(),
        template_ids: template_ids.iter().cloned().collect(),
        creation_devices: creation_devices.iter().cloned().collect(),
        weather_codes: weather_codes.iter().cloned().collect(),
        music_artists: music_artists.iter().cloned().collect(),
        activities: activities.iter().cloned().collect(),
    };
    let wildcard_query = query.is_empty();
    if wildcard_query && !filters.has_non_query_filter() {
        return Err(anyhow!(
            "query must not be empty unless at least one filter flag is provided"
        ));
    }

    let effective_params = SearchEntriesEffectiveParams {
        query: query.clone(),
        threshold,
        limit,
        journal_ids,
        tags,
        date_from,
        date_to,
        date_from_epoch_ms: date_from_ms,
        date_to_epoch_ms: date_to_ms,
        favorite: args.favorite,
        has_checklist: args.has_checklist,
        places,
        media,
        prompt_ids,
        template_ids,
        creation_devices,
        weather_codes,
        music_artists,
        activities,
        sort_by,
        direction,
        wildcard_query,
    };

    Ok(NormalizedEntrySearch {
        query,
        threshold,
        limit,
        sort_by,
        direction,
        wildcard_query,
        filters,
        effective_params,
    })
}

fn normalize_optional_string(value: Option<String>) -> Option<String> {
    value
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}

fn normalize_string_values(values: Vec<String>, lowercase: bool) -> Vec<String> {
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    for value in values {
        let trimmed = value.trim();
        if trimmed.is_empty() {
            continue;
        }
        let normalized = if lowercase {
            trimmed.to_lowercase()
        } else {
            trimmed.to_owned()
        };
        if seen.insert(normalized.clone()) {
            out.push(normalized);
        }
    }
    out
}

fn normalize_media_values(values: Vec<SearchMediaType>) -> Vec<SearchMediaType> {
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    for value in values {
        if seen.insert(value) {
            out.push(value);
        }
    }
    out
}

fn parse_search_bound(value: Option<&str>, kind: ParsedBoundKind) -> Result<Option<i64>> {
    value
        .map(|raw| parse_date_to_epoch_ms(raw, kind))
        .transpose()
}

fn parse_date_to_epoch_ms(raw: &str, kind: ParsedBoundKind) -> Result<i64> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(anyhow!("date bounds must not be empty"));
    }
    if let Ok(value) = trimmed.parse::<i64>() {
        return Ok(value);
    }
    if let Ok(value) = trimmed.parse::<f64>() {
        if !value.is_finite() || value.fract() != 0.0 {
            return Err(anyhow!(
                "numeric date bounds must be finite integer epoch milliseconds"
            ));
        }
        if value < i64::MIN as f64 || value > i64::MAX as f64 {
            return Err(anyhow!("date value is out of supported range"));
        }
        return Ok(value as i64);
    }
    if let Ok(dt) = OffsetDateTime::parse(trimmed, &Rfc3339) {
        let millis = dt.unix_timestamp_nanos().div_euclid(1_000_000);
        return i64::try_from(millis).context("date value is out of supported range");
    }

    if let Ok(date) = time::Date::parse(trimmed, &YYYY_MM_DD_DATE_FORMAT) {
        let midnight = date
            .with_hms(0, 0, 0)
            .context("failed to parse date bound as midnight timestamp")?
            .assume_utc();
        let start_ms = i64::try_from(midnight.unix_timestamp_nanos().div_euclid(1_000_000))
            .context("date value is out of supported range")?;
        return match kind {
            ParsedBoundKind::LowerInclusive => Ok(start_ms),
            ParsedBoundKind::UpperInclusive => {
                let Some(next_day) = date.next_day() else {
                    return Err(anyhow!("date value is out of supported range"));
                };
                let next_midnight = next_day
                    .with_hms(0, 0, 0)
                    .context("failed to parse upper date bound as midnight timestamp")?
                    .assume_utc();
                let next_start_ms =
                    i64::try_from(next_midnight.unix_timestamp_nanos().div_euclid(1_000_000))
                        .context("date value is out of supported range")?;
                Ok(next_start_ms.saturating_sub(1))
            }
        };
    }

    Err(anyhow!(
        "date bound must be epoch milliseconds, YYYY-MM-DD, or RFC3339"
    ))
}

fn entry_matches_filters(
    value: &Value,
    id: &str,
    text: &str,
    filters: &EntrySearchFilters,
) -> bool {
    if !filters.tags.is_empty() && !entry_matches_tag_filter(value, &filters.tags) {
        return false;
    }
    if (filters.date_from_ms.is_some() || filters.date_to_ms.is_some())
        && !entry_matches_date_filter(value, filters.date_from_ms, filters.date_to_ms)
    {
        return false;
    }
    if filters.favorite && !entry_is_favorite(value) {
        return false;
    }
    if filters.has_checklist && !entry_has_checklist(value, text) {
        return false;
    }
    if !filters.places.is_empty() && !entry_matches_place_filter(value, &filters.places) {
        return false;
    }
    if !filters.media.is_empty() && !entry_matches_media_filter(value, &filters.media) {
        return false;
    }
    if !filters.prompt_ids.is_empty()
        && !entry_matches_id_filter(
            value,
            &["promptID", "promptId", "prompt_id"],
            &filters.prompt_ids,
        )
    {
        return false;
    }
    if !filters.template_ids.is_empty()
        && !entry_matches_id_filter(
            value,
            &["templateID", "templateId", "template_id"],
            &filters.template_ids,
        )
    {
        return false;
    }
    if !filters.creation_devices.is_empty()
        && !entry_matches_creation_device_filter(value, &filters.creation_devices)
    {
        return false;
    }
    if !filters.weather_codes.is_empty()
        && !entry_matches_nested_string_filter(value, &["weather", "code"], &filters.weather_codes)
    {
        return false;
    }
    if !filters.music_artists.is_empty()
        && !entry_matches_nested_string_filter(value, &["music", "artist"], &filters.music_artists)
    {
        return false;
    }
    if !filters.activities.is_empty()
        && !entry_matches_string_filter(value, &["activity"], &filters.activities)
    {
        return false;
    }

    // Id is currently unused by filters, but we intentionally keep it in the signature for
    // potential id-scoped filters and easier debugging when adding future filter dimensions.
    let _ = id;
    true
}

fn entry_matches_tag_filter(value: &Value, tags: &HashSet<String>) -> bool {
    let Some(raw_tags) = value_by_keys(value, &["tags"]).and_then(Value::as_array) else {
        return false;
    };
    raw_tags
        .iter()
        .filter_map(Value::as_str)
        .map(str::trim)
        .filter(|tag| !tag.is_empty())
        .map(str::to_lowercase)
        .any(|tag| tags.contains(&tag))
}

fn entry_matches_date_filter(value: &Value, min_ms: Option<i64>, max_ms: Option<i64>) -> bool {
    let Some(entry_date) = extract_entry_date_ms(value) else {
        return false;
    };
    if let Some(min_value) = min_ms
        && entry_date < min_value
    {
        return false;
    }
    if let Some(max_value) = max_ms
        && entry_date > max_value
    {
        return false;
    }
    true
}

fn entry_is_favorite(value: &Value) -> bool {
    value_by_keys(value, &["starred", "isStarred", "favorite"])
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

fn entry_has_checklist(value: &Value, text: &str) -> bool {
    if contains_checklist_markers(text) {
        return true;
    }
    if let Some(body) = value.get("body").and_then(Value::as_str)
        && contains_checklist_markers(body)
    {
        return true;
    }
    if let Some(rich_text) =
        value_by_keys(value, &["richTextJSON", "richTextJson", "rich_text_json"])
    {
        let serialized = match rich_text {
            Value::String(raw) => raw.to_lowercase(),
            other => other.to_string().to_lowercase(),
        };
        if serialized.contains("\"checklist\"")
            || serialized.contains("\"checklistitem\"")
            || serialized.contains("\"tasklist\"")
        {
            return true;
        }
    }
    false
}

fn contains_checklist_markers(input: &str) -> bool {
    input.lines().map(str::trim_start).any(|line| {
        line.starts_with("- [ ]")
            || line.starts_with("- [x]")
            || line.starts_with("- [X]")
            || line.starts_with("* [ ]")
            || line.starts_with("* [x]")
            || line.starts_with("* [X]")
    })
}

fn entry_matches_place_filter(value: &Value, places: &HashSet<String>) -> bool {
    let Some(place) =
        value_by_nested_path(value, &["location", "placeName"]).and_then(Value::as_str)
    else {
        return false;
    };
    places.contains(&place.trim().to_lowercase())
}

fn entry_matches_media_filter(value: &Value, media: &HashSet<SearchMediaType>) -> bool {
    let Some(moments) = value_by_keys(value, &["moments"]).and_then(Value::as_array) else {
        return false;
    };
    moments
        .iter()
        .filter_map(|moment| {
            moment
                .get("type")
                .or_else(|| moment.get("momentType"))
                .or_else(|| moment.get("moment_type"))
                .and_then(Value::as_str)
                .and_then(parse_media_type)
        })
        .any(|kind| media.contains(&kind))
}

fn parse_media_type(value: &str) -> Option<SearchMediaType> {
    match value.trim().to_ascii_lowercase().as_str() {
        "image" | "photo" => Some(SearchMediaType::Image),
        "video" => Some(SearchMediaType::Video),
        "audio" => Some(SearchMediaType::Audio),
        "pdfattachment" => Some(SearchMediaType::PdfAttachment),
        _ => None,
    }
}

fn entry_matches_id_filter(value: &Value, keys: &[&str], candidates: &HashSet<String>) -> bool {
    let Some(raw_value) = first_string_from_keys(value, keys) else {
        return false;
    };
    candidates.contains(raw_value)
}

fn entry_matches_creation_device_filter(value: &Value, devices: &HashSet<String>) -> bool {
    let client_meta_matches = [
        first_nested_string(value, &["clientMeta", "creationDevice"]),
        first_nested_string(value, &["clientMeta", "deviceName"]),
    ]
    .into_iter()
    .flatten()
    .map(|value| value.to_lowercase())
    .any(|value| devices.contains(&value));
    if client_meta_matches {
        return true;
    }

    let Some(moments) = value_by_keys(value, &["moments"]).and_then(Value::as_array) else {
        return false;
    };
    moments
        .iter()
        .filter_map(|moment| moment.get("creationDevice").and_then(Value::as_str))
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_lowercase)
        .any(|value| devices.contains(&value))
}

fn entry_matches_nested_string_filter(
    value: &Value,
    path: &[&str],
    filters: &HashSet<String>,
) -> bool {
    let Some(raw_value) = first_nested_string(value, path) else {
        return false;
    };
    filters.contains(&raw_value.to_lowercase())
}

fn entry_matches_string_filter(value: &Value, keys: &[&str], filters: &HashSet<String>) -> bool {
    let Some(raw_value) = first_string_from_keys(value, keys) else {
        return false;
    };
    filters.contains(&raw_value.to_lowercase())
}

fn value_by_keys<'a>(value: &'a Value, keys: &[&str]) -> Option<&'a Value> {
    for key in keys {
        if let Some(candidate) = value.get(*key)
            && !candidate.is_null()
        {
            return Some(candidate);
        }
    }
    let payload = value.get("payload")?;
    for key in keys {
        if let Some(candidate) = payload.get(*key)
            && !candidate.is_null()
        {
            return Some(candidate);
        }
    }
    None
}

fn value_by_nested_path<'a>(value: &'a Value, path: &[&str]) -> Option<&'a Value> {
    get_nested_value(value, path)
        .filter(|candidate| !candidate.is_null())
        .or_else(|| {
            value
                .get("payload")
                .and_then(|payload| get_nested_value(payload, path))
                .filter(|candidate| !candidate.is_null())
        })
}

fn get_nested_value<'a>(value: &'a Value, path: &[&str]) -> Option<&'a Value> {
    let mut current = value;
    for key in path {
        current = current.get(*key)?;
    }
    Some(current)
}

fn first_string_from_keys<'a>(value: &'a Value, keys: &[&str]) -> Option<&'a str> {
    value_by_keys(value, keys)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

fn first_nested_string<'a>(value: &'a Value, path: &[&str]) -> Option<&'a str> {
    value_by_nested_path(value, path)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

fn extract_entry_date_ms(value: &Value) -> Option<i64> {
    value_by_keys(value, &["date"]).and_then(parse_entry_datetime_value)
}

fn extract_entry_edit_date_ms(value: &Value) -> Option<i64> {
    value_by_keys(
        value,
        &["user_edit_date", "userEditDate", "updated_at", "updatedAt"],
    )
    .and_then(parse_entry_datetime_value)
    .or_else(|| extract_entry_date_ms(value))
}

fn parse_entry_datetime_value(value: &Value) -> Option<i64> {
    match value {
        Value::Number(number) => number
            .as_i64()
            .or_else(|| number.as_f64().map(|raw| raw as i64)),
        Value::String(raw) => parse_date_to_epoch_ms(raw, ParsedBoundKind::LowerInclusive).ok(),
        _ => None,
    }
}

fn sort_entry_results(
    merged: &mut [ScoredItem],
    row_by_id: &HashMap<String, Value>,
    sort_by: SearchEntriesSortBy,
    direction: SearchSortDirection,
    wildcard_query: bool,
) {
    match (sort_by, wildcard_query) {
        (SearchEntriesSortBy::Relevancy, false) => {
            merged.sort_by(|a, b| {
                let order = match direction {
                    SearchSortDirection::Asc => a.similarity.total_cmp(&b.similarity),
                    SearchSortDirection::Desc => b.similarity.total_cmp(&a.similarity),
                };
                if order.is_eq() {
                    a.id.cmp(&b.id)
                } else {
                    order
                }
            });
        }
        (SearchEntriesSortBy::Relevancy, true) | (SearchEntriesSortBy::EntryDate, _) => {
            sort_entry_results_by_date(merged, row_by_id, EntryDateSortMode::EntryDate, direction);
        }
        (SearchEntriesSortBy::EditDate, _) => {
            sort_entry_results_by_date(merged, row_by_id, EntryDateSortMode::EditDate, direction);
        }
    }
}

fn sort_entry_results_by_date(
    merged: &mut [ScoredItem],
    row_by_id: &HashMap<String, Value>,
    mode: EntryDateSortMode,
    direction: SearchSortDirection,
) {
    merged.sort_by(|a, b| {
        let key_a = row_by_id
            .get(&a.id)
            .and_then(|value| match mode {
                EntryDateSortMode::EntryDate => extract_entry_date_ms(value),
                EntryDateSortMode::EditDate => extract_entry_edit_date_ms(value),
            })
            .unwrap_or(0);
        let key_b = row_by_id
            .get(&b.id)
            .and_then(|value| match mode {
                EntryDateSortMode::EntryDate => extract_entry_date_ms(value),
                EntryDateSortMode::EditDate => extract_entry_edit_date_ms(value),
            })
            .unwrap_or(0);
        let primary = match direction {
            SearchSortDirection::Asc => key_a.cmp(&key_b),
            SearchSortDirection::Desc => key_b.cmp(&key_a),
        };
        if primary.is_eq() {
            match direction {
                SearchSortDirection::Asc => a.id.cmp(&b.id),
                SearchSortDirection::Desc => b.id.cmp(&a.id),
            }
        } else {
            primary
        }
    });
}

fn parse_context_items(rows: Vec<String>) -> Result<Vec<(ContextItem, Value)>> {
    let mut parsed = Vec::with_capacity(rows.len());
    for row in rows {
        let raw_value: Value = serde_json::from_str(&row)
            .context("failed to decode stored context item row as JSON")?;
        let item: ContextItem = serde_json::from_value(raw_value.clone())
            .context("failed to decode context item fields")?;
        parsed.push((item, raw_value));
    }
    Ok(parsed)
}

fn filter_candidates(
    rows: Vec<(ContextItem, Value)>,
    category_filter: Option<&str>,
) -> Vec<(ContextItem, Value)> {
    rows.into_iter()
        .filter(|(item, _)| item.deleted_at.is_none())
        .filter(|(item, _)| {
            if let Some(category) = category_filter {
                item.category == category
            } else {
                true
            }
        })
        .collect()
}

fn find_items_by_keyword_match(
    query: &str,
    candidates: &[(ContextItem, Value)],
) -> Vec<ScoredItem> {
    candidates
        .iter()
        .filter(|(item, _)| has_word_boundary_match(&item.content, query))
        .map(|(item, _)| ScoredItem {
            id: item.id.clone(),
            similarity: KEYWORD_MATCH_SCORE,
        })
        .collect()
}

fn find_items_by_semantic_similarity(
    query: &str,
    threshold: f64,
    candidates: &[(ContextItem, Value)],
) -> Vec<ScoredItem> {
    let Some(query_dimension) = candidates
        .iter()
        .filter_map(|(item, _)| item.embedding.as_ref().map(|v| v.len()))
        .find(|len| *len > 0)
    else {
        return Vec::new();
    };
    let query_embedding = extract_query_embedding(query, query_dimension);
    candidates
        .iter()
        .filter_map(|(item, _)| {
            let embedding = item.embedding.as_ref()?;
            let similarity = cosine_similarity(&query_embedding, embedding)?;
            if similarity >= threshold {
                Some(ScoredItem {
                    id: item.id.clone(),
                    similarity,
                })
            } else {
                None
            }
        })
        .collect()
}

fn find_entry_keyword_matches(query: &str, candidates: &[EntryCandidate]) -> Vec<ScoredItem> {
    candidates
        .iter()
        .filter(|candidate| has_word_boundary_match(&candidate.text, query))
        .map(|candidate| ScoredItem {
            id: candidate.id.clone(),
            similarity: KEYWORD_MATCH_SCORE,
        })
        .collect()
}

fn find_entry_semantic_matches(
    store: &Store,
    query: &str,
    threshold: f64,
    journal_id: Option<&str>,
) -> Result<Vec<ScoredItem>> {
    if !embeddings_enabled() {
        return Ok(Vec::new());
    }
    let query_embedding = query_embedding(query)?;
    let rows = store.search_entry_semantic_scores(&query_embedding, threshold, journal_id)?;
    Ok(rows
        .into_iter()
        .map(|(id, similarity)| ScoredItem { id, similarity })
        .collect())
}

fn find_entry_semantic_matches_for_journals(
    store: &Store,
    query: &str,
    threshold: f64,
    journal_ids: &[String],
) -> Result<Vec<ScoredItem>> {
    if !embeddings_enabled() {
        return Ok(Vec::new());
    }
    if journal_ids.is_empty() {
        return find_entry_semantic_matches(store, query, threshold, None);
    }
    if journal_ids.len() == 1 {
        return find_entry_semantic_matches(store, query, threshold, Some(&journal_ids[0]));
    }
    let query_embedding = query_embedding(query)?;
    let mut merged = Vec::new();
    for journal_id in journal_ids {
        merged.extend(
            store
                .search_entry_semantic_scores(&query_embedding, threshold, Some(journal_id))?
                .into_iter()
                .map(|(id, similarity)| ScoredItem { id, similarity }),
        );
    }
    Ok(merged)
}

fn merge_results(
    keyword_matches: Vec<ScoredItem>,
    semantic_matches: Vec<ScoredItem>,
) -> Vec<ScoredItem> {
    let mut by_id: HashMap<String, f64> = HashMap::new();
    for item in keyword_matches.into_iter().chain(semantic_matches) {
        by_id
            .entry(item.id)
            .and_modify(|score| {
                if item.similarity > *score {
                    *score = item.similarity;
                }
            })
            .or_insert(item.similarity);
    }

    let mut merged: Vec<ScoredItem> = by_id
        .into_iter()
        .map(|(id, similarity)| ScoredItem { id, similarity })
        .collect();
    merged.sort_by(|a, b| {
        b.similarity
            .total_cmp(&a.similarity)
            .then_with(|| a.id.cmp(&b.id))
    });
    merged
}

fn normalize_threshold(threshold: f64) -> f64 {
    if threshold.is_finite() {
        threshold.clamp(-1.0, 1.0)
    } else {
        DEFAULT_THRESHOLD
    }
}

fn extract_query_embedding(query: &str, dimension: usize) -> Vec<f64> {
    if dimension == 0 {
        return Vec::new();
    }
    let mut counts: HashMap<String, f64> = HashMap::new();
    for token in tokenize(query) {
        *counts.entry(token).or_insert(0.0) += 1.0;
    }

    let mut embedding = vec![0.0_f64; dimension];
    for (token, count) in counts {
        let idx = hash_token_to_index(&token, embedding.len());
        embedding[idx] += count;
    }
    normalize_vector(&mut embedding);
    embedding
}

fn tokenize(input: &str) -> Vec<String> {
    input
        .split(|c: char| !(c.is_alphanumeric() || c == '_'))
        .filter(|token| !token.is_empty())
        .map(|token| token.to_lowercase())
        .collect()
}

fn hash_token_to_index(token: &str, size: usize) -> usize {
    let mut hash = 2166136261u32;
    for b in token.as_bytes() {
        hash ^= *b as u32;
        hash = hash.wrapping_mul(16777619);
    }
    (hash as usize) % size
}

fn normalize_vector(values: &mut [f64]) {
    let norm = values.iter().map(|v| v * v).sum::<f64>().sqrt();
    if norm == 0.0 {
        return;
    }
    for value in values {
        *value /= norm;
    }
}

fn cosine_similarity(vector_a: &[f64], vector_b: &[f64]) -> Option<f64> {
    if vector_a.len() != vector_b.len() || vector_a.is_empty() {
        return None;
    }

    let mut dot_product = 0.0_f64;
    let mut norm_a = 0.0_f64;
    let mut norm_b = 0.0_f64;
    for (a, b) in vector_a.iter().zip(vector_b) {
        dot_product += a * b;
        norm_a += a * a;
        norm_b += b * b;
    }
    if norm_a == 0.0 || norm_b == 0.0 {
        return Some(0.0);
    }

    Some(dot_product / (norm_a.sqrt() * norm_b.sqrt()))
}

fn has_word_boundary_match(content: &str, query: &str) -> bool {
    let content_lower = content.to_lowercase();
    let query_lower = query.trim().to_lowercase();
    if query_lower.is_empty() {
        return false;
    }

    content_lower
        .match_indices(&query_lower)
        .any(|(start, matched)| {
            let end = start + matched.len();
            let left_ok = content_lower[..start]
                .chars()
                .next_back()
                .is_none_or(is_word_boundary_char);
            let right_ok = content_lower[end..]
                .chars()
                .next()
                .is_none_or(is_word_boundary_char);
            left_ok && right_ok
        })
}

fn is_word_boundary_char(c: char) -> bool {
    !(c.is_alphanumeric() || c == '_')
}

fn value_id(value: &Value) -> Option<String> {
    value
        .get("id")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
}

fn remove_embedding_field(mut value: Value) -> Value {
    if let Some(obj) = value.as_object_mut() {
        obj.remove("embedding");
    }
    value
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn test_store_path(name: &str) -> PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time should be valid")
            .as_nanos();
        std::env::temp_dir().join(format!("dayone-cli-search-{name}-{unique}.db"))
    }

    fn base_search_args(query: &str) -> SearchEntriesArgs {
        SearchEntriesArgs {
            query: query.to_owned(),
            threshold: -1.0,
            limit: Some(50),
            journal_ids: Vec::new(),
            tags: Vec::new(),
            date_from: None,
            date_to: None,
            favorite: false,
            has_checklist: false,
            places: Vec::new(),
            media: Vec::new(),
            prompt_ids: Vec::new(),
            template_ids: Vec::new(),
            creation_devices: Vec::new(),
            weather_codes: Vec::new(),
            music_artists: Vec::new(),
            activities: Vec::new(),
            sort_by: SearchEntriesSortBy::Relevancy,
            direction: SearchSortDirection::Desc,
        }
    }

    #[test]
    fn cosine_similarity_returns_none_for_vector_size_mismatch() {
        let left = vec![1.0, 0.0, 0.0];
        let right = vec![1.0, 0.0];
        assert!(cosine_similarity(&left, &right).is_none());
    }

    #[test]
    fn keyword_match_uses_word_boundaries() {
        assert!(has_word_boundary_match(
            "I play tennis on Saturdays",
            "tennis"
        ));
        assert!(!has_word_boundary_match("I love my tennisshoes", "tennis"));
    }

    #[test]
    fn merge_results_keeps_highest_score_and_respects_sorting() {
        let keyword_matches = vec![
            ScoredItem {
                id: "a".to_owned(),
                similarity: 0.51,
            },
            ScoredItem {
                id: "b".to_owned(),
                similarity: 0.51,
            },
        ];
        let semantic_matches = vec![
            ScoredItem {
                id: "a".to_owned(),
                similarity: 0.91,
            },
            ScoredItem {
                id: "c".to_owned(),
                similarity: 0.81,
            },
        ];

        let merged = merge_results(keyword_matches, semantic_matches);
        let ids: Vec<&str> = merged.iter().map(|item| item.id.as_str()).collect();
        assert_eq!(ids, vec!["a", "c", "b"]);
        assert_eq!(merged[0].similarity, 0.91);
    }

    #[test]
    fn remove_embedding_field_strips_embedding_only() {
        let value = json!({
            "id": "x",
            "content": "hello",
            "embedding": [0.1, 0.2]
        });
        let sanitized = remove_embedding_field(value);
        assert_eq!(sanitized.get("id").and_then(Value::as_str), Some("x"));
        assert_eq!(
            sanitized.get("content").and_then(Value::as_str),
            Some("hello")
        );
        assert!(sanitized.get("embedding").is_none());
    }

    #[test]
    fn entries_search_supports_semantic_and_keyword_ranking() {
        let path = test_store_path("entries-semantic");
        let store = Store::open_at(&path).expect("store should open");
        store
            .upsert_entry_json_row(
                "entry-1",
                "journal-1",
                Some("2026-03-17T12:00:00.000Z"),
                None,
                r#"{"id":"entry-1","journal_id":"journal-1","body":"I played tennis today"}"#,
            )
            .expect("entry 1 should save");
        store
            .upsert_entry_json_row(
                "entry-2",
                "journal-1",
                Some("2026-03-17T11:00:00.000Z"),
                None,
                r#"{"id":"entry-2","journal_id":"journal-1","body":"A random note"}"#,
            )
            .expect("entry 2 should save");
        store
            .upsert_entry_embedding(
                "entry-1",
                "journal-1",
                "hash-1",
                &vec![1.0; ENTRY_EMBEDDING_DIMENSION],
            )
            .expect("embedding should save");

        let mut args = base_search_args("tennis");
        args.limit = Some(10);
        args.journal_ids = vec!["journal-1".to_owned()];
        let output = entries(&store, args).expect("entries search should work");

        assert!(output.count >= 1);
        assert_eq!(output.items[0].id, "entry-1");
        assert_eq!(
            output.effective_params.journal_ids,
            vec!["journal-1".to_owned()]
        );
    }

    #[test]
    fn effective_params_serialize_with_web_style_enum_values() {
        let mut args = base_search_args("tennis");
        args.sort_by = SearchEntriesSortBy::EntryDate;
        args.direction = SearchSortDirection::Asc;
        args.media = vec![SearchMediaType::Image, SearchMediaType::PdfAttachment];
        let search = normalize_entry_search(args).expect("search args should normalize");
        let json = serde_json::to_value(&search.effective_params)
            .expect("effective params should serialize");

        assert_eq!(json["sort_by"], json!("entryDate"));
        assert_eq!(json["direction"], json!("asc"));
        assert_eq!(json["media"], json!(["image", "pdfAttachment"]));
    }

    #[test]
    fn entries_search_rejects_blank_query_without_filters() {
        let path = test_store_path("entries-empty-query");
        let store = Store::open_at(&path).expect("store should open");
        let err = entries(&store, base_search_args("   ")).expect_err("blank query should fail");
        assert!(err.to_string().contains("query must not be empty"));
    }

    #[test]
    fn entries_search_allows_blank_query_with_filters() {
        let path = test_store_path("entries-blank-query-filters");
        let store = Store::open_at(&path).expect("store should open");
        store
            .upsert_entry_json_row(
                "entry-1",
                "journal-1",
                Some("2026-03-17T12:00:00.000Z"),
                None,
                r#"{"id":"entry-1","journal_id":"journal-1","body":"Checklist entry\n- [ ] task","starred":true}"#,
            )
            .expect("entry 1 should save");
        store
            .upsert_entry_json_row(
                "entry-2",
                "journal-1",
                Some("2026-03-17T11:00:00.000Z"),
                None,
                r#"{"id":"entry-2","journal_id":"journal-1","body":"No checklist","starred":true}"#,
            )
            .expect("entry 2 should save");

        let mut args = base_search_args("");
        args.favorite = true;
        args.has_checklist = true;
        let output = entries(&store, args).expect("blank query with filters should work");
        assert_eq!(output.count, 1);
        assert_eq!(output.items[0].id, "entry-1");
        assert!(output.effective_params.wildcard_query);
    }

    #[test]
    fn entries_search_applies_combined_filters() {
        let path = test_store_path("entries-combined-filters");
        let store = Store::open_at(&path).expect("store should open");
        store
            .upsert_entry_json_row(
                "entry-a",
                "journal-1",
                Some("2026-03-17T12:00:00.000Z"),
                None,
                r#"{
                    "id":"entry-a",
                    "journal_id":"journal-1",
                    "date":1700000000000,
                    "body":"Trip notes",
                    "tags":["travel","morning"],
                    "starred":true,
                    "location":{"placeName":"Portland"},
                    "moments":[{"id":"m1","type":"photo"}],
                    "promptID":"prompt-1",
                    "templateID":"template-1",
                    "clientMeta":{"creationDevice":"iPhone 15 Pro"},
                    "weather":{"code":"clear-day"},
                    "music":{"artist":"Radiohead"},
                    "activity":"Walking"
                }"#,
            )
            .expect("entry-a should save");
        store
            .upsert_entry_json_row(
                "entry-b",
                "journal-2",
                Some("2026-03-17T11:00:00.000Z"),
                None,
                r#"{
                    "id":"entry-b",
                    "journal_id":"journal-2",
                    "date":1700000000000,
                    "body":"Trip notes",
                    "tags":["travel"],
                    "starred":true,
                    "location":{"placeName":"Portland"},
                    "moments":[{"id":"m2","type":"video"}],
                    "promptID":"prompt-1",
                    "templateID":"template-1",
                    "clientMeta":{"creationDevice":"iPhone 15 Pro"},
                    "weather":{"code":"clear-day"},
                    "music":{"artist":"Radiohead"},
                    "activity":"Walking"
                }"#,
            )
            .expect("entry-b should save");

        let mut args = base_search_args("");
        args.journal_ids = vec!["journal-1".to_owned()];
        args.tags = vec!["travel".to_owned()];
        args.date_from = Some("2023-11-14".to_owned());
        args.date_to = Some("2023-11-14".to_owned());
        args.favorite = true;
        args.places = vec!["portland".to_owned()];
        args.media = vec![SearchMediaType::Image];
        args.prompt_ids = vec!["prompt-1".to_owned()];
        args.template_ids = vec!["template-1".to_owned()];
        args.creation_devices = vec!["iphone 15 pro".to_owned()];
        args.weather_codes = vec!["clear-day".to_owned()];
        args.music_artists = vec!["radiohead".to_owned()];
        args.activities = vec!["walking".to_owned()];

        let output = entries(&store, args).expect("combined filters should match");
        assert_eq!(output.count, 1);
        assert_eq!(output.items[0].id, "entry-a");
    }

    #[test]
    fn entries_search_legacy_journal_id_is_none_for_multi_journal_filter() {
        let path = test_store_path("entries-journal-id-output");
        let store = Store::open_at(&path).expect("store should open");
        store
            .upsert_entry_json_row(
                "entry-a",
                "journal-1",
                Some("2026-03-17T12:00:00.000Z"),
                None,
                r#"{"id":"entry-a","journal_id":"journal-1","body":"Trip notes","tags":["x"]}"#,
            )
            .expect("entry-a should save");
        store
            .upsert_entry_json_row(
                "entry-b",
                "journal-2",
                Some("2026-03-17T11:00:00.000Z"),
                None,
                r#"{"id":"entry-b","journal_id":"journal-2","body":"Trip notes","tags":["x"]}"#,
            )
            .expect("entry-b should save");

        let mut args = base_search_args("");
        args.journal_ids = vec!["journal-1".to_owned(), "journal-2".to_owned()];
        args.tags = vec!["x".to_owned()];
        let output = entries(&store, args).expect("multi-journal search should work");
        assert_eq!(output.count, 2);
        assert!(output.journal_id.is_none());
        assert_eq!(output.effective_params.journal_ids.len(), 2);
    }

    #[test]
    fn entries_search_journal_filter_uses_store_scope_even_when_payload_journal_differs() {
        let path = test_store_path("entries-journal-store-scope");
        let store = Store::open_at(&path).expect("store should open");
        store
            .upsert_entry_json_row(
                "entry-a",
                "journal-1",
                Some("2026-03-17T12:00:00.000Z"),
                None,
                r#"{
                    "id":"entry-a",
                    "journalId":"SYNCABLE-JOURNAL-ID",
                    "body":"stillness and calm",
                    "tags":["focus"]
                }"#,
            )
            .expect("entry-a should save");

        let mut args = base_search_args("stillness");
        args.journal_ids = vec!["journal-1".to_owned()];
        let output = entries(&store, args).expect("journal-scoped search should work");
        assert_eq!(output.count, 1);
        assert_eq!(output.items[0].id, "entry-a");
    }

    #[test]
    fn entries_search_sorts_by_entry_date_with_direction() {
        let path = test_store_path("entries-sort-entry-date");
        let store = Store::open_at(&path).expect("store should open");
        store
            .upsert_entry_json_row(
                "entry-older",
                "journal-1",
                Some("2026-03-17T10:00:00.000Z"),
                None,
                r#"{"id":"entry-older","journal_id":"journal-1","date":1000,"body":"dated entry"}"#,
            )
            .expect("older should save");
        store
            .upsert_entry_json_row(
                "entry-newer",
                "journal-1",
                Some("2026-03-17T12:00:00.000Z"),
                None,
                r#"{"id":"entry-newer","journal_id":"journal-1","date":2000,"body":"dated entry"}"#,
            )
            .expect("newer should save");

        let mut asc_args = base_search_args("dated");
        asc_args.sort_by = SearchEntriesSortBy::EntryDate;
        asc_args.direction = SearchSortDirection::Asc;
        let asc_output = entries(&store, asc_args).expect("asc sort should work");
        assert_eq!(asc_output.items[0].id, "entry-older");

        let mut desc_args = base_search_args("dated");
        desc_args.sort_by = SearchEntriesSortBy::EntryDate;
        desc_args.direction = SearchSortDirection::Desc;
        let desc_output = entries(&store, desc_args).expect("desc sort should work");
        assert_eq!(desc_output.items[0].id, "entry-newer");
    }

    #[test]
    fn entries_search_sorts_by_edit_date_with_direction() {
        let path = test_store_path("entries-sort-edit-date");
        let store = Store::open_at(&path).expect("store should open");
        store
            .upsert_entry_json_row(
                "entry-first",
                "journal-1",
                Some("2026-03-17T10:00:00.000Z"),
                None,
                r#"{"id":"entry-first","journal_id":"journal-1","date":1000,"userEditDate":1500,"tags":["x"],"body":"edit date"}"#,
            )
            .expect("first should save");
        store
            .upsert_entry_json_row(
                "entry-second",
                "journal-1",
                Some("2026-03-17T12:00:00.000Z"),
                None,
                r#"{"id":"entry-second","journal_id":"journal-1","date":2000,"userEditDate":2500,"tags":["x"],"body":"edit date"}"#,
            )
            .expect("second should save");

        let mut args = base_search_args("");
        args.tags = vec!["x".to_owned()];
        args.sort_by = SearchEntriesSortBy::EditDate;
        args.direction = SearchSortDirection::Asc;
        let output = entries(&store, args).expect("edit date sort should work");
        assert_eq!(output.items[0].id, "entry-first");
        assert_eq!(output.items[1].id, "entry-second");
    }

    #[test]
    fn date_bound_parser_handles_day_bounds_and_rejects_invalid() {
        let start = parse_date_to_epoch_ms("2026-04-10", ParsedBoundKind::LowerInclusive)
            .expect("lower date bound should parse");
        let end = parse_date_to_epoch_ms("2026-04-10", ParsedBoundKind::UpperInclusive)
            .expect("upper date bound should parse");
        assert_eq!(end - start, 86_399_999);

        let err = parse_date_to_epoch_ms("04/10/2026", ParsedBoundKind::LowerInclusive)
            .expect_err("invalid date should fail");
        assert!(err.to_string().contains("date bound must be"));

        for raw in ["NaN", "inf", "-inf", "1700000000000.5"] {
            let err = parse_date_to_epoch_ms(raw, ParsedBoundKind::LowerInclusive)
                .expect_err("non-finite or fractional values should fail");
            assert!(
                err.to_string()
                    .contains("finite integer epoch milliseconds")
            );
        }
    }
}
