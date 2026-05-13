//! Tiny styling layer for terminal output.
//!
//! Detects whether stdout is a TTY and honours `NO_COLOR`; emits ANSI escape
//! sequences only when both checks pass. There's no dependency on a
//! third-party crate — this is small enough that hand-rolling keeps the
//! palette explicit and the binary lean.
//!
//! The palette is intentionally narrow: accent (cyan) for the *one* thing
//! you should look at, warn (amber) for things that need action, two tiers
//! of grey for decoration (`dim`) versus secondary data (`secondary`), a
//! softened grey for the merged-section, and OK (sage) for successful
//! states. Bright red is reserved for hard errors and used sparingly.
//!
//! The grey tiers are picked to stay legible against dark terminal
//! backgrounds (default macOS Pro, Solarized Dark, GitHub dark) without
//! shouting on light themes — values in the 244–250 range hit that
//! window. Anything ≤ 240 starts disappearing against typical dark
//! backgrounds.

use std::io::IsTerminal;

fn color_disabled_by_env() -> bool {
    std::env::var_os("NO_COLOR").is_some() || std::env::var_os("GIT_STACK_NO_COLOR").is_some()
}

/// True when stdout is an interactive terminal and the user has not opted out.
pub fn use_color() -> bool {
    !color_disabled_by_env() && std::io::stdout().is_terminal()
}

/// True when stderr is an interactive terminal and the user has not opted out.
/// Used by helpers that wrap text in `eprintln!`-bound output (e.g. `submit`
/// printing PR URLs to the status stream).
pub fn use_color_stderr() -> bool {
    !color_disabled_by_env() && std::io::stderr().is_terminal()
}

/// Wrap `text` in the given ANSI SGR sequence, or return it unchanged when
/// colour is disabled for the chosen stream.
pub fn sgr(text: &str, params: &str) -> String {
    sgr_on(text, params, use_color())
}

pub fn sgr_stderr(text: &str, params: &str) -> String {
    sgr_on(text, params, use_color_stderr())
}

fn sgr_on(text: &str, params: &str, on: bool) -> String {
    if !on {
        return text.to_string();
    }
    format!("\x1b[{params}m{text}\x1b[0m")
}

pub fn accent(s: &str) -> String {
    sgr(s, "1;38;5;81")
} // bold cyan
pub fn ok(s: &str) -> String {
    sgr(s, "38;5;108")
} // sage
pub fn warn(s: &str) -> String {
    sgr(s, "38;5;215")
} // amber
pub fn merged(s: &str) -> String {
    sgr(s, "38;5;246")
} // soft grey; merged-section rows
pub fn dim(s: &str) -> String {
    sgr(s, "38;5;244")
} // chrome grey: separators, labels, decoration
pub fn secondary(s: &str) -> String {
    sgr(s, "38;5;250")
} // legible grey for secondary data values (paths, refs, counts)
pub fn url(s: &str) -> String {
    sgr(s, "4;38;5;250")
} // underlined secondary
pub fn bold(s: &str) -> String {
    sgr(s, "1")
}

/// Stderr-aware URL style. Use this when printing URLs via `eprintln!`.
pub fn url_err(s: &str) -> String {
    sgr_stderr(s, "4;38;5;250")
}

/// Glyphs used to draw the stack as a graph. Keep them ASCII-friendly where
/// possible — the chain pipes are the one place we lean on Unicode because
/// the visual is the whole point.
pub mod glyph {
    pub const CHAIN: &str = "│";
    #[allow(dead_code)]
    pub const TEE: &str = "├─";
    pub const ELBOW: &str = "└─";
    pub const CURRENT: &str = "●";
    pub const ACTIVE: &str = "○";
    pub const MERGED: &str = "✓";
    pub const ARROW: &str = "→";
    pub const WARN: &str = "▲";
}
