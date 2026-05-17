// ───────────────────────────────────────────────────────────────────────────
// backend — the one match point that routes sanctel between PTY backends.
//
// The spike (issue #16) introduces zellij as a parallel backend behind a
// `SANCTEL_BACKEND` env var. The default is `tmux` (existing behavior,
// unchanged); setting `SANCTEL_BACKEND=zellij` opts into the spike path.
//
// This file is the *only* place the rest of the codebase reads the env var.
// Everywhere else asks `Backend::from_env()` and matches on the result, so
// backend-specific imports stay encapsulated in their owning modules
// (`tmux_cli`, `zellij_cli`, `zellij_daemon`).
// ───────────────────────────────────────────────────────────────────────────

/// The PTY backend sanctel is configured to use for this process. Set at
/// startup (one read of `SANCTEL_BACKEND`) and treated as immutable for the
/// rest of the process lifetime; live-switching backends is not a goal.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Backend {
    /// The shipped default — tmux on a dedicated `-L sanctel` socket.
    Tmux,
    /// The spike backend — `zellij web` daemon over WebSocket. Opt-in only.
    Zellij,
}

/// The env var name. Exposed for tests so they don't drift from production.
pub const ENV_VAR: &str = "SANCTEL_BACKEND";

impl Backend {
    /// Resolve the backend from the process environment. Defaults to `Tmux`.
    pub fn from_env() -> Backend {
        Backend::from_env_value(std::env::var(ENV_VAR).ok().as_deref())
    }

    /// Pure resolver — separated so unit tests can drive it without touching
    /// process env. Recognized values are `"zellij"` (case-sensitive, matches
    /// the issue spec) and `"tmux"`; anything else (None, empty string,
    /// typo, unset) maps to the safe default `Tmux`.
    ///
    /// The "unknown → tmux" branch is deliberate: a misspelled env var must
    /// NOT silently switch to the spike backend, nor must it surface a
    /// startup error that blocks the user from launching sanctel at all.
    /// Falling back to the byte-identical tmux behavior is the conservative
    /// choice while the spike is opt-in.
    pub fn from_env_value(value: Option<&str>) -> Backend {
        match value.map(str::trim) {
            Some("zellij") => Backend::Zellij,
            _ => Backend::Tmux,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Unset env var → Tmux. The default-stays-tmux invariant is the
    /// load-bearing promise of the spike (acceptance criterion #1): an
    /// existing sanctel install must not change behavior on upgrade.
    #[test]
    fn unset_defaults_to_tmux() {
        assert_eq!(Backend::from_env_value(None), Backend::Tmux);
    }

    /// `SANCTEL_BACKEND=zellij` → Zellij. The only opt-in path.
    #[test]
    fn zellij_value_selects_zellij_backend() {
        assert_eq!(Backend::from_env_value(Some("zellij")), Backend::Zellij);
    }

    /// `SANCTEL_BACKEND=tmux` → Tmux. Explicit-tmux is the same as unset,
    /// but the user can spell it out without breaking anything.
    #[test]
    fn explicit_tmux_value_selects_tmux_backend() {
        assert_eq!(Backend::from_env_value(Some("tmux")), Backend::Tmux);
    }

    /// Typos / unrecognized values → Tmux (the safe default). A misspelled
    /// env var must not silently activate the spike backend.
    #[test]
    fn unknown_value_falls_back_to_tmux() {
        assert_eq!(Backend::from_env_value(Some("zelli")), Backend::Tmux);
        assert_eq!(Backend::from_env_value(Some("TMUX")), Backend::Tmux);
        assert_eq!(Backend::from_env_value(Some("zellij ")), Backend::Zellij);
        assert_eq!(Backend::from_env_value(Some("")), Backend::Tmux);
    }

    /// Leading/trailing whitespace is stripped. Shell tooling occasionally
    /// pads env values when they're built up by string concatenation.
    #[test]
    fn whitespace_around_value_is_trimmed() {
        assert_eq!(Backend::from_env_value(Some("  zellij  ")), Backend::Zellij);
        assert_eq!(Backend::from_env_value(Some("\tzellij\n")), Backend::Zellij);
    }
}
