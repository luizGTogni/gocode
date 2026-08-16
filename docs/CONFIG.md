# Gocode — Configuration Specification

**Status:** Initial technical draft  
**Product:** Gocode  
**Target version:** v0.1.0  
**Scope:** Global configuration, project configuration, secrets, resolution, migrations

---

# 1. Purpose

This document defines the configuration system used by Gocode.

The configuration architecture must support:

- global user settings;
- project-specific settings;
- provider selection;
- model selection;
- thinking settings;
- update preferences;
- agent defaults;
- future migrations;
- secure credential separation;
- deterministic precedence.

The normal user should not need to edit configuration files manually.

Configuration files exist primarily for persistence, advanced use, debugging, and automation.

---

# 2. Core Principle

Configuration should be automatic by default and editable when needed.

The expected normal flow is:

```text
gocode
↓
onboarding / UI settings
↓
Gocode writes config
↓
future launches reuse it
```

Not:

```text
read documentation
↓
create TOML manually
↓
guess keys
↓
fix syntax errors
```

---

# 3. Configuration Layers

Gocode has two primary persistent configuration scopes:

```text
Global configuration
Project configuration
```

Conceptually:

```text
~/.gocode/config.toml
<project>/.gocode/project.toml
```

---

# 4. Global Directory

On Windows v0.1.0:

```text
%USERPROFILE%\.gocode\
```

Suggested structure:

```text
.gocode\
├── config.toml
├── state.json
├── models.json
├── logs\
├── cache\
└── bin\
```

The global path must be resolved through one internal abstraction so future Linux/macOS support does not require changing business logic.

---

# 5. Project Directory

Inside the detected project root:

```text
<project-root>\.gocode\
```

Suggested structure:

```text
.gocode\
├── project.toml
├── instructions.md
└── sessions\
```

The directory should be created automatically.

---

# 6. Configuration Responsibilities

Global config should contain settings that normally apply across projects.

Examples:

- default provider;
- default model;
- UI preferences;
- update behavior;
- global agent defaults.

Project config should contain settings that apply only to the current project.

Examples:

- project name;
- provider override;
- model override;
- project-specific thinking mode;
- instructions path;
- project-specific agent behavior.

---

# 7. Global Configuration Example

Conceptual example:

```toml
schema_version = 1

default_provider = "nvidia"
default_model = "model-id"

[ui]
theme = "system"
show_thinking_summary = true

[agent]
validate_after_edit = true

[updates]
check_on_startup = true
```

Exact keys may evolve before v0.1.0.

---

# 8. Project Configuration Example

```toml
schema_version = 1

[project]
name = "my-project"

[agent]
instructions = "instructions.md"

[model]
provider = "nvidia"
model = "model-id"
thinking = "auto"
```

The project configuration should remain intentionally small.

---

# 9. Configuration Precedence

Recommended resolution order:

```text
CLI flags
↓
Project config
↓
Global config
↓
Provider defaults
↓
Built-in defaults
```

Higher layers override lower layers.

---

# 10. Resolved Configuration

The rest of the application should consume normalized configuration.

Conceptual type:

```rust
pub struct ResolvedConfig {
    pub provider: ProviderId,
    pub model: Option<ModelId>,
    pub thinking: ThinkingSettings,
    pub ui: UiConfig,
    pub updates: UpdateConfig,
    pub agent: AgentConfig,
}
```

This keeps precedence logic out of the Agent and TUI.

---

# 11. Config Loading Flow

```text
resolve global path
↓
load global config
↓
detect project root
↓
load project config
↓
apply CLI overrides
↓
validate
↓
resolve defaults
↓
ResolvedConfig
```

---

# 12. Missing Config Files

## UI preferences

Gocode stores user-interface preferences separately in the global configuration directory as
`preferences.toml`. It is schema-versioned and ignores unknown fields so newer versions remain
compatible with older clients. If it is malformed, Gocode starts with safe defaults, reports a
recovery warning, and leaves the original file untouched.

The file persists only the selected theme, default personality, and key bindings. It never
contains credentials, permissions, tools, model settings, or safety policy.

### Extending preferences in code

- Add a `KeyAction` and its default binding in `crates/gocode-core/src/preferences.rs`; the
  `/keymap` command discovers it from `KeyAction::all`.
- Add a `ThemeName` and its semantic `ThemeTokens` palette in `crates/gocode-tui/src/lib.rs`.
  Components should consume semantic tokens (background, primary/secondary text, border,
  highlight, success/warning/error, command, diffs, approval, danger), never a new literal
  colour.
- Add a `PersonalityName` plus its presentation instruction in
  `crates/gocode-agent/src/context.rs`. The instruction must remain presentation-only and must
  explicitly defer to system rules, project instructions, user requests, tools, and permissions.

