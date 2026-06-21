use super::*;
use crate::model_info::model_info_from_slug;
use std::collections::HashMap;

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
    catalog.insert("zhipuai/glm-5.2".to_string(), entry("GLM-5.2", Some(1_000_000)));
    catalog.insert("openai/gpt-5.2".to_string(), entry("GPT-5.2", Some(400_000)));
    catalog.insert("openai/gpt-5".to_string(), entry("GPT-5", Some(400_000)));

    // Exact key.
    assert_eq!(context_of(find_entry("zhipuai/glm-5.2", &catalog)), Some(1_000_000));
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
    model = apply_metadata(model, &metadata);

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
