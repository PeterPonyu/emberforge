mod hooks;
pub mod error;
pub mod manager;
pub mod registry;
pub mod types;

#[cfg(test)]
mod tests;

// Re-export the public API (same surface as before the split).
pub use error::{PluginError, PluginManifestValidationError};
pub use hooks::{HookEvent, HookRunResult, HookRunner};
pub use manager::{
    InstallOutcome, PluginManager, PluginManagerConfig, UpdateOutcome,
};
pub use registry::{PluginRegistry, PluginSummary, RegisteredPlugin};
pub use types::{
    BundledPlugin, BuiltinPlugin, ExternalPlugin, InstalledPluginRecord, InstalledPluginRegistry,
    Plugin, PluginCommandManifest, PluginDefinition, PluginHooks, PluginInstallSource, PluginKind,
    PluginLifecycle, PluginManifest, PluginMetadata, PluginPermission, PluginTool,
    PluginToolDefinition, PluginToolManifest, PluginToolPermission,
};
