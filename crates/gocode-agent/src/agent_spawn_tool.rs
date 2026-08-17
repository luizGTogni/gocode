//! `agent_spawn`: the tool a subagent below the nesting depth limit gets so it can delegate a
//! bounded subtask to a child subagent and wait for its result. Only ever registered by
//! [`crate::subagent_manager::SubagentWorker::tools_for`] — never present for a subagent already
//! at [`crate::MAX_SUBAGENT_DEPTH`], so a child can never itself spawn a grandchild.

use std::{path::PathBuf, sync::Arc, time::Duration};

use gocode_core::{ModelId, PermissionMode, Provider, SubagentMode};
use gocode_tools::{
    Tool, ToolContext, ToolDefinition, ToolError, ToolFuture, ToolName, ToolOutput, ToolRegistry,
    ToolResult, process::ProcessRunner,
};

use crate::{SpawnRequest, SubagentManager};

/// Delegates a bounded subtask to a child subagent and blocks until it finishes, returning its
/// result summary as the tool's output. See the module doc for why this can only ever be
/// registered one level deep.
pub struct AgentSpawnTool {
    pub manager: Arc<SubagentManager>,
    pub parent_session_id: String,
    pub parent_subagent_id: String,
    /// The spawning subagent's own depth; the child is created at `depth + 1`.
    pub depth: usize,
    /// Model id the child runs under — inherited from the parent rather than model-chosen, to
    /// keep provider/credential selection out of the model's hands.
    pub model: String,
    pub provider: Arc<dyn Provider>,
    /// Base tool registry the child gets (never includes `agent_spawn` itself; that is added
    /// fresh by the child's own worker if its depth still allows nesting).
    pub tools: Arc<ToolRegistry>,
    pub project_root: PathBuf,
    /// The parent's own effective permission mode; the child's is clamped to at most this, so
    /// permissions cannot escalate through a chain of spawns.
    pub parent_permission_mode: PermissionMode,
    pub worktree_runner: Arc<dyn ProcessRunner>,
    pub instructions: Option<String>,
}

/// How often the tool polls for the child's completion while blocked waiting on it.
const POLL_INTERVAL: Duration = Duration::from_millis(300);

impl Tool for AgentSpawnTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: ToolName::new("agent_spawn"),
            description: "Delegates a bounded, self-contained subtask to a child subagent and \
                waits for it to finish, returning its result summary. Use this to hand off a \
                piece of work you do not need to do yourself (e.g. investigating a separate area \
                of the codebase while you focus on the main task). This call blocks until the \
                child finishes, fails, or times out — it is not a way to run work in the \
                background. The child cannot itself spawn further subagents."
                .into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "task": {
                        "type": "string",
                        "description": "The child subagent's objective, in your own words."
                    },
                    "mode": {
                        "type": "string",
                        "enum": ["research", "plan", "implement", "review"],
                        "description": "research/plan/review are read-only; implement may write \
                            files, only inside its own worktree (pair it with worktree: true)."
                    },
                    "worktree": {
                        "type": "boolean",
                        "description": "Only valid with mode=implement; creates an isolated \
                            worktree for the child's changes."
                    }
                },
                "required": ["task", "mode"]
            }),
        }
    }

    fn execute(&self, ctx: ToolContext, input: serde_json::Value) -> ToolFuture<'_> {
        Box::pin(async move {
            let task = input
                .get("task")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string)
                .ok_or_else(|| ToolError::InvalidArguments("\"task\" is required".into()))?;
            let mode = input
                .get("mode")
                .and_then(serde_json::Value::as_str)
                .and_then(SubagentMode::parse)
                .ok_or_else(|| {
                    ToolError::InvalidArguments(
                        "\"mode\" must be one of research, plan, implement, review".into(),
                    )
                })?;
            let worktree_requested = input
                .get("worktree")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false);

            let request = SpawnRequest {
                parent_session_id: self.parent_session_id.clone(),
                task,
                mode,
                model: ModelId::new(self.model.clone()),
                worktree_requested,
                parent_permission_mode: self.parent_permission_mode,
                provider: self.provider.clone(),
                tools: self.tools.clone(),
                project_root: self.project_root.clone(),
                worktree_runner: self.worktree_runner.clone(),
                instructions: self.instructions.clone(),
                depth: self.depth + 1,
                parent_subagent_id: Some(self.parent_subagent_id.clone()),
            };

            let id = match self.manager.spawn(request).await {
                Ok(id) => id,
                Err(error) => {
                    return Ok(ToolResult::failed(
                        ctx.call_id,
                        format!("could not spawn child subagent: {error}"),
                    ));
                }
            };

            loop {
                if ctx.cancellation.is_cancelled() {
                    return Ok(ToolResult::cancelled(ctx.call_id));
                }
                if let Some(record) = self.manager.get(&id).await
                    && record.status.is_terminal()
                {
                    let summary = record.result.as_ref().map_or_else(
                        || format!("(no result; status: {})", record.status.label()),
                        |result| result.summary.clone(),
                    );
                    return Ok(ToolResult::success(
                        ctx.call_id,
                        ToolOutput::new(format!(
                            "Child subagent {} finished: {}. {summary}",
                            id.get(..8).unwrap_or(&id),
                            record.status.label(),
                        )),
                    ));
                }
                tokio::time::sleep(POLL_INTERVAL).await;
            }
        })
    }
}
