//! pageon's JavaScript: V8 (single-threaded), DOM bindings generated from WebIDL, and the HTML
//! event loop on pageon's virtual clock.
//!
//! The loader in the `pageon` crate drives an [`Engine`]: one per page, a realm per document,
//! scripts run at their parse position, then the page settles on virtual time. Every member a
//! page can see is listed in `idl/allowlist.toml` or generated from the IDL (`[Reflect]`
//! attributes, event handlers, constants); anything else is absent, so feature detection works.

mod cv;
mod dom;
mod engine;
mod eval;
pub mod frames;
mod modules;
#[rustfmt::skip]
mod generated;
pub mod realm;
mod select;
mod serialize;

pub use dom::scroll::{Amount, Scroller, Wheeled};
pub use dom::session::{HistoryOp, State as SavedState};
pub use dom::webmcp::ToolInfo;
pub use engine::{Engine, Limits, RealmConfig, ScrollOffsets, Settled};
pub use eval::{Arg, Handle, PromiseState, Thrown};
pub use modules::{ScriptKind, script_kind};
pub use realm::{Dialog, NavRequest, Target};
pub use serialize::{Depth, LocalKey, LocalValue};

/// What went wrong in a page's scripts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum PageErrorKind {
    /// An uncaught exception.
    Exception,
    /// A task ran longer than the time limit and was terminated.
    TimeLimit,
    /// Scripts hit the heap limit; scripting stopped for the page.
    MemoryLimit,
    /// A script pageon does not run (module scripts, navigation from script).
    Unsupported,
    /// An external script failed to load.
    Load,
}

impl std::fmt::Display for PageErrorKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Exception => "exception",
            Self::TimeLimit => "time-limit",
            Self::MemoryLimit => "memory-limit",
            Self::Unsupported => "unsupported",
            Self::Load => "load",
        })
    }
}

/// A page error: what happened and where.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PageError {
    /// Kind.
    pub kind: PageErrorKind,
    /// Message.
    pub message: String,
    /// Script URL or name.
    pub source: String,
    /// Line (1-based; 0 if unknown).
    pub line: u32,
    /// Column (1-based; 0 if unknown).
    pub column: u32,
}

impl std::fmt::Display for PageError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} {}", self.kind, self.source)?;
        if self.line > 0 {
            write!(f, ":{}:{}", self.line, self.column)?;
        }
        // Page-authored text: quoted (one line, newlines escaped) and capped, so it can never
        // read as view lines.
        let msg: String = self.message.chars().take(500).collect();
        let cut = if self.message.chars().count() > 500 {
            "…"
        } else {
            ""
        };
        write!(
            f,
            " {}",
            serde_json::to_string(&format!("{msg}{cut}")).unwrap_or_default()
        )
    }
}
