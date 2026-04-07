use std::fmt;

/// Structured error type for tool execution, replacing `Result<T, String>`.
#[derive(Debug)]
pub enum ToolExecError {
    /// JSON deserialization of tool input failed.
    Deserialize(serde_json::Error),
    /// JSON serialization of tool output failed.
    Serialize(serde_json::Error),
    /// File I/O error during tool execution.
    Io(std::io::Error),
    /// HTTP request error (`WebFetch`, `WebSearch`).
    Http(String),
    /// The requested tool name is not registered.
    UnsupportedTool(String),
    /// Generic runtime error from a tool handler.
    Runtime(String),
}

impl fmt::Display for ToolExecError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Deserialize(err) => write!(f, "tool input error: {err}"),
            Self::Serialize(err) => write!(f, "tool output error: {err}"),
            Self::Io(err) => write!(f, "{err}"),
            Self::Http(msg) | Self::Runtime(msg) => write!(f, "{msg}"),
            Self::UnsupportedTool(name) => write!(f, "unsupported tool: {name}"),
        }
    }
}

impl std::error::Error for ToolExecError {}

impl From<serde_json::Error> for ToolExecError {
    fn from(err: serde_json::Error) -> Self {
        Self::Serialize(err)
    }
}

impl From<std::io::Error> for ToolExecError {
    fn from(err: std::io::Error) -> Self {
        Self::Io(err)
    }
}

impl From<String> for ToolExecError {
    fn from(msg: String) -> Self {
        Self::Runtime(msg)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_unsupported_tool() {
        let err = ToolExecError::UnsupportedTool("FakeTool".to_string());
        assert_eq!(err.to_string(), "unsupported tool: FakeTool");
    }

    #[test]
    fn display_runtime_error() {
        let err = ToolExecError::Runtime("something went wrong".to_string());
        assert_eq!(err.to_string(), "something went wrong");
    }

    #[test]
    fn display_io_error() {
        let err = ToolExecError::Io(std::io::Error::new(std::io::ErrorKind::NotFound, "file not found"));
        assert!(err.to_string().contains("file not found"));
    }

    #[test]
    fn from_string_creates_runtime() {
        let err: ToolExecError = "test error".to_string().into();
        assert!(matches!(err, ToolExecError::Runtime(_)));
    }

    #[test]
    fn from_io_error() {
        let io_err = std::io::Error::new(std::io::ErrorKind::PermissionDenied, "denied");
        let err: ToolExecError = io_err.into();
        assert!(matches!(err, ToolExecError::Io(_)));
    }
}
