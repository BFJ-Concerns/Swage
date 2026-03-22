//! Bubblewrap-based sandbox implementation for Linux.
//!
//! Namespace isolation via `bwrap`, with configurable mounts and
//! die-with-parent for automatic child cleanup.

mod command;
mod sandbox;

pub use command::find_bwrap;
pub use sandbox::BubblewrapSandbox;
