# Emberforge Cross-Port Contract (v1)

## 0. Purpose & Scope

This document specifies the authoritative wire format and data schemas that all four implementations of Emberforge must comply with:
- **Rust (emberforge)** — reference implementation
- **C++ (emberforge-cpp)** — translation
- **Go (emberforge-go)** — translation
- **TypeScript (emberforge-ts)** — translation

This contract prevents drift by defining:
1. JSON schemas for persisted session records
2. Message block types and serialization rules
3. Hook event manifests and backends
4. Plugin and skill metadata formats
5. Tool invocation wire format

This is **not** an implementation guide. Language-specific details (lifetimes, async patterns, type systems) are left to each port. Only the **structured data on disk and over the wire** is specified here.

---

## 1. Session Record

A session is a persistent conversation transcript stored as JSON. It is the primary artifact exchanged between implementations.

### 1.1 JSON Schema

```json
{
  "type": "object",
  "required": ["version", "messages"],
  "properties": {
    "version": {
      "type": "integer",
      "description": "Schema version (currently 1)"
    },
    "messages": {
      "type": "array",
      "description": "Ordered list of conversation messages",
      "items": {"$ref": "#/definitions/ConversationMessage"}
    },
    "plan_mode": {
      "type": "boolean",
      "description": "When true, model operates in plan-only mode (no tool execution)",
      "default": false
    }
  },
  "definitions": {
    "ConversationMessage": {
      "type": "object",
      "required": ["role", "blocks"],
      "properties": {
        "role": {
          "type": "string",
          "enum": ["system", "user", "assistant", "tool"],
          "description": "Message originator"
        },
        "blocks": {
          "type": "array",
          "description": "Content blocks for this message",
          "items": {"$ref": "#/definitions/ContentBlock"}
        },
        "usage": {
          "type": "object",
          "description": "Optional token usage metrics",
          "$ref": "#/definitions/TokenUsage"
        }
      }
    },
    "ContentBlock": {
      "oneOf": [
        {"$ref": "#/definitions/TextBlock"},
        {"$ref": "#/definitions/ToolUseBlock"},
        {"$ref": "#/definitions/ToolResultBlock"}
      ]
    },
    "TextBlock": {
      "type": "object",
      "required": ["type", "text"],
      "properties": {
        "type": {"const": "text"},
        "text": {"type": "string"}
      }
    },
    "ToolUseBlock": {
      "type": "object",
      "required": ["type", "id", "name", "input"],
      "properties": {
        "type": {"const": "tool_use"},
        "id": {"type": "string", "description": "Unique tool invocation ID"},
        "name": {"type": "string", "description": "Tool name"},
        "input": {"type": "string", "description": "JSON string of tool input object"}
      }
    },
    "ToolResultBlock": {
      "type": "object",
      "required": ["type", "tool_use_id", "tool_name", "output", "is_error"],
      "properties": {
        "type": {"const": "tool_result"},
        "tool_use_id": {"type": "string", "description": "ID of the ToolUseBlock this answers"},
        "tool_name": {"type": "string", "description": "Name of the tool that executed"},
        "output": {"type": "string", "description": "Tool execution result (or error message)"},
        "is_error": {"type": "boolean", "description": "Whether output is an error"}
      }
    },
    "TokenUsage": {
      "type": "object",
      "required": ["input_tokens", "output_tokens", "cache_creation_input_tokens", "cache_read_input_tokens"],
      "properties": {
        "input_tokens": {"type": "integer", "minimum": 0},
        "output_tokens": {"type": "integer", "minimum": 0},
        "cache_creation_input_tokens": {"type": "integer", "minimum": 0, "default": 0},
        "cache_read_input_tokens": {"type": "integer", "minimum": 0, "default": 0}
      }
    }
  }
}
```

### 1.2 Example Session

