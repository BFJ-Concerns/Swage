//! Tool abstraction for agent capabilities.
//!
//! Defines the `Tool` trait for extensible agent actions. Tools encapsulate
//! discrete capabilities (file operations, API calls, computations) that agents
//! can invoke during task execution.
//!
//! # Tool Responsibilities
//!
//! - **Metadata provision**: Name, description, and JSON Schema for parameters
//! - **Parameter validation**: Ensure arguments match expected schema
//! - **Execution**: Perform the tool's action and return structured results
//! - **Capability declaration**: Advertise requirements (filesystem, network, side effects)
//!
//! # Examples
//!
//! ```rust,ignore
//! use swage::tool::{Tool, ToolResult, ToolCapabilities};
//! use serde_json::json;
//!
//! struct Calculator;
//!
//! #[async_trait]
//! impl Tool for Calculator {
//!     fn name(&self) -> &str {
//!         "calculate"
//!     }
//!
//!     fn description(&self) -> &str {
//!         "Performs arithmetic calculations"
//!     }
//!
//!     fn parameters(&self) -> serde_json::Value {
//!         json!({
//!             "type": "object",
//!             "properties": {
//!                 "expression": {
//!                     "type": "string",
//!                     "description": "The arithmetic expression to evaluate"
//!                 }
//!             },
//!             "required": ["expression"]
//!         })
//!     }
//!
//!     fn capabilities(&self) -> ToolCapabilities {
//!         ToolCapabilities {
//!             filesystem_access: false,
//!             network_access: false,
//!             has_side_effects: false,
//!             async_only: false,
//!             long_running: false,
//!         }
//!     }
//!
//!     async fn execute(&self, args: serde_json::Value) -> Result<ToolResult, ToolError> {
//!         let expr = args["expression"].as_str()
//!             .ok_or_else(|| ToolError::InvalidParameters("missing expression".into()))?;
//!
//!         // Evaluation logic here
//!         let result = evaluate(expr)?;
//!
//!         Ok(ToolResult {
//!             output: json!({ "result": result }),
//!             success: true,
//!         })
//!     }
//! }
//! ```

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::error::ToolError;

/// Async trait for tool implementations.
///
/// Tools extend agent capabilities by providing discrete, well-defined actions.
/// Each tool declares its interface via JSON Schema and executes with structured
/// input/output.
///
/// # Thread Safety
///
/// Implementations must be `Send + Sync` to support concurrent access across async tasks.
///
/// # Parameter Validation
///
/// Tools receive pre-validated arguments matching their declared schema. Implementations
/// should still perform defensive validation for critical operations but can generally
/// assume structural correctness.
#[async_trait]
pub trait Tool: Send + Sync {
    /// Returns the canonical name for this tool.
    ///
    /// Must be unique within a tool registry. Used by agents to identify and invoke
    /// the tool. Should be lowercase, hyphen-separated (e.g., "file-read", "http-get").
    ///
    /// # Contract
    ///
    /// - Names must be stable across tool instances
    /// - Must not contain spaces or special characters (only alphanumeric and hyphens)
    /// - Should be concise but descriptive
    fn name(&self) -> &str;

    /// Returns a human-readable description of what this tool does.
    ///
    /// Used to help agents understand when and how to use the tool. Should include:
    /// - Primary purpose and use cases
    /// - Key behaviour or constraints
    /// - Any important limitations
    ///
    /// # Contract
    ///
    /// - Should be 1-3 sentences, clear and concise
    /// - Written for LLM consumption (guides tool selection)
    fn description(&self) -> &str;

    /// Returns the JSON Schema describing this tool's parameters.
    ///
    /// Defines the structure and validation rules for arguments passed to `execute()`.
    /// Must be a valid JSON Schema (draft 7 or later recommended).
    ///
    /// # Schema Requirements
    ///
    /// - Root must be an object type
    /// - All parameters should have descriptions to guide agents
    /// - Use `required` array for mandatory parameters
    /// - Include `type`, `format`, `pattern`, etc. for validation
    ///
    /// # Contract
    ///
    /// The schema returned must be stable for a given tool version. Changes to the
    /// schema constitute a breaking change in the tool's interface.
    ///
    /// # Examples
    ///
    /// ```json
    /// {
    ///     "type": "object",
    ///     "properties": {
    ///         "path": {
    ///             "type": "string",
    ///             "description": "File path to read"
    ///         },
    ///         "encoding": {
    ///             "type": "string",
    ///             "enum": ["utf-8", "ascii"],
    ///             "default": "utf-8"
    ///         }
    ///     },
    ///     "required": ["path"]
    /// }
    /// ```
    fn parameters(&self) -> serde_json::Value;

