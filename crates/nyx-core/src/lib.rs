//! What every Nyx app shares underneath its window: who it is, where its
//! files live, its preferences file, and running other programs.

pub mod app;
pub mod cmd;
#[cfg(feature = "gtk")]
pub mod control;
pub mod migrate;
pub mod paths;
#[cfg(feature = "gtk")]
pub mod prefs;
