//! Terminal-free application core: value access, cell rendering, detail composition, YAML,
//! store, scheduler, sampler, search, session, stats, status, actions, guardrails, tasks,
//! journal, names, can-i, the on-disk cache, the palette's history, the refresh schedule, the
//! logging switch, and the contexts seam.
//! Nothing here touches the terminal.

pub mod actions;
pub mod cache;
pub mod can_i;
pub mod cell;
pub mod contexts;
pub mod detail;
pub mod evidence;
pub mod export;
pub mod guardrails;
pub mod history;
pub mod journal;
pub mod log;
pub mod names;
pub mod refresh;
pub mod sampler;
pub mod scheduler;
pub mod search;
pub mod session;
pub mod source;
pub mod stats;
pub mod status;
pub mod store;
pub mod tasks;
pub mod yaml;

// `path` lives in the catalog: `prism` needs it for `Kind::name_path` and cannot depend on
// `core`. Re-exported rather than wrapped, so `nutsh_core::path::get` is the same function.
pub use nutsh_catalog::path;
