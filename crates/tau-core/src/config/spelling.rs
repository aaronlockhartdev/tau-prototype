//! The rules that make `provider/key` a name for exactly one endpoint (#354).
//!
//! §6.7's routing, the GUI's model menu, a session record's model field,
//! `[thinking].levels` keys and an agent type's frontmatter all carry the id as
//! one string, so the string has to answer for one endpoint and no more. Each
//! rule below removes a shape where it answers for more than one, or for none.
//! A model key may itself carry a `/` — `openai/gpt-5`, the spelling of the
//! gateways that host several vendors, legal under a provider whose own name is
//! that first segment: §6.7 consumes the prefix once, so the declared key is
//! addressed by the full id `<provider>/openai/gpt-5` (#354).
//!
//! A key of nothing is the "for none" case in its sharpest form: `models` is a
//! map, so `[providers.dev.models.""]` is a legal entry, and a provider's first
//! key is the model a session opens on when the config names no `default_model`.
use super::{Config, LoadError};

/// Refuse a config whose id spelling is ambiguous at load, naming the entry the
/// user's edit has to change.
pub(crate) fn validate(config: &Config) -> Result<(), LoadError> {
    for (name, provider) in &config.providers {
        if name.contains('/') {
            return Err(LoadError::ProviderNameHasSlash {
                provider: name.clone(),
            });
        }
        // The empty key is the id a session opens on (a provider's first key is
        // the default model), and a session on it appends `model_changed` to
        // `""` at every reopen forever: the reader refuses to name no model, so
        // the fold never answers the model question, so each reopen sees a
        // change it has to record, and the log is append-only (Add6-A1).
        if provider.models.contains_key("") {
            return Err(LoadError::ModelKeyEmpty {
                provider: name.clone(),
            });
        }
    }
    for key in config.thinking.levels.keys() {
        // derive_turn looks a level up by the key a model id routes to, with a
        // configured provider's prefix stripped; a prefixed key no provider
        // declares as a model is read by nothing.
        if key.contains('/')
            && !config
                .providers
                .values()
                .any(|provider| provider.models.contains_key(key))
        {
            return Err(LoadError::LevelUndeclared { key: key.clone() });
        }
    }
    Ok(())
}