```json
{
  "version": 1,
  "messages": [
    {
      "role": "user",
      "blocks": [
        {
          "type": "text",
          "text": "List the files in /tmp"
        }
      ]
    },
    {
      "role": "assistant",
      "blocks": [
        {
          "type": "text",
          "text": "I'll list the files in /tmp for you."
        },
        {
          "type": "tool_use",
          "id": "tooluse_abc123",
          "name": "bash",
          "input": "{\"command\": \"ls /tmp\"}"
        }
      ],
      "usage": {
        "input_tokens": 100,
        "output_tokens": 50,
        "cache_creation_input_tokens": 0,
        "cache_read_input_tokens": 0
      }
    },
    {
      "role": "tool",
      "blocks": [
        {
          "type": "tool_result",
          "tool_use_id": "tooluse_abc123",
          "tool_name": "bash",
          "output": "file1.txt\nfile2.log",
          "is_error": false
        }
      ]
    }
  ],
  "plan_mode": false
}
```

### 1.3 Authoritative Rust Source

- `/home/zeyufu/Desktop/emberforge/crates/runtime/src/session.rs` — `Session`, `ConversationMessage`, `ContentBlock`, `MessageRole`, `TokenUsage` structs
- Serialization: `serde` with `#[serde(tag = "type", rename_all = "snake_case")]` on `ContentBlock`
- On-disk format: UTF-8 JSON, formatted for readability

---

## 2. ConversationMessage Variants

Messages are always serialized with a `role` field and a `blocks` array. The role determines who authored the message; blocks describe the content.

### 2.1 UserText (Role: "user")

User messages may contain text blocks, tool results, or both. Most commonly a single `TextBlock`.

```json
{
  "role": "user",
  "blocks": [
    {
      "type": "text",
      "text": "What is 2+2?"
    }
  ]
}
```

### 2.2 UserImage (Role: "user")

*Note:* The Rust runtime does not currently define an `ImageBlock` variant. If images are added, this section will be updated. For now, images are transmitted via the API layer, not in the session record.

### 2.3 AssistantText (Role: "assistant")

Assistant messages may contain text and/or `ToolUse` blocks. The model generates these.

```json
{
  "role": "assistant",
  "blocks": [
    {
      "type": "text",
      "text": "I'll help you with that."
    },
    {
      "type": "tool_use",
      "id": "call_xyz789",
      "name": "read_file",
      "input": "{\"path\": \"/etc/hosts\"}"
    }
  ],
  "usage": {
    "input_tokens": 150,
    "output_tokens": 75,
    "cache_creation_input_tokens": 0,
    "cache_read_input_tokens": 0
  }
}
```

### 2.4 ToolUse

Not a message role, but a content block type. Appears in assistant messages. Represents a single tool invocation request.

**Wire name:** `tool_use` (snake_case)

**Fields:**
- `id` (string): Unique identifier for this tool call (user-assigned or model-generated)
- `name` (string): Tool name (e.g., "bash", "read_file")
- `input` (string): JSON-serialized tool input object

Example:
```json
{
  "type": "tool_use",
  "id": "tool_1",
  "name": "bash",
  "input": "{\"command\": \"echo hello\"}"
}
```

### 2.5 ToolResult

Not a message role, but a content block type. Appears in tool messages (role: "tool"). Represents the result of executing a tool.

**Wire name:** `tool_result` (snake_case)

**Fields:**
- `tool_use_id` (string): ID of the `ToolUseBlock` this answers
- `tool_name` (string): Name of the tool that was executed
- `output` (string): Result of execution (or error message if `is_error=true`)
- `is_error` (boolean): Whether the output represents an error

Example:
```json
{
  "role": "tool",
  "blocks": [
    {
      "type": "tool_result",
      "tool_use_id": "tool_1",
      "tool_name": "bash",
      "output": "hello",
      "is_error": false
    }
  ]
}
```

---

## 3. Tool Invocation Wire Format

### 3.1 ToolSpec (Tool Definition)

A ToolSpec describes a single tool available to the model. Tools are registered in a global registry and passed to the API layer.

**Wire representation (JSON):**

```json
{
  "type": "object",
  "required": ["name", "description", "input_schema", "required_permission"],
  "properties": {
    "name": {
      "type": "string",
      "description": "Unique tool identifier (e.g., 'bash', 'read_file')"
    },
    "description": {
      "type": "string",
      "description": "Human-readable tool description"
    },
    "input_schema": {
      "type": "object",
      "description": "JSON Schema defining the tool's input parameters"
    },
    "required_permission": {
      "type": "string",
      "enum": ["read_only", "workspace_write", "danger_full_access"],
      "description": "Minimum permission level required to execute this tool"
    }
  }
}
```

**Example:**