Missing config is not an error.

If global config does not exist:

```text
create default config when needed
```

If project config does not exist:

```text
create minimal project config when needed
```

The first startup should not fail because files are absent.

---

# 13. Invalid Config

If a config file exists but cannot be parsed:

Gocode should:

1. preserve the file;
2. show a clear error;
3. avoid silently overwriting it;
4. offer recovery if practical.

Example UI message:

```text
Gocode could not read your project configuration.

The file was not changed.

[ Open details ] [ Continue with defaults ]
```

Exact UX belongs to the TUI layer.

---

# 14. Config Validation

Validation should happen after parsing.

Examples:

- unknown provider;
- invalid model identifier;
- invalid thinking value;
- negative timeout;
- unsupported schema version.

Do not rely only on TOML parsing.

---

# 15. Unknown Keys

Recommended MVP behavior:

```text
allow unknown keys with warning
```

This provides forward compatibility.

Strict rejection can make downgrade/upgrade workflows harder.

---

# 16. Schema Version

Every persistent config file should have:

```toml
schema_version = 1
```

The config schema version is independent from the Gocode application version.

---

# 17. Schema Compatibility

Conceptually:

```text
config schema <= supported schema
→ load / migrate
```

```text
config schema > supported schema
→ do not rewrite blindly
```

A newer config opened by an older Gocode version should produce a safe warning.

---

# 18. Migration Architecture

Conceptual:

```rust
trait ConfigMigration {
    fn from_version(&self) -> u32;
    fn to_version(&self) -> u32;
    fn migrate(&self, value: toml::Value) -> Result<toml::Value>;
}
```

A heavy migration framework is not required initially.

Simple ordered migration functions are sufficient.

---

# 19. Migration Rules

Migrations should be:

- deterministic;
- idempotent where possible;
- tested;
- safe;
- backed up before risky changes.

---

# 20. Migration Backup

Before rewriting a config during migration:

```text
config.toml
↓
config.toml.bak
↓
write migrated config
```

Backup behavior can be refined later.

---

# 21. Atomic Writes

Configuration writes should be atomic where practical.

Recommended pattern:

```text
write temp file
↓
flush
↓
replace target
```

This reduces corruption risk.

---

# 22. Formatting Preservation

Preserving user comments and formatting is desirable but not mandatory for the first internal implementation.

If config is primarily written by Gocode, a normalized rewrite is acceptable.

Before public v0.1.0, evaluate whether comment-preserving TOML editing is needed.

---

# 23. Credentials Are Not Configuration

API keys and secrets must not be stored in ordinary TOML files.

Do not write:

```toml
api_key = "nvapi-..."
```

to:

```text
~/.gocode/config.toml
```

---

# 24. Credential Resolution

Recommended priority:

```text
environment variable
↓
OS credential store
↓
onboarding
```

For NVIDIA:

```text
NVIDIA_API_KEY
```

---

# 25. Credential Store

Conceptual interface:

```rust
pub trait CredentialStore {
    async fn get(
        &self,
        key: CredentialKey,
    ) -> Result<Option<SecretString>, CredentialError>;

    async fn set(
        &self,
        key: CredentialKey,
        value: SecretString,
    ) -> Result<(), CredentialError>;

    async fn delete(
        &self,
        key: CredentialKey,
    ) -> Result<(), CredentialError>;
}
```

---

# 26. Windows Credential Storage

v0.1.0 target:

```text
Windows Credential Manager
```

The specific Rust crate should be selected based on:

- Windows reliability;
- maintenance;
- secure API behavior;
- cross-platform future compatibility.

---

# 27. Credential Profiles

Future-friendly concept:

```text
provider = nvidia
profile = default
```

Config may store only the profile name.

Example:

```toml
[providers.nvidia]
credential_profile = "default"
```

The secret itself remains outside the file.

---

# 28. Environment Variable Override

If:

```text
NVIDIA_API_KEY
```

exists, Gocode may use it without persisting it.

This is useful for:

- CI;
- temporary shells;
- development;
- managed environments.

---

# 29. Credential Source

Internally, Gocode may track:

```rust
pub enum CredentialSource {
    Environment,
    OsStore,
    SessionInput,
}
```

Never log the secret value.

---

# 30. Global Config Types

Conceptual:

```rust
pub struct GlobalConfig {
    pub schema_version: u32,
    pub default_provider: Option<ProviderId>,
    pub default_model: Option<ModelId>,
    pub ui: UiConfig,
    pub agent: AgentConfig,
    pub updates: UpdateConfig,
}
```

---

