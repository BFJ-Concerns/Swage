//! Storage traits and implementations — re-exported from `swage-storage`.
//!
//! [`store::Storage`] is the supertrait combining all sub-traits.
//! [`SqliteStorage`] is the bundled SQLite backend. Model types
//! (`Session`, `Thread`, `Turn`, `Message`, etc.) live in [`model`].

pub use swage_storage::*;