```json
{
  "name": "read_file",
  "description": "Read the contents of a file",
  "input_schema": {
    "type": "object",
    "required": ["path"],
    "properties": {
      "path": {
        "type": "string",
        "description": "Absolute file path"
      },
      "offset": {
        "type": "integer",
        "description": "Optional: byte offset to start reading from"
      },
      "limit": {
        "type": "integer",
        "description": "Optional: maximum bytes to read"
      }
    }
  },
  "required_permission": "read_only"
}
```

### 3.2 Required Permission Enum

Tool permissions are ordered from least to most restrictive:

| Permission | On-Wire Name | Meaning |
|-----------|--------------|---------|
| ReadOnly | `read_only` | Read files, list directories, search (no writes, no execution) |
| WorkspaceWrite | `workspace_write` | Read/write files in the project workspace |
| DangerFullAccess | `danger_full_access` | Execute arbitrary commands, modify system state |

**Serialization:** Use kebab-case (lower-case with hyphens). Rust source uses `#[serde(rename_all = "kebab-case")]`.

### 3.3 Input/Output Content Block Variants

#### API InputMessage (sent to LLM provider)

```json
{
  "role": "user",
  "content": [
    {
      "type": "text",
      "text": "Hello"
    }
  ]
}
```

or

```json
{
  "role": "user",
  "content": [
    {
      "type": "tool_result",
      "tool_use_id": "call_123",
      "content": [
        {
          "type": "text",
          "text": "Result here"
        }
      ],
      "is_error": false
    }
  ]
}
```

#### API OutputMessage (received from LLM provider)

```json
{
  "id": "msg_abc123",
  "type": "message",
  "role": "assistant",
  "content": [
    {
      "type": "text",
      "text": "I can help"
    },
    {
      "type": "tool_use",
      "id": "tooluse_xyz",
      "name": "bash",
      "input": {"command": "ls /tmp"}
    }
  ],
  "model": "claude-opus-4-6",
  "stop_reason": "tool_use",
  "usage": {
    "input_tokens": 100,
    "output_tokens": 50,
    "cache_creation_input_tokens": 0,
    "cache_read_input_tokens": 0
  }
}
```

**Note:** API layer input/output blocks use `type` field to discriminate; values like `tool_use`, `tool_result`, `text`, `thinking` are snake_case.

---

## 4. Hook Manifest

Hooks allow plugins and external systems to react to runtime events (tool use, session lifecycle, etc.).

### 4.1 JSON Schema

```json
{
  "type": "object",
  "required": ["event"],
  "properties": {
    "event": {
      "type": "string",
      "enum": [
        "PreToolUse",
        "PostToolUse",
        "SessionStart",
        "SessionEnd",
        "SubagentStart",
        "SubagentStop",
        "CompactStart",
        "CompactEnd",
        "ToolError",
        "PermissionDenied",
        "ConfigChange",
        "UserPromptSubmit",
        "Notification",
        "PluginLoad",
        "PluginUnload",
        "CwdChanged",
        "FileChanged"
      ]
    },
    "type": {
      "type": "string",
      "enum": ["command", "http"],
      "description": "Execution backend type"
    },
    "run": {
      "type": "string",
      "description": "Shell command to execute (for type='command')"
    },
    "url": {
      "type": "string",
      "description": "HTTP endpoint to POST to (for type='http')"
    },
    "headers": {
      "type": "object",
      "description": "Custom HTTP headers (for type='http')",
      "additionalProperties": {"type": "string"}
    },
    "match": {
      "type": "object",
      "description": "Optional match rule for tool events",
      "properties": {
        "tool_names": {
          "type": "array",
          "items": {"type": "string"},
          "description": "Only trigger for these tool names (empty = all)"
        },
        "commands": {
          "type": "array",
          "items": {"type": "string"},
          "description": "Glob patterns for bash command inputs (e.g., 'rm *', 'git push*')"
        }
      }
    },
    "timeout_secs": {
      "type": "integer",
      "minimum": 1,
      "default": 30,
      "description": "Timeout in seconds"
    },
    "async": {
      "type": "boolean",
      "default": false,
      "description": "Run asynchronously (non-blocking)"
    },
    "status_message": {
      "type": "string",
      "description": "Custom status message during execution"
    },
    "once": {
      "type": "boolean",
      "default": false,
      "description": "Fire once, then auto-remove"
    }
  }
}
```

