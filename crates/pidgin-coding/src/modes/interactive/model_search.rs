//! Mirrors pi-coding-agent's interactive `model-search`
//! (`packages/coding-agent/src/modes/interactive/model-search.ts`).
//!
//! Pure string-builders that flatten a model's identity into the searchable
//! text used by the fuzzy model pickers. `get_model_search_text` is the general
//! form; `get_model_selector_search_text` deliberately keeps the bare model id
//! out of the leading position so exact provider-prefixed queries rank ahead of
//! proxy-provider ids.

// straitjacket-allow-file:duplication

/// The minimal model shape the search-text builders consume.
///
/// Mirrors pi's local `ModelSearchItem` interface (`{ id, provider, name? }`)
/// rather than the full model-catalog `Model`, so callers can pass any
/// model-like value without carrying the entire catalog record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelSearchItem {
    /// Stable model identifier (unique within its provider).
    pub id: String,
    /// Owning provider id (e.g. `anthropic`, `openai`).
    pub provider: String,
    /// Optional human-readable display name.
    pub name: Option<String>,
}

/// Builds the searchable text for a model, leading with the bare model id.
///
/// Mirrors pi's `getModelSearchText`.
pub fn get_model_search_text(item: &ModelSearchItem) -> String {
    let ModelSearchItem { id, provider, .. } = item;
    let name = name_suffix(item);
    format!("{id} {provider} {provider}/{id} {provider} {id}{name}")
}

/// Builds the searchable text for the `/model` selector.
///
/// The `/model` selector search should rank exact provider-prefixed queries
/// before proxy-provider ids like `openrouter/openai/gpt-5`, so keep the bare
/// model id out of the leading position.
///
/// Mirrors pi's `getModelSelectorSearchText`.
pub fn get_model_selector_search_text(item: &ModelSearchItem) -> String {
    let ModelSearchItem { id, provider, .. } = item;
    let name = name_suffix(item);
    format!("{provider} {provider}/{id} {provider} {id}{name}")
}

/// Mirrors pi's `item.name ? \` ${item.name}\` : ""`, where an absent or empty
/// name (JavaScript falsy) contributes no suffix.
fn name_suffix(item: &ModelSearchItem) -> String {
    item.name
        .as_deref()
        .filter(|name| !name.is_empty())
        .map(|name| format!(" {name}"))
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(id: &str, provider: &str, name: Option<&str>) -> ModelSearchItem {
        ModelSearchItem {
            id: id.to_string(),
            provider: provider.to_string(),
            name: name.map(str::to_string),
        }
    }

    #[test]
    fn search_text_leads_with_bare_id() {
        let text = get_model_search_text(&item("gpt-5", "openai", None));
        assert_eq!(text, "gpt-5 openai openai/gpt-5 openai gpt-5");
    }

    #[test]
    fn search_text_appends_name_when_present() {
        let text = get_model_search_text(&item("gpt-5", "openai", Some("GPT-5")));
        assert_eq!(text, "gpt-5 openai openai/gpt-5 openai gpt-5 GPT-5");
    }

    #[test]
    fn selector_text_keeps_bare_id_out_of_leading_position() {
        let text = get_model_selector_search_text(&item("gpt-5", "openai", None));
        assert_eq!(text, "openai openai/gpt-5 openai gpt-5");
        // The bare id never leads, so a provider-prefixed query wins over
        // proxy-provider ids like `openrouter/openai/gpt-5`.
        assert!(!text.starts_with("gpt-5"));
    }

    #[test]
    fn selector_text_appends_name_when_present() {
        let text = get_model_selector_search_text(&item("gpt-5", "openai", Some("GPT-5")));
        assert_eq!(text, "openai openai/gpt-5 openai gpt-5 GPT-5");
    }

    #[test]
    fn provider_prefixed_query_ranks_ahead_of_proxy_provider() {
        // A proxy-provider entry whose bare id embeds another provider's prefix.
        let proxy = get_model_selector_search_text(&item("openai/gpt-5", "openrouter", None));
        // The direct entry for the same underlying model.
        let direct = get_model_selector_search_text(&item("gpt-5", "openai", None));

        // The direct entry leads with its real provider, so a query of
        // `openai/gpt-5` matches at the front of `direct` but only later in the
        // proxy entry.
        assert!(direct.starts_with("openai openai/gpt-5"));
        assert!(!proxy.starts_with("openai/gpt-5"));
        assert!(proxy.starts_with("openrouter"));
    }

    #[test]
    fn empty_name_contributes_no_suffix() {
        // JavaScript treats an empty string as falsy, so no trailing space/name.
        let text = get_model_search_text(&item("gpt-5", "openai", Some("")));
        assert_eq!(text, "gpt-5 openai openai/gpt-5 openai gpt-5");
    }
}
