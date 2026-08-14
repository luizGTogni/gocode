//! Registry that resolves a model-requested tool name to its implementation.

use std::{collections::HashMap, sync::Arc};

use crate::contract::{Tool, ToolDefinition, ToolName};

/// Stores available tools and resolves them by name.
#[derive(Clone, Default)]
pub struct ToolRegistry {
    tools: HashMap<ToolName, Arc<dyn Tool>>,
}

impl ToolRegistry {
    /// Creates an empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers `tool`, replacing any prior registration under the same name.
    pub fn register(&mut self, tool: Arc<dyn Tool>) {
        let name = tool.definition().name;
        self.tools.insert(name, tool);
    }

    /// Looks up a tool by name.
    #[must_use]
    pub fn get(&self, name: &ToolName) -> Option<Arc<dyn Tool>> {
        self.tools.get(name).cloned()
    }

    /// Reports whether `name` is registered.
    #[must_use]
    pub fn contains(&self, name: &ToolName) -> bool {
        self.tools.contains_key(name)
    }

    /// Exports every registered tool's definition, for delivery to a capable model.
    #[must_use]
    pub fn definitions(&self) -> Vec<ToolDefinition> {
        let mut definitions: Vec<_> = self.tools.values().map(|tool| tool.definition()).collect();
        definitions.sort_by(|a, b| a.name.as_str().cmp(b.name.as_str()));
        definitions
    }

    /// Number of registered tools.
    #[must_use]
    pub fn len(&self) -> usize {
        self.tools.len()
    }

    /// Whether the registry has no registered tools.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.tools.is_empty()
    }
}

/// Builds a registry containing every MVP built-in tool.
#[must_use]
pub fn builtin_registry() -> ToolRegistry {
    let mut registry = ToolRegistry::new();
    registry.register(Arc::new(crate::tools::list_files::ListFilesTool));
    registry.register(Arc::new(crate::tools::read_file::ReadFileTool));
    registry.register(Arc::new(crate::tools::search::SearchTool));
    registry.register(Arc::new(crate::tools::write_file::WriteFileTool));
    registry.register(Arc::new(crate::tools::apply_patch::ApplyPatchTool));
    registry.register(Arc::new(
        crate::tools::run_command::RunCommandTool::default(),
    ));
    registry.register(Arc::new(crate::tools::git::GitStatusTool));
    registry.register(Arc::new(crate::tools::git::GitDiffTool));
    registry
}

#[cfg(test)]
mod tests {
    use super::{ToolRegistry, builtin_registry};
    use crate::contract::{
        Tool, ToolContext, ToolDefinition, ToolError, ToolFuture, ToolName, ToolResult,
    };
    use std::sync::Arc;

    struct FakeTool;

    impl Tool for FakeTool {
        fn definition(&self) -> ToolDefinition {
            ToolDefinition {
                name: ToolName::new("fake_tool"),
                description: "A fake tool used only in tests.".into(),
                input_schema: serde_json::json!({"type": "object"}),
            }
        }

        fn execute(&self, _ctx: ToolContext, _input: serde_json::Value) -> ToolFuture<'_> {
            Box::pin(async move {
                Err::<ToolResult, ToolError>(ToolError::Internal("unused in this test".into()))
            })
        }
    }

    #[test]
    fn registered_tool_resolves_by_name() {
        let mut registry = ToolRegistry::new();
        registry.register(Arc::new(FakeTool));

        assert!(registry.contains(&ToolName::new("fake_tool")));
        assert!(registry.get(&ToolName::new("fake_tool")).is_some());
    }

    #[test]
    fn unavailable_tool_is_not_found() {
        let registry = ToolRegistry::new();
        assert!(registry.get(&ToolName::new("does_not_exist")).is_none());
    }

    #[test]
    fn builtin_registry_exposes_every_mvp_tool() {
        let registry = builtin_registry();
        let names: Vec<_> = registry
            .definitions()
            .into_iter()
            .map(|definition| definition.name.as_str().to_string())
            .collect();

        for expected in [
            "list_files",
            "read_file",
            "search",
            "write_file",
            "apply_patch",
            "run_command",
            "git_status",
            "git_diff",
        ] {
            assert!(names.contains(&expected.to_string()), "missing {expected}");
        }
        assert_eq!(registry.len(), 8);
    }
}
