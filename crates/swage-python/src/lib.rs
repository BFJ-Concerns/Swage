//! Child-process Python runtime support.
//!
//! Provides worker-host infrastructure for `python_execute`, programmable
//! assembly, and topology definition execution.

mod blocking;

pub mod runner;
pub mod runtime_resolver;
pub mod topology;
pub mod worker_host;
pub mod worker_pool;
pub mod worker_protocol;

pub use runner::{ConcreteAssemblyRunner, ConfiguredAssemblyRunner};
pub use worker_host::{NestedToolHost, PythonExecutionResult, PythonWorkerError, PythonWorkerRuntime};
pub use worker_pool::{WorkerLease, WorkerPool};