# 31. Project Config Types

Conceptual:

```rust
pub struct ProjectConfig {
    pub schema_version: u32,
    pub project: ProjectSection,
    pub model: Option<ProjectModelConfig>,
    pub agent: ProjectAgentConfig,
}
```

Only overrideable values should exist here.

---

# 32. UI Config

Possible MVP fields:

```rust
pub struct UiConfig {
    pub theme: ThemeMode,
    pub show_thinking_summary: bool,
}
```

Theme may remain effectively fixed/system in the first release.

---

# 33. Agent Config

Possible:

```rust
pub struct AgentConfig {
    pub validate_after_edit: bool,
    pub limits: AgentLimitConfig,
}
```

Avoid exposing every internal limit to users in v0.1.0.

---

# 34. Update Config

```rust
pub struct UpdateConfig {
    pub check_on_startup: bool,
}
```

Do not add:

```text
ignore_version
```

for the initial update behavior.

The user should be asked again on future startups if they decline.

---

# 35. Model Config

Conceptual project/global state:

```rust
pub struct ModelSelectionConfig {
    pub provider: ProviderId,
    pub model: ModelId,
    pub thinking: ThinkingModeConfig,
}
```

Capabilities must still be revalidated on startup.

---

# 36. Thinking Config

Persistent user-facing values should remain provider-independent.

Examples:

```text
auto
off
on
effort:<value>
budget:<tokens>
```

The exact TOML representation can be structured.

Example:

```toml
[model.thinking]
mode = "auto"
```

For effort:

```toml
[model.thinking]
mode = "effort"
value = "high"
```

---

# 37. Thinking Validation

A saved thinking configuration may become invalid if:

- model changes;
- provider metadata changes;
- model capability changes.

On resolution:

```text
saved setting
↓
validate against ModelCapabilities
↓
if invalid → fall back to Auto + notify user
```

Do not fail startup.

---

# 38. Default Values

Built-in defaults should be centralized.

Conceptually:

```rust
impl Default for UiConfig
impl Default for AgentConfig
impl Default for UpdateConfig
```

Avoid scattered literal defaults.

---

# 39. Default Provider

For the v0.1.0 onboarding path:

```text
NVIDIA NIM
```

is the only provider.

Still, store the selection explicitly once configured.

---

# 40. Default Model

Do not permanently hardcode a single model ID if the NVIDIA catalog is dynamic.

Preferred:

```text
user-selected model
```

If no model is saved:

```text
open model picker
```

A recommendation system may preselect a candidate.

---

# 41. Project Instructions Path

Default:

```toml
[agent]
instructions = "instructions.md"
```

Resolved relative to:

```text
<project>/.gocode/
```

---

# 42. Missing Instructions File

If configured file does not exist:

- continue;
- treat as no project instructions;
- optionally show a subtle warning.

Do not fail Agent startup.

---

# 43. State vs Config

Not all persisted data belongs in TOML config.

Use:

```text
config.toml
```

for user settings.

Use:

```text
state.json
```

for transient application state when needed.

Examples:

- last selected screen;
- last successful metadata refresh;
- internal non-user-facing state.

Keep the distinction clear.

---

# 44. `state.json`

Potential global state:

```json
{
  "schema_version": 1,
  "last_provider": "nvidia"
}
```

Do not store credentials.

---

# 45. Model Cache vs Config

Model metadata belongs in cache/state, not user config.

Do not serialize full model capability catalogs into:

```text
config.toml
```

---

# 46. Config Reload

v0.1.0 does not need live filesystem watching.

Config changes made through the TUI can update runtime state immediately.

Manual external edits may require restart or explicit reload.

---

# 47. CLI Overrides

Future/initial useful flags may include:

```text
--model
--provider
--debug
```

CLI overrides should normally apply to the current process only unless explicitly persisted.

---

# 48. CLI Persistence

Example:

```text
gocode --model foo
```

should not silently rewrite the user's default model unless documented.

Runtime override:

```text
CLI
>
project
>
global
```

---

# 49. Config Paths

Path resolution functions should be centralized.

Examples:

```rust
global_gocode_dir()
global_config_path()
project_gocode_dir(project_root)
project_config_path(project_root)
```

---

# 50. Platform Abstraction

Do not spread:

```rust
std::env::var("USERPROFILE")
```

through the application.

Use a platform/path service.

Future Linux/macOS logic can live there.

---

# 51. Path Normalization

Config path values should use platform-aware `PathBuf`.

Persist relative project paths when possible.

Example:

```text
instructions.md
```

not an absolute machine-specific path.

---

# 52. Logging Configuration

Debug logging level may eventually be configurable.

Do not expose secret logging.