    /// Returns the capability requirements for this tool.
    ///
    /// Declares what resources and permissions the tool needs to function. Used for:
    /// - Security enforcement (deny tools that exceed allowed capabilities)
    /// - Performance planning (isolate long-running or async-only tools)
    /// - User consent (warn before running tools with side effects)
    ///
    /// # Contract
    ///
    /// Capabilities must accurately reflect the tool's actual behaviour. Under-declaring
    /// capabilities (e.g., claiming no filesystem access when the tool writes files)
    /// violates the security model.
    fn capabilities(&self) -> ToolCapabilities;

    /// Executes the tool with the provided arguments.
    ///
    /// Performs the tool's action and returns structured results. The `args` parameter
    /// contains JSON data matching the schema returned by `parameters()`.
    ///
    /// # Arguments
    ///
    /// - `args`: JSON value containing tool parameters. Guaranteed to validate against
    ///   the schema from `parameters()` when called through a proper tool registry.
    ///   Implementations should still validate critical invariants defensively.
    ///
    /// # Returns
    ///
    /// - `Ok(ToolResult)`: Successful execution. The `success` field indicates logical
    ///   success (true) or expected failure (false, e.g., file not found).
    /// - `Err(ToolError)`: Execution failed due to invalid parameters, permissions,
    ///   timeout, or unexpected errors.
    ///
    /// # Error Handling
    ///
    /// Use `ToolResult { success: false, ... }` for expected failures that don't indicate
    /// a bug or system error (e.g., "file not found" when reading a file).
    ///
    /// Use `Err(ToolError)` for:
    /// - Invalid arguments (despite schema validation)
    /// - Permission denied
    /// - Timeouts
    /// - Unexpected errors (I/O failures, panics caught)
    ///
    /// # Cancellation
    ///
    /// Tools should respect cancellation by checking the task's cancellation token
    /// periodically during long-running operations. Drop futures cleanly.
    async fn execute(&self, args: serde_json::Value) -> Result<ToolResult, ToolError>;
}

/// Result of a tool execution.
///
/// Distinguishes between successful execution with logical failure (success=false)
/// and execution errors (Err). For example, a file-read tool:
/// - Returns `ToolResult { success: false, ... }` if the file doesn't exist
/// - Returns `Err(ToolError)` if permission is denied or I/O fails unexpectedly
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ToolResult {
    /// Structured output from the tool execution.
    ///
    /// Format is tool-specific. Common patterns:
    /// - Success: `{ "content": "...", "metadata": {...} }`
    /// - Logical failure: `{ "error": "file not found", "path": "/foo/bar" }`
    ///
    /// Agents parse this to extract information or handle failures.
    pub output: serde_json::Value,

    /// Indicates logical success (true) or expected failure (false).
    ///
    /// When false, the tool executed correctly but couldn't complete its intended
    /// action (e.g., search returned no results, file doesn't exist).
    /// When true, the action completed as requested.
    pub success: bool,
}

/// Capability flags describing a tool's resource requirements and behaviour.
///
/// Used for security enforcement, performance planning, and user consent.
/// All fields default to false (no special requirements).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub struct ToolCapabilities {
    /// Tool requires filesystem access (read or write).
    ///
    /// Set to true if the tool:
    /// - Reads files or directories
    /// - Writes, modifies, or deletes files
    /// - Checks file existence or metadata
    pub filesystem_access: bool,

    /// Tool requires network access.
    ///
    /// Set to true if the tool:
    /// - Makes HTTP/HTTPS requests
    /// - Opens network sockets
    /// - Communicates with external services
    pub network_access: bool,

    /// Tool has side effects beyond returning a result.
    ///
    /// Set to true if the tool:
    /// - Modifies persistent state (files, databases, external services)
    /// - Sends notifications or messages
    /// - Performs actions that cannot be easily undone
    ///
    /// Read-only operations should set this to false.
    pub has_side_effects: bool,

    /// Tool can only be executed in an async context.
    ///
    /// Set to true if the tool:
    /// - Performs I/O operations that require async runtime
    /// - Depends on async dependencies
    /// - Cannot provide a synchronous execution path
    ///
    /// Most tools should set this to true given the async `execute()` signature.
    pub async_only: bool,

    /// Tool may take significant time to complete.
    ///
    /// Set to true if the tool:
    /// - Typically runs for >5 seconds
    /// - Makes multiple external requests
    /// - Performs complex computations
    ///
    /// Helps orchestrators schedule work appropriately and set timeouts.
    pub long_running: bool,
}

impl Default for ToolCapabilities {
    /// Creates a default ToolCapabilities with all flags set to false.
    ///
    /// Represents a pure, stateless, synchronous computation with no external
    /// dependencies. Most real tools will override at least some fields.
    fn default() -> Self {
        Self {
            filesystem_access: false,
            network_access: false,
            has_side_effects: false,
            async_only: false,
            long_running: false,
        }
    }
}