### 4.2 17 Hook Event Types

| Event Type | Trigger | Context (tool_name) | Example Use |
|----------|---------|-------------------|-------------|
| PreToolUse | Before tool execution | tool_name, tool_input | Validation, audit logging |
| PostToolUse | After successful tool execution | tool_name, tool_input, tool_output | Notification, cleanup |
| SessionStart | Session begins | (none) | Initialize logging |
| SessionEnd | Session terminates | (none) | Archive session, cleanup |
| SubagentStart | Subagent spawned | (none) | Track delegation |
| SubagentStop | Subagent completes | (none) | Collect results |
| CompactStart | Session compaction begins | (none) | Notify before compression |
| CompactEnd | Session compaction completes | (none) | Notify after compression |
| ToolError | Tool execution fails | tool_name, tool_input, error | Error tracking |
| PermissionDenied | Tool blocked by permission | tool_name | Security logging |
| ConfigChange | Runtime config updated | (context varies) | Reload settings |
| UserPromptSubmit | User submits prompt | (user text) | Rate limiting |
| Notification | Generic notification | (varies) | Alert system |
| PluginLoad | Plugin loaded | plugin_id | Plugin activity log |
| PluginUnload | Plugin unloaded | plugin_id | Plugin activity log |
| CwdChanged | Working directory changed | new_cwd | Update file watchers |
| FileChanged | Project file modified | file_path | Trigger rebuilds |

### 4.3 Match Rules

Match rules filter which tool calls trigger a hook (only for tool events).

**Fields:**
- `tool_names` (array of strings): If non-empty, only trigger if tool name is in this list. Empty = match all.
- `commands` (array of strings): Glob patterns for bash command inputs. Example: `"rm *"` matches any `bash` tool input containing `rm` followed by any characters.

**Example:**

```json
{
  "event": "PreToolUse",
  "type": "command",
  "run": "bash hooks/validate.sh",
  "match": {
    "tool_names": ["bash"],
    "commands": ["rm *", "git push*"]
  }
}
```

This fires before bash execution only if the command contains `rm ` or starts with `git push`.

### 4.4 Backends

#### Command Backend

Executes a shell command. Exit code semantics:

| Code | Behavior |
|------|----------|
| 0 | Allow, optionally capture stdout as message |
| 2 | Deny (block tool execution) |
| Other | Warn (allow but log) |

**On-Wire:**

```json
{
  "event": "PreToolUse",
  "type": "command",
  "run": "echo checking && exit 0"
}
```

#### HTTP Backend

POST the hook payload (JSON) to a URL. Returns the response body as a message.

**Payload structure:**

```json
{
  "hook_event_name": "PreToolUse",
  "tool_name": "bash",
  "tool_input": {"command": "ls /tmp"},
  "tool_input_json": "{\"command\": \"ls /tmp\"}",
  "tool_output": null,
  "tool_result_is_error": false
}
```

**On-Wire:**

```json
{
  "event": "PreToolUse",
  "type": "http",
  "url": "https://example.com/hooks/pre-tool",
  "headers": {
    "Authorization": "Bearer token123",
    "X-Custom": "value"
  }
}
```

---

## 5. Plugin Manifest

A plugin is a distributable extension providing tools, commands, and hooks. The manifest is stored as `plugin.json` in the plugin's root directory.

### 5.1 JSON Schema

