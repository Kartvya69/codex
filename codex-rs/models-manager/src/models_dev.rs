//! Best-effort enrichment of unknown model slugs via the public
//! <https://models.dev> catalog.
//!
//! When a configured model slug is absent from both the bundled catalog and
//! the provider `/models` endpoint, the manager falls back to a conservative
//! hardcoded [`ModelInfo`] (for example, a 272k context window). That fallback
//! is safe but degrades behavior for well-known third-party models and emits a
//! "model metadata not found" warning on every turn.
//!
//! To avoid that, we look the slug up in the models.dev catalog and persist
//! the derived metadata on disk so the lookup never repeats. Every network and
//! cache operation here is fail-safe: on any miss or error we simply hand back
//! the original fallback unchanged, so this feature can never leave things
//! worse than the existing behavior.

use std::collections::HashMap;
use std::path::Path;
use std::time::Duration;

use codex_protocol::openai_models::ModelInfo;
use serde::{Deserialize, Serialize};
use tracing::{info, warn};

const MODELS_DEV_URL: &str = "https://models.dev/models.json";
const CACHE_FILE_NAME: &str = "models_dev_cache.json";
const FETCH_TIMEOUT: Duration = Duration::from_secs(10);
/// How long a "not found" result is remembered before re-querying models.dev,
/// so an unknown slug does not trigger a network call on every session.
const NEGATIVE_CACHE_TTL: chrono::Duration = chrono::Duration::hours(24);

/// Subset of [`ModelInfo`] that models.dev can authoritatively inform. Stored
/// on disk so successful lookups (and "not found" results) never repeat.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct CachedModelMetadata {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    display_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    context_window: Option<i64>,
}

/// One persisted record per slug: either resolved metadata or a negative mark.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct CacheRecord {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    found: Option<CachedModelMetadata>,
    /// When the last models.dev lookup for this slug returned no match.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    not_found_at: Option<chrono::DateTime<chrono::Utc>>,
}

type CacheMap = HashMap<String, CacheRecord>;

