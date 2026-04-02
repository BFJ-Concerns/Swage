//! Embedded Python runtime for Programmatic Tool Calling (PTC).
//!
//! Provides a bundled Python environment (via PyO3) that executes agent-generated
//! Python code with access to Swage's built-in tools as callable functions in a
//! `swage` namespace module. The gateway registers `python_execute` as a tool;
//! this crate handles the runtime, tool bindings, and execution machinery.

pub mod runtime;
pub mod dispatch;
pub mod execute;
pub mod bindings;
pub mod assembly_bindings;
pub mod assembly;
pub mod runner;
pub mod topology;
pub mod topology_bindings;

pub use runner::ConcreteAssemblyRunner;