```json
{
  "type": "object",
  "required": ["name", "version", "description", "permissions"],
  "properties": {
    "name": {"type": "string"},
    "version": {
      "type": "string",
      "description": "Semantic version (e.g., '1.0.0')"
    },
    "description": {"type": "string"},
    "permissions": {
      "type": "array",
      "items": {
        "type": "string",
        "enum": ["read", "write", "execute"]
      },
      "description": "Plugin permission requests"
    },
    "defaultEnabled": {
      "type": "boolean",
      "default": false,
      "description": "Auto-enable this plugin by default"
    },
    "hooks": {
      "type": "object",
      "description": "Hook command registrations",
      "properties": {
        "PreToolUse": {
          "type": "array",
          "items": {"type": "string"},
          "description": "Shell commands to run on PreToolUse"
        },
        "PostToolUse": {
          "type": "array",
          "items": {"type": "string"},
          "description": "Shell commands to run on PostToolUse"
        }
      }
    },
    "lifecycle": {
      "type": "object",
      "description": "Plugin lifecycle hooks",
      "properties": {
        "Init": {
          "type": "array",
          "items": {"type": "string"},
          "description": "Commands to run on plugin load"
        },
        "Shutdown": {
          "type": "array",
          "items": {"type": "string"},
          "description": "Commands to run on plugin unload"
        }
      }
    },
    "tools": {
      "type": "array",
      "items": {"$ref": "#/definitions/PluginToolManifest"}
    },
    "commands": {
      "type": "array",
      "items": {"$ref": "#/definitions/PluginCommandManifest"}
    }
  },
  "definitions": {
    "PluginToolManifest": {
      "type": "object",
      "required": ["name", "description", "inputSchema", "command", "requiredPermission"],
      "properties": {
        "name": {"type": "string"},
        "description": {"type": "string"},
        "inputSchema": {
          "type": "object",
          "description": "JSON Schema for tool inputs"
        },
        "command": {"type": "string"},
        "args": {
          "type": "array",
          "items": {"type": "string"},
          "default": []
        },
        "requiredPermission": {
          "type": "string",
          "enum": ["read-only", "workspace-write", "danger-full-access"]
        }
      }
    },
    "PluginCommandManifest": {
      "type": "object",
      "required": ["name", "description", "command"],
      "properties": {
        "name": {"type": "string"},
        "description": {"type": "string"},
        "command": {"type": "string"}
      }
    }
  }
}
```

### 5.2 Bundled vs External Plugins

Plugins are classified by source:

| Kind | Location | Disabled By | Editable |
|------|----------|-------------|----------|
| builtin | Compiled into runtime | Not disableable | No |
| bundled | `.ember/plugins/` (project-level) | `settings.json` | Yes |
| external | `~/.ember/plugins/` (user-level) | `settings.json` | Yes |

On-wire, plugins declare their marketplace: "builtin", "bundled", or "external".

---

## 6. Skill Frontmatter

A skill is a callable unit of work defined in markdown with YAML frontmatter. Skills live in `.ember/skills/` and are invoked via the Skill tool.

### 6.1 YAML Frontmatter Schema

```yaml
---
name: string              # Required: Skill identifier (e.g., 'list-files')
description: string       # Required: One-sentence description
triggers: [string]        # Optional: Keywords that activate this skill
source: string           # Optional: Source location or attribution
# Additional fields may be present; parsers must be lenient.
---
```

**Example:**

```yaml
---
name: find-large-files
description: Locate files larger than a specified threshold in a directory.
triggers:
  - "find large files"
  - "disk usage"
source: built-in
---
```

### 6.2 Body Conventions

After the frontmatter closing `---`, the rest of the file is the **skill body**. There is no fixed format:
- May be markdown prose and examples
- May be runnable code
- May be step-by-step instructions
- Parsers should treat it as opaque text

The body is passed to the model when the skill is invoked, allowing the skill to provide context, examples, and instructions.

**Example body:**

```markdown
This skill finds all files in a directory tree larger than the specified size.
It uses the `find` command with `-size` flag.

Usage:
  find-large-files /path 100M

This will list all files larger than 100 megabytes in /path and subdirectories.
```

---

## 7. Authoritative Rust Source Index

| Contract Section | Rust Source File | Type/Struct |
|-----------------|------------------|------------|
| Session Record | `crates/runtime/src/session.rs` | `Session`, `ConversationMessage`, `ContentBlock`, `MessageRole` |
| Token Usage | `crates/runtime/src/session.rs` | `TokenUsage` |
| Hook Event | `crates/runtime/src/hooks.rs` | `HookEvent` (enum) |
| Hook Manifest | `crates/runtime/src/hooks.rs` | `HookDefinition`, `HookBackend`, `HookMatchRule` |
| Tool Spec | `crates/tools/src/registry.rs` | `ToolSpec` |
| Tool Permission | `crates/runtime/src/lib.rs` | `PermissionMode` (enum) |
| Plugin Manifest | `crates/plugins/src/types.rs` | `PluginManifest`, `PluginToolManifest` |
| Plugin Permission | `crates/plugins/src/types.rs` | `PluginPermission`, `PluginToolPermission` (enums) |
| Agent Definition | `crates/runtime/src/agent_loader.rs` | `AgentDefinition` |
| Memory Frontmatter | `crates/runtime/src/memory.rs` | `MemoryFrontmatter`, `MemoryType` |

