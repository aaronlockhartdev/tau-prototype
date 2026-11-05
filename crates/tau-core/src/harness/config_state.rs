//! A config layer's state as one value: what runs, and the load failure it runs
//! beside. These were two maps keyed by the same string (`configs` +
//! `config_stale`), their pairing — "the mark describes the merged entry beside
//! it" — kept only in prose.

use crate::config::Config;

/// A config layer the code could not read: the file to name to the user, and
/// what the loader said. The session keeps running on its last good config
/// beside the rejection (#359).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ConfigRejection {
    pub(crate) path: String,
    pub(crate) reason: String,
}

/// One layer's config state. The mark is a field of the config it describes, so
/// a write that replaces the config replaces the mark in the same step; the
/// pairing no longer asks two maps to agree (#359).
#[derive(Debug, Clone)]
// A config is large and a rejection is two strings, and that asymmetry is the
// domain: `Current` owning the config that runs is the point of the change, and
// every read is on the session path. Boxing would buy bytes per read to buy an
// allocation per entry.
#[allow(clippy::large_enum_variant)]
pub(crate) enum ConfigState {
    /// The config that runs. `rejection` is the layer that stopped reading
    /// while this config kept running (the watcher's failure path), or a cause
    /// a cold read folded in from the system layer.
    Current {
        config: Config,
        rejection: Option<ConfigRejection>,
    },
    /// Nothing reads: the failure is all there is to hand a cold read (#359's
    /// "a failed read writes nothing" — a mark with no merge beside it).
    Marked { rejection: ConfigRejection },
}

impl ConfigState {
    /// The config a session runs on, if one reads. A mark alone has none: the
    /// cold read meets the file.
    pub(crate) fn config(&self) -> Option<&Config> {
        match self {
            Self::Current { config, .. } => Some(config),
            Self::Marked { .. } => None,
        }
    }

    /// The cause a turn boundary is told, if the layer stopped reading.
    pub(crate) fn rejection(&self) -> Option<&ConfigRejection> {
        match self {
            Self::Current { rejection, .. } => rejection.as_ref(),
            Self::Marked { rejection } => Some(rejection),
        }
    }

    /// The layer stopped reading. The running config stays — the session keeps
    /// running on the last one that read — and the cause rides beside it.
    pub(crate) fn mark_stale(&mut self, rejection: ConfigRejection) {
        match self {
            Self::Current {
                rejection: slot, ..
            } => *slot = Some(rejection),
            Self::Marked { rejection: slot } => *slot = rejection,
        }
    }

    /// A read that touched the layer this state describes answers its mark: the
    /// repair self-heals at the next read, even where no watcher is parked to
    /// reload (#419). A mark with no config is the whole state, and it goes.
    pub(crate) fn answer_mark(&mut self) {
        match self {
            Self::Current { rejection, .. } => *rejection = None,
            Self::Marked { .. } => {
                *self = Self::Current {
                    config: Config::default(),
                    rejection: None,
                }
            }
        }
    }

    /// The layer read: this config runs, and the mark that described the entry
    /// it replaces rides the swap — a write that dropped the mark silently
    /// would un-tell a failure the disk has not repaired (#359).
    pub(crate) fn set_current(&mut self, config: Config) {
        let rejection = self.rejection().cloned();
        *self = Self::Current { config, rejection };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rejection() -> ConfigRejection {
        ConfigRejection {
            path: "/p/.tau/config.toml".into(),
            reason: "parse error".into(),
        }
    }

    #[test]
    fn a_mark_rides_the_config_it_describes() {
        // Positive: the running config survives its layer's failure, and the
        // cause is the boundary's to tell.
        let mut state = ConfigState::Current {
            config: Config::default(),
            rejection: None,
        };
        state.mark_stale(rejection());
        assert!(state.config().is_some(), "the session keeps running");
        assert_eq!(state.rejection(), Some(&rejection()));
        // A read that repairs answers the mark and keeps the config.
        state.answer_mark();
        assert_eq!(state.rejection(), None);
        assert!(state.config().is_some());
    }

    #[test]
    fn a_mark_with_no_merge_has_nothing_to_run_on() {
        // Negative: a mark alone is not a config; the cold read meets the file.
        let mut state = ConfigState::Marked {
            rejection: rejection(),
        };
        assert!(state.config().is_none(), "no config, only the cause");
        assert_eq!(state.rejection(), Some(&rejection()));
        state.answer_mark();
        assert_eq!(state.rejection(), None, "the mark is answered");
    }
}
