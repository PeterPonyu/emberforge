pub mod error;
mod executor;
mod implementations;
mod registry;
mod specs;
pub mod team_helpers;
mod types;

#[cfg(test)]
mod tests;

// Re-export the public API (same surface as before the split).
pub use error::ToolExecError;
pub use executor::execute_tool;
pub use registry::{
    GlobalToolRegistry, ToolManifestEntry, ToolRegistry, ToolSource, ToolSpec,
};
pub use specs::mvp_tool_specs;