Possible:

```toml
[logging]
level = "info"
```

Not required for v0.1.0 UI.

---

# 53. Unknown Provider Config

If config references:

```text
provider = "openai"
```

but the binary does not include that provider:

```text
provider unavailable
↓
prompt user to select available provider
```

Do not crash.

---

# 54. Unknown Model Config

If configured model no longer exists:

```text
mark unavailable
↓
show model picker
```

Preserve the old value until replacement is selected if useful for diagnostics.

---

# 55. Config Error Types

Conceptual:

```rust
pub enum ConfigError {
    Io(String),
    Parse(String),
    UnsupportedSchema(u32),
    InvalidValue {
        path: String,
        message: String,
    },
    Migration(String),
}
```

---

# 56. Config Service

Recommended central service:

```rust
pub struct ConfigService {
    paths: ConfigPaths,
}
```

Responsibilities:

- load;
- merge;
- validate;
- migrate;
- save.

---

# 57. Config Resolver

Keep resolution explicit:

```rust
pub struct ConfigResolver;
```

Conceptually:

```rust
resolve(
    global,
    project,
    cli,
    defaults
) -> ResolvedConfig
```

This should be heavily unit tested.

---

# 58. Serialization

Recommended:

```text
serde
toml
```

Use typed structs rather than manual TOML value traversal for ordinary config.

Migration code may use generic values when helpful.

---

# 59. Backward Compatibility

Before v1.0, config may evolve, but Gocode should avoid unnecessary breakage.

Schema migrations allow internal iteration without requiring users to delete `.gocode`.

---

# 60. Project Config in Git

The project-local `.gocode` directory may eventually be committed to Git.

Therefore separate shareable content from machine-specific content.

Good shareable examples:

```text
project.toml
instructions.md
```

Machine-specific/session data may need ignore guidance:

```text
sessions/
```

The final Git strategy should be documented separately.

---

# 61. Secrets in Project Config

Never store provider secrets in project config, especially because it may be committed.

---

# 62. Config Generation

When creating files, write minimal content.

Avoid generating hundreds of commented options.

Example initial project config:

```toml
schema_version = 1

[agent]
instructions = "instructions.md"
```

---

# 63. Config UX

TUI settings should map to config fields.

Example:

```text
/model
↓
select model
↓
save default model
```

No user-facing TOML knowledge required.

---

# 64. Save Feedback

Successful config changes may show:

```text
✓ Saved
```

Failures should be explicit.

---

# 65. Failed Save

If Gocode cannot persist a setting:

- keep runtime state if safe;
- tell the user it may not persist;
- do not claim it was saved.

---

# 66. Read-Only Filesystem

If global/project config cannot be written:

Gocode may continue in temporary mode when possible.

Example:

```text
Configuration changes will not be saved in this directory.
```

---

# 67. Config Security

Config files may contain:

- model IDs;
- project names;
- paths;
- UI preferences.

They must not contain:

- API keys;
- bearer tokens;
- passwords.

---

# 68. Debug Output

If displaying resolved config in debug mode:

redact sensitive or environment-derived values.

Example:

```text
credential_source = "environment"
```

not:

```text
credential = "nvapi-..."
```

---

# 69. Unit Tests

Minimum config tests:

```text
global only
project overrides global
CLI overrides project
missing files
invalid TOML
unknown keys
schema migration
newer schema
default values
invalid thinking setting
missing model
atomic save failure
```

---

# 70. Migration Tests

Each migration should have:

```text
old fixture
↓
migration
↓
expected new fixture
```

Also test repeated migration where applicable.

---

# 71. Windows Tests

Validate:

```text
%USERPROFILE% path
Unicode username
Unicode project path
read-only file
CRLF
atomic replacement
Credential Manager integration
```

---

# 72. Definition of Done

The configuration system is ready for v0.1.0 when:

- global config is created automatically;
- project config is created automatically;
- precedence is deterministic;
- TUI changes persist;
- credentials are not stored in TOML;
- NVIDIA environment variable override works;
- invalid configs fail safely;
- schema versioning exists;
- migrations can be added;
- saved model/thinking settings are revalidated;
- config paths work correctly on Windows;
- startup does not require manual config editing.

---

# 73. Reference Resolution Flow

```text
Built-in defaults
↓
Global config
↓
Project config
↓
CLI overrides
↓
Capability validation
↓
ResolvedConfig
```

Credentials flow separately:

```text
Environment
↓
OS credential store
↓
Onboarding
```

---

# 74. Final Rule

Configuration should persist user intent without exposing internal complexity.

> If the TUI can safely choose, validate, or persist a setting for the user, the user should not be forced to edit a configuration file manually.
