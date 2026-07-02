use super::*;
use crate::model_info::model_info_from_slug;
use codex_protocol::openai_models::ModelInfo;
use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

/// Offline resolver that always serves a fixed catalog, for batch enrichment.
#[derive(Debug)]
struct StaticCatalogResolver {
    entries: Vec<(String, Option<String>, Option<i64>)>,
}

impl ModelsDevResolver for StaticCatalogResolver {
    fn fetch_catalog<'a>(
        &'a self,
    ) -> Pin<Box<dyn Future<Output = Option<CatalogMap>> + Send + 'a>> {
        let entries = self.entries.clone();
        Box::pin(async move {
            let mut catalog = CatalogMap::new();
            for (key, name, context) in entries {
                catalog.insert(
                    key,
                    ModelsDevEntry {
                        name,
                        limit: context.map(|c| ModelsDevLimit { context: Some(c) }),
                    },
                );
            }
            Some(catalog)
        })
    }
}

#[derive(Debug)]
struct OfflineResolver;

impl ModelsDevResolver for OfflineResolver {
    fn fetch_catalog<'a>(
        &'a self,
    ) -> Pin<Box<dyn Future<Output = Option<CatalogMap>> + Send + 'a>> {
        Box::pin(async { None })
    }
}

fn entry(name: &str, context: Option<i64>) -> ModelsDevEntry {
    ModelsDevEntry {
        name: Some(name.to_string()),
        limit: context.map(|c| ModelsDevLimit { context: Some(c) }),
    }
}

/// Borrow the catalog entry's context window (the entries are behind shared refs).
fn context_of(entry: Option<&ModelsDevEntry>) -> Option<i64> {
    entry.and_then(|e| e.limit.as_ref()).and_then(|l| l.context)
}

#[test]
fn name_part_strips_single_provider_prefix() {
    assert_eq!(name_part("zhipuai/glm-5.2"), "glm-5.2");
    assert_eq!(name_part("openai/gpt-5.2-codex"), "gpt-5.2-codex");
    assert_eq!(name_part("glm-5.2"), "glm-5.2");
    assert_eq!(name_part("/weird"), "weird");
    // A trailing slash with nothing after keeps the whole slug.
    assert_eq!(name_part("trailing/"), "trailing/");
}

#[test]
fn find_entry_matches_exact_key_then_name_then_prefix() {
    let mut catalog = HashMap::new();
    catalog.insert(
        "zhipuai/glm-5.2".to_string(),
        entry("GLM-5.2", Some(1_000_000)),
    );
    catalog.insert(
        "openai/gpt-5.2".to_string(),
        entry("GPT-5.2", Some(400_000)),
    );
    catalog.insert("openai/gpt-5".to_string(), entry("GPT-5", Some(400_000)));

    // Exact key.
    assert_eq!(
        context_of(find_entry("zhipuai/glm-5.2", &catalog)),
        Some(1_000_000)
    );
    // Exact name part, no provider prefix on the slug.
    assert_eq!(context_of(find_entry("glm-5.2", &catalog)), Some(1_000_000));
    // Case-insensitive name match.
    assert_eq!(context_of(find_entry("GLM-5.2", &catalog)), Some(1_000_000));
    // Prefix fallback: gpt-5.2 wins over gpt-5 for the codex variant.
    assert_eq!(
        find_entry("gpt-5.2-codex", &catalog).and_then(|e| e.name.clone()),
        Some("GPT-5.2".to_string())
    );
}

#[test]
fn find_entry_prefix_requires_separator_boundary() {
    let mut catalog = HashMap::new();
    // `glm-5` must NOT match `glm-50` (next char is a digit, not a separator).
    catalog.insert("zhipuai/glm-5".to_string(), entry("GLM-5", Some(128_000)));
    assert!(find_entry("glm-50", &catalog).is_none());
}

#[test]
fn find_entry_returns_none_for_truly_unknown_slug() {
    let catalog = HashMap::<String, ModelsDevEntry>::new();
    assert!(find_entry("totally-made-up-model", &catalog).is_none());
}

#[test]
fn metadata_from_entry_skips_empty_names_and_nonpositive_context() {
    let meta = metadata_from_entry(&ModelsDevEntry {
        name: Some("   ".to_string()),
        limit: Some(ModelsDevLimit { context: Some(0) }),
    });
    assert_eq!(meta.display_name, None);
    assert_eq!(meta.context_window, None);

    let meta = metadata_from_entry(&entry("GLM-5.2", Some(1_000_000)));
    assert_eq!(meta.display_name.as_deref(), Some("GLM-5.2"));
    assert_eq!(meta.context_window, Some(1_000_000));
}

#[test]
fn apply_metadata_overrides_context_window_and_clears_fallback_flag() {
    let mut model = model_info_from_slug("glm-5.2");
    assert!(model.used_fallback_model_metadata);
    // The fallback hardcodes a 272k window.
    assert_eq!(model.context_window, Some(272_000));

    let metadata = CachedModelMetadata {
        display_name: Some("GLM-5.2".to_string()),
        context_window: Some(1_000_000),
    };
    apply_metadata(&mut model, &metadata);

    assert!(!model.used_fallback_model_metadata);
    assert_eq!(model.context_window, Some(1_000_000));
    assert_eq!(model.max_context_window, Some(1_000_000));
    assert_eq!(model.auto_compact_token_limit, None);
    assert_eq!(model.display_name, "GLM-5.2");
}