/// Entry shape for <https://models.dev/models.json> (only the fields we use).
#[derive(Debug, Deserialize)]
pub(crate) struct ModelsDevEntry {
    #[serde(default)]
    pub(crate) name: Option<String>,
    #[serde(default)]
    pub(crate) limit: Option<ModelsDevLimit>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct ModelsDevLimit {
    #[serde(default)]
    pub(crate) context: Option<i64>,
}

/// The full models.dev catalog keyed by slug.
pub(crate) type CatalogMap = HashMap<String, ModelsDevEntry>;

/// Resolves the models.dev catalog. Implemented against the live HTTP endpoint
/// in production and stubbed in tests to keep `get_model_info` hermetic.
pub(crate) trait ModelsDevResolver: std::fmt::Debug + Send + Sync {
    /// Returns the full slug -> entry catalog, or `None` if it could not be
    /// obtained (offline, timeout, parse error). Returning `None` simply skips
    /// enrichment; it is never a hard failure.
    fn fetch_catalog<'a>(
        &'a self,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Option<CatalogMap>> + Send + 'a>>;
}

/// HTTP-backed resolver used in production.
#[derive(Debug, Default, Clone)]
pub(crate) struct HttpModelsDevResolver;

impl ModelsDevResolver for HttpModelsDevResolver {
    fn fetch_catalog<'a>(
        &'a self,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Option<CatalogMap>> + Send + 'a>> {
        Box::pin(async move {
            match fetch_catalog().await {
                Ok(map) => Some(map),
                Err(err) => {
                    info!(error = %err, "models.dev: catalog fetch failed");
                    None
                }
            }
        })
    }
}

/// Test-only resolver that never fetches, so `enrich` is an offline no-op and
/// `get_model_info` stays hermetic in the manager test suite.
#[cfg(test)]
#[derive(Debug, Default)]
pub(crate) struct NoopModelsDevResolver;

#[cfg(test)]
impl ModelsDevResolver for NoopModelsDevResolver {
    fn fetch_catalog<'a>(
        &'a self,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Option<CatalogMap>> + Send + 'a>> {
        Box::pin(async { None })
    }
}

/// Try to replace `fallback` metadata with richer data sourced from
/// models.dev for `fallback.slug`. On any miss or error the original fallback
/// is returned unchanged.
pub(crate) async fn enrich(
    codex_home: &Path,
    mut fallback: ModelInfo,
    resolver: &dyn ModelsDevResolver,
) -> ModelInfo {
    let slug = fallback.slug.clone();
    let cache_path = codex_home.join(CACHE_FILE_NAME);
    let cache = load_cache(&cache_path).await;

    // Positive cache hit: merge remembered metadata and stop.
    if let Some(record) = cache.get(&slug)
        && let Some(metadata) = &record.found
    {
        info!(model = %slug, "models.dev: using cached metadata");
        apply_metadata(&mut fallback, metadata);
        return fallback;
    }

    // Negative cache hit (within TTL): skip the network entirely.
    if let Some(record) = cache.get(&slug)
        && let Some(checked_at) = record.not_found_at
        && chrono::Utc::now().signed_duration_since(checked_at) < NEGATIVE_CACHE_TTL
    {
        return fallback;
    }

    // Cache miss or stale negative entry: query models.dev. The entry must be
    // resolved to owned metadata while the catalog borrow is still live.
    let mut cache = cache;
    let metadata = match resolver.fetch_catalog().await {
        Some(catalog) => find_entry(&slug, &catalog).map(metadata_from_entry),
        None => return fallback,
    };

    match metadata {
        Some(metadata) => {
            cache.insert(
                slug.clone(),
                CacheRecord {
                    found: Some(metadata.clone()),
                    not_found_at: None,
                },
            );
            persist_cache(&cache_path, &cache).await;
            info!(model = %slug, "models.dev: enriched fallback metadata");
            apply_metadata(&mut fallback, &metadata);
            fallback
        }
        None => {
            // Remember the miss so we don't re-query every session.
            cache.insert(
                slug.clone(),
                CacheRecord {
                    found: None,
                    not_found_at: Some(chrono::Utc::now()),
                },
            );
            persist_cache(&cache_path, &cache).await;
            fallback
        }
    }
}

/// Batch enrichment for a freshly fetched provider catalog.
///
/// Used after a BYOK provider's `/v1/models` listing is parsed into minimal
/// [`ModelInfo`] entries (no context window). This fetches the models.dev
/// catalog once and fills in the display name + context window for every entry
/// that is still missing one, persisting each result (positive or negative) to
/// the on-disk cache so later sessions and per-slug [`enrich`] calls skip the
/// network. Entirely fail-safe: on any error the minimal entries are returned
/// unchanged, and entries that already carry a context window are skipped.
pub(crate) async fn enrich_many(
    codex_home: &Path,
    models: &mut [ModelInfo],
    resolver: &dyn ModelsDevResolver,
) {
    if !models.iter().any(|model| model.context_window.is_none()) {
        return;
    }

    let cache_path = codex_home.join(CACHE_FILE_NAME);
    let mut cache = load_cache(&cache_path).await;

    // First pass: satisfy as many entries as possible from the on-disk cache so
    // a routine provider refresh (every few minutes) does not re-hit models.dev
    // once every slug has been resolved once.
    let mut unresolved: Vec<usize> = Vec::new();
    for (index, model) in models.iter_mut().enumerate() {
        if model.context_window.is_some() {
            continue;
        }

        let slug = model.slug.clone();
        if let Some(record) = cache.get(&slug)
            && let Some(metadata) = &record.found
        {
            apply_metadata(model, metadata);
            continue;
        }

        // Stale negative entries are re-queried; fresh ones are skipped.
        if let Some(record) = cache.get(&slug)
            && let Some(checked_at) = record.not_found_at
            && chrono::Utc::now().signed_duration_since(checked_at) < NEGATIVE_CACHE_TTL
        {
            continue;
        }

        unresolved.push(index);
    }

    // Every minimal entry was satisfied from cache: no network needed.
    if unresolved.is_empty() {
        return;
    }

    let Some(catalog) = resolver.fetch_catalog().await else {
        // Offline / timeout / parse error: leave the minimal entries unchanged.
        return;
    };

    let mut changed = false;
    for index in unresolved {
        let model = &mut models[index];
        let slug = model.slug.clone();
        match find_entry(&slug, &catalog).map(metadata_from_entry) {
            Some(metadata) => {
                apply_metadata(model, &metadata);
                cache.insert(
                    slug,
                    CacheRecord {
                        found: Some(metadata),
                        not_found_at: None,
                    },
                );
                changed = true;
            }
            None => {
                cache.insert(
                    slug,
                    CacheRecord {
                        found: None,
                        not_found_at: Some(chrono::Utc::now()),
                    },
                );
                changed = true;
            }
        }
    }

    if changed {
        info!(
            enriched = cache.values().filter(|r| r.found.is_some()).count(),
            "models.dev: enriched provider catalog"
        );
        persist_cache(&cache_path, &cache).await;
    }
}

/// Merge remembered metadata onto the model descriptor in place.
fn apply_metadata(model: &mut ModelInfo, metadata: &CachedModelMetadata) {
    if let Some(display_name) = &metadata.display_name {
        model.display_name = display_name.clone();
    }
    if let Some(context_window) = metadata.context_window {
        model.context_window = Some(context_window);
        // Keep the ceiling aligned with the resolved window so config overrides
        // do not clamp it back down to the old fallback value.
        model.max_context_window = Some(context_window);
        // Let core rederive the 90% compaction threshold from the new window.
        model.auto_compact_token_limit = None;
    }
    // We now have authoritative metadata, so this is no longer a fallback.
    model.used_fallback_model_metadata = false;
}

fn metadata_from_entry(entry: &ModelsDevEntry) -> CachedModelMetadata {
    CachedModelMetadata {
        display_name: entry
            .name
            .clone()
            .map(|n| n.trim().to_string())
            .filter(|n| !n.is_empty()),
        context_window: entry
            .limit
            .as_ref()
            .and_then(|limit| limit.context)
            .filter(|context| *context > 0),
    }
}

/// Fetch the full models.dev catalog as a slug -> entry map.
async fn fetch_catalog() -> Result<CatalogMap, String> {
    let client = reqwest::Client::builder()
        .timeout(FETCH_TIMEOUT)
        .connect_timeout(FETCH_TIMEOUT)
        .build()
        .map_err(|err| err.to_string())?;
    let response = client
        .get(MODELS_DEV_URL)
        .send()
        .await
        .map_err(|err| err.to_string())?
        .error_for_status()
        .map_err(|err| err.to_string())?;
    response
        .json::<CatalogMap>()
        .await
        .map_err(|err| err.to_string())
}

/// Find the catalog entry matching `slug`, tolerant of provider prefixes and
/// minor version suffixes (e.g. `glm-5.2`, `zhipuai/glm-5.2`, `gpt-5.2-codex`).
fn find_entry<'a>(slug: &str, catalog: &'a CatalogMap) -> Option<&'a ModelsDevEntry> {
    // 1. Exact key match (slug already carries a provider prefix).
    if let Some(entry) = catalog.get(slug) {
        return Some(entry);
    }

    let target = name_part(slug).to_ascii_lowercase();

    // 2. Exact match on the model name portion, ignoring provider prefix/case.
    // 3. Otherwise the longest catalog name that is a clean prefix of the
    //    target (e.g. `gpt-5.2` for `gpt-5.2-codex`); the next character must
    //    be a separator so `glm-5` cannot hijack `glm-50`.
    //
    // The candidate name is a temporary `String`, so we only keep its length
    // alongside the catalog-borrowed entry reference.
    let mut prefix_best: Option<(usize, &ModelsDevEntry)> = None;
    for (key, entry) in catalog {
        let candidate = name_part(key).to_ascii_lowercase();
        if candidate == target {
            return Some(entry);
        }
        if candidate.len() < target.len()
            && target.starts_with(&candidate)
            && is_separator(target.as_bytes().get(candidate.len()).copied())
        {
            let is_better = prefix_best
                .as_ref()
                .is_none_or(|(best_len, _)| candidate.len() > *best_len);
            if is_better {
                prefix_best = Some((candidate.len(), entry));
            }
        }
    }
    prefix_best.map(|(_, entry)| entry)
}

/// Strip a single leading `provider/` segment, if present.
fn name_part(slug: &str) -> &str {
    match slug.split_once('/') {
        Some((_, right)) if !right.is_empty() => right,
        _ => slug,
    }
}

fn is_separator(byte: Option<u8>) -> bool {
    matches!(byte, Some(b'-' | b'.' | b'_' | b'/'))
}

async fn load_cache(path: &Path) -> CacheMap {
    let bytes = match tokio::fs::read(path).await {
        Ok(bytes) => bytes,
        Err(_) => return CacheMap::new(),
    };
    match serde_json::from_slice::<CacheMap>(&bytes) {
        Ok(map) => map,
        Err(err) => {
            warn!(?path, error = %err, "models.dev cache: corrupt, ignoring");
            CacheMap::new()
        }
    }
}

async fn persist_cache(path: &Path, cache: &CacheMap) {
    let Ok(bytes) = serde_json::to_vec_pretty(cache) else {
        return;
    };
    if let Err(err) = tokio::fs::write(path, &bytes).await {
        warn!(?path, error = %err, "models.dev cache: failed to persist");
    }
}

#[cfg(test)]
#[path = "models_dev_tests.rs"]
mod tests;