---

## 8. Canonical Default Model Registry

All four ports of Emberforge should use the **same default model name** when no model is explicitly provided or configured via environment variables. This ensures consistent behavior across implementations.

### 8.1 Recommended Canonical Default: `qwen3:8b`

**Rationale:**
- **Local-only**: Does not require API keys or internet connectivity (works via Ollama)
- **Matches existing cpp port**: The C++ port already defaults to `qwen3:8b`, establishing a precedent
- **Low friction**: Ollama is widely available and easy to install
- **No licensing concerns**: Open-source model

### 8.2 Current Port Defaults (Iteration 1)

| Port | Current default | Source | Notes |
|---|---|---|---|
| Rust | (no hardcoded default) | `crates/api/src/client.rs` | Caller must provide model; no fallback constant |
| C++ | `qwen3:8b` | `apps/ember_cli/main.cpp:14` | Set via env var `EMBER_MODEL` or hardcoded string |
| Go | `claude-sonnet-4-6` | `pkg/api/provider.go:3` (const `DefaultModel`) | API-based model; requires `ANTHROPIC_API_KEY` |
| TypeScript | `llama3.2` | `packages/api/src/ollama_provider.ts:10` | Set in constructor default parameter |

### 8.3 Migration Path (Iteration 3 and beyond)

**Note:** This section documents the canonical target; ports are **NOT** expected to align immediately. Aligning all four ports to use `qwen3:8b` as the default is deferred to a future iteration (iter3 or later) to avoid unnecessary churn in iter2.

When ports do align, the changes will be:
1. Rust: Add a constant `const DEFAULT_MODEL: &str = "qwen3:8b"` in `crates/api/src/client.rs` and use it as the fallback when no model is provided
2. Go: Update `const DefaultModel = "qwen3:8b"` in `pkg/api/provider.go:3`
3. TypeScript: Update the default parameter in `packages/api/src/ollama_provider.ts` constructor from `"llama3.2"` to `"qwen3:8b"`
4. C++: No change needed (already at the canonical default)

---

## 9. Conformance Checklist

For each port to be considered compliant, it must:

- [ ] **Session I/O**: Load and save sessions from/to JSON matching the schema in § 1.1
- [ ] **Message Roles**: Support all four message roles (system, user, assistant, tool)
- [ ] **Content Blocks**: Serialize ContentBlock variants (text, tool_use, tool_result) with `type` discriminator
- [ ] **Tool Specs**: Define tools with name, description, input_schema, required_permission
- [ ] **Permissions**: Serialize permissions as: `read_only`, `workspace_write`, `danger_full_access`
- [ ] **Hook Events**: Support all 17 event types listed in § 4.2
- [ ] **Hook Backends**: Support both command and HTTP backends with proper exit code semantics
- [ ] **Plugin Manifest**: Load and validate `plugin.json` matching schema in § 5.1
- [ ] **Agent Definitions**: Load agent JSON files with camelCase field names (agentType, displayName, etc.)
- [ ] **Skill Frontmatter**: Parse YAML frontmatter (name, description, triggers, source) and preserve body
- [ ] **Wire Format**: All persisted JSON uses snake_case field names (session_mode, tool_use_id, etc.) unless explicitly otherwise (e.g., agentType in agent definitions)
- [ ] **Error Handling**: Reject non-compliant JSON with clear error messages; gracefully skip unrecognized hook event types

---

## Notes on Snake Case vs camelCase

The contract uses **snake_case** for session records and runtime schemas (e.g., `tool_use_id`, `input_tokens`, `required_permission`). However, agent definitions use **camelCase** (e.g., `agentType`, `displayName`) to match their JSON-driven authorship pattern. When reading the authoritative Rust source, check the `#[serde(...)]` attributes — they determine on-wire names, not Rust field names.

---

## Version History

- **v1** (2026-04-07): Initial contract. Covers sessions, messages, hooks, plugins, tools, agents, skills.