#[tokio::test]
async fn cache_round_trips_through_disk() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(CACHE_FILE_NAME);

    let mut cache: CacheMap = CacheMap::new();
    cache.insert(
        "glm-5.2".to_string(),
        CacheRecord {
            found: Some(CachedModelMetadata {
                display_name: Some("GLM-5.2".to_string()),
                context_window: Some(1_000_000),
            }),
            not_found_at: None,
        },
    );
    persist_cache(&path, &cache).await;
    let loaded = load_cache(&path).await;

    let record = loaded.get("glm-5.2").unwrap();
    let metadata = record.found.as_ref().unwrap();
    assert_eq!(metadata.context_window, Some(1_000_000));
}

#[tokio::test]
async fn load_cache_returns_empty_for_missing_or_corrupt_file() {
    let dir = tempfile::tempdir().unwrap();

    // Missing file.
    assert!(load_cache(&dir.path().join("nope.json")).await.is_empty());

    // Corrupt file.
    let corrupt = dir.path().join("bad.json");
    tokio::fs::write(&corrupt, b"{ not json").await.unwrap();
    assert!(load_cache(&corrupt).await.is_empty());
}

#[tokio::test]
async fn enrich_many_fills_context_window_for_byok_entries() {
    let dir = tempfile::tempdir().unwrap();
    let resolver = StaticCatalogResolver {
        entries: vec![(
            "zhipuai/glm-5.2".to_string(),
            Some("GLM-5.2".to_string()),
            Some(1_000_000),
        )],
    };
    let mut models = vec![
        ModelInfo::minimal_remote("glm-5.2"),
        ModelInfo::minimal_remote("anthropic/claude-sonnet-4.5"),
    ];
    // Minimal BYOK entries start without a context window.
    assert!(models.iter().all(|model| model.context_window.is_none()));

    enrich_many(dir.path(), &mut models, &resolver).await;

    // The known slug is enriched from the catalog.
    let glm = models
        .iter()
        .find(|model| model.slug == "glm-5.2")
        .expect("glm-5.2 entry present");
    assert_eq!(glm.context_window, Some(1_000_000));
    assert_eq!(glm.max_context_window, Some(1_000_000));
    assert_eq!(glm.display_name, "GLM-5.2");

    // The unknown slug is left unchanged (fail-safe).
    let claude = models
        .iter()
        .find(|model| model.slug == "anthropic/claude-sonnet-4.5")
        .expect("claude entry present");
    assert!(claude.context_window.is_none());
}

#[tokio::test]
async fn enrich_many_is_noop_when_offline_and_when_already_enriched() {
    let dir = tempfile::tempdir().unwrap();

    // Offline resolver: minimal entries must be returned unchanged.
    let mut offline_models = vec![ModelInfo::minimal_remote("glm-5.2")];
    enrich_many(dir.path(), &mut offline_models, &OfflineResolver).await;
    assert!(offline_models[0].context_window.is_none());

    // Entries that already carry a context window are not re-queried.
    let mut enriched = vec![ModelInfo::minimal_remote("glm-5.2")];
    enriched[0].context_window = Some(200_000);
    let resolver = StaticCatalogResolver {
        entries: vec![(
            "zhipuai/glm-5.2".to_string(),
            Some("GLM-5.2".to_string()),
            Some(1_000_000),
        )],
    };
    enrich_many(dir.path(), &mut enriched, &resolver).await;
    assert_eq!(enriched[0].context_window, Some(200_000));
    assert_eq!(enriched[0].display_name, "glm-5.2");
}

#[tokio::test]
async fn enrich_many_skips_network_once_cache_satisfies_all() {
    use std::sync::atomic::{AtomicU32, Ordering};

    #[derive(Debug)]
    struct CountingResolver {
        count: Arc<AtomicU32>,
    }
    impl ModelsDevResolver for CountingResolver {
        fn fetch_catalog<'a>(
            &'a self,
        ) -> Pin<Box<dyn Future<Output = Option<CatalogMap>> + Send + 'a>> {
            let count = self.count.clone();
            Box::pin(async move {
                count.fetch_add(1, Ordering::SeqCst);
                let mut catalog = CatalogMap::new();
                catalog.insert(
                    "zhipuai/glm-5.2".to_string(),
                    entry("GLM-5.2", Some(1_000_000)),
                );
                Some(catalog)
            })
        }
    }

    let dir = tempfile::tempdir().unwrap();
    let count = Arc::new(AtomicU32::new(0));
    let resolver = CountingResolver {
        count: count.clone(),
    };

    // First pass: cache miss -> fetch, enrich, persist.
    let mut first = vec![ModelInfo::minimal_remote("glm-5.2")];
    enrich_many(dir.path(), &mut first, &resolver).await;
    assert_eq!(first[0].context_window, Some(1_000_000));
    assert_eq!(count.load(Ordering::SeqCst), 1);

    // Second pass: fresh minimal entry, but the slug is now cached -> no fetch.
    let mut second = vec![ModelInfo::minimal_remote("glm-5.2")];
    enrich_many(dir.path(), &mut second, &resolver).await;
    assert_eq!(second[0].context_window, Some(1_000_000));
    assert_eq!(
        count.load(Ordering::SeqCst),
        1,
        "routine refresh should be satisfied from cache without re-fetching models.dev"
    );
}
