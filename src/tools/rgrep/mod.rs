//! rgrep module - enhanced grep search with gitignore, type filtering, and context.
//! Ported from upstream tools/rgrep/ (1600+ lines across 6 files).

pub mod binary;
pub mod config;
pub mod gitignore;
pub mod searcher;
pub mod sink;
pub mod types;
pub mod walker;

pub use config::{OutputMode, SearchConfig, SearchResult, SearchResultEntry};
pub use searcher::search;
pub use sink::format_result;
pub use types::{extensions_for_type, types_for_extension, file_matches_type};
