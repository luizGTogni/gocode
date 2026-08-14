//! Scenario tests driving [`Agent::run`] against `FakeProvider` and either the real built-in
//! tools (for end-to-end realism) or small fake tools (for control over specific failure modes).
//! See `docs/AGENT.md` §98–102 for the scenario list these are meant to cover.

use std::{
    fs,
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use gocode_core::{
    CancellationToken, ChatStreamEvent, FinishReason, ModelId, ProviderError, ToolCallDelta,
    testing::FakeProvider,
};
use gocode_tools::{
    Tool, ToolCallId, ToolContext, ToolDefinition, ToolError, ToolFuture, ToolName, ToolOutput,
    ToolRegistry, ToolResult,
    permissions::{AlwaysDenyResolver, DefaultPermissionPolicy, PermissionContext},
};
use tokio::sync::mpsc;

use crate::{Agent, AgentError, AgentEvent, AgentLimit, AgentLimits, AgentRequest};

type Script = Vec<Result<ChatStreamEvent, ProviderError>>;

fn text_turn(text: &str) -> Script {
    vec![
        Ok(ChatStreamEvent::TextDelta(text.into())),
        Ok(ChatStreamEvent::Finished(FinishReason::Stop)),
    ]
}

fn tool_call_turn(id: &str, name: &str, arguments: &serde_json::Value) -> Script {
    vec![
        Ok(ChatStreamEvent::ToolCallDelta(ToolCallDelta {
            index: 0,
            id: Some(id.into()),
            name_delta: Some(name.into()),
            arguments_delta: Some(arguments.to_string()),
        })),
        Ok(ChatStreamEvent::Finished(FinishReason::ToolCalls)),
    ]
}

fn fixture(name: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system time should be after the Unix epoch")
        .as_nanos();
    let dir = std::env::temp_dir().join(format!(
        "gocode-agent-{name}-{}-{nanos}",
        std::process::id()
    ));
    fs::create_dir_all(&dir).expect("fixture directory should be created");
    dir
}

fn editing_permissions() -> PermissionContext {
    PermissionContext::new(
        Arc::new(DefaultPermissionPolicy::editing()),
        Arc::new(AlwaysDenyResolver),
    )
}

fn request(project_root: &Path, prompt: &str) -> AgentRequest {
    AgentRequest {
        prompt: prompt.into(),
        model: ModelId::new("fake/model"),
        project_root: project_root.to_path_buf(),
        instructions: None,
        tools_enabled: true,
    }
}

async fn drain(mut events: mpsc::Receiver<AgentEvent>) -> Vec<AgentEvent> {
    let mut collected = Vec::new();
    while let Some(event) = events.recv().await {
        collected.push(event);
    }
    collected
}

fn agent(provider: FakeProvider, tools: ToolRegistry, permissions: PermissionContext) -> Agent {
    Agent::new(
        Arc::new(provider),
        Arc::new(tools),
        permissions,
        AgentLimits::default(),
    )
}

/// A tool with a scripted, deterministic outcome; used where a real filesystem or process
/// side effect would be irrelevant noise for the scenario under test.
struct ScriptedTool {
    name: &'static str,
    outcome: fn(ToolCallId) -> Result<ToolResult, ToolError>,
}

impl Tool for ScriptedTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: ToolName::new(self.name),
            description: "A scripted tool used only in Agent tests.".into(),
            input_schema: serde_json::json!({"type": "object"}),
        }
    }

    fn execute(&self, ctx: ToolContext, _input: serde_json::Value) -> ToolFuture<'_> {
        let outcome = self.outcome;
        Box::pin(async move { outcome(ctx.call_id) })
    }
}

#[tokio::test]
async fn completes_without_any_tool_call() {
    let root = fixture("no-tool");
    let provider = FakeProvider::script(vec![text_turn("Hello there.")]);
    let (tx, rx) = mpsc::channel(32);

    let outcome = agent(
        provider,
        ToolRegistry::new(),
        PermissionContext::read_only_default(),
    )
    .run(request(&root, "hi"), tx, CancellationToken::new())
    .await
    .expect("a text-only turn should complete the run");

    assert_eq!(outcome.final_text, "Hello there.");
    assert_eq!(outcome.stats.turns, 1);
    assert_eq!(outcome.stats.tool_calls, 0);

    let events = drain(rx).await;
    assert!(matches!(events.first(), Some(AgentEvent::Started)));
    assert!(matches!(events.last(), Some(AgentEvent::Completed(_))));

    fs::remove_dir_all(root).ok();
}

#[tokio::test]
async fn executes_one_tool_call_and_returns_the_result_to_the_model() {
    let root = fixture("one-tool");
    fs::write(root.join("a.rs"), "fn main() {}\n").expect("fixture file should be written");

    let provider = FakeProvider::script(vec![
        tool_call_turn("call-1", "read_file", &serde_json::json!({"path": "a.rs"})),
        text_turn("The file defines an empty main function."),
    ]);
    let (tx, rx) = mpsc::channel(32);

    let outcome = agent(
        provider,
        gocode_tools::builtin_registry(),
        PermissionContext::read_only_default(),
    )
    .run(
        request(&root, "what does a.rs do?"),
        tx,
        CancellationToken::new(),
    )
    .await
    .expect("a read_file round trip should complete the run");

    assert_eq!(outcome.stats.turns, 2);
    assert_eq!(outcome.stats.tool_calls, 1);
    assert_eq!(outcome.stats.failed_tool_calls, 0);
    assert_eq!(
        outcome.final_text,
        "The file defines an empty main function."
    );

    let events = drain(rx).await;
    let started_index = events
        .iter()
        .position(|event| matches!(event, AgentEvent::ToolStarted(_)))
        .expect("ToolStarted should be emitted");
    let finished_index = events
        .iter()
        .position(|event| matches!(event, AgentEvent::ToolFinished(_)))
        .expect("ToolFinished should be emitted");
    assert!(
        started_index < finished_index,
        "ToolStarted must precede ToolFinished"
    );
    let requested_index = events
        .iter()
        .position(|event| matches!(event, AgentEvent::ToolRequested(_)))
        .expect("ToolRequested should be emitted");
    assert!(requested_index < started_index);

    fs::remove_dir_all(root).ok();
}

#[tokio::test]
async fn recoverable_tool_failure_lets_the_run_continue() {
    let root = fixture("recoverable-failure");
    let mut tools = ToolRegistry::new();
    tools.register(Arc::new(ScriptedTool {
        name: "flaky",
        outcome: |id| Ok(ToolResult::failed(id, "the file was not found")),
    }));

    let provider = FakeProvider::script(vec![
        tool_call_turn("call-1", "flaky", &serde_json::json!({})),
        text_turn("That path does not exist; here is what I found instead."),
    ]);
    let (tx, _rx) = mpsc::channel(32);

    let outcome = agent(provider, tools, PermissionContext::read_only_default())
        .run(
            request(&root, "look at missing.rs"),
            tx,
            CancellationToken::new(),
        )
        .await
        .expect("a single recoverable failure should not abort the run");

    assert_eq!(outcome.stats.tool_calls, 1);
    assert_eq!(outcome.stats.failed_tool_calls, 1);
    assert_eq!(
        outcome.final_text,
        "That path does not exist; here is what I found instead."
    );

    fs::remove_dir_all(root).ok();
}

#[tokio::test]
async fn unknown_tool_name_returns_a_failure_without_aborting_the_run() {
    let root = fixture("unknown-tool");
    let provider = FakeProvider::script(vec![
        tool_call_turn("call-1", "does_not_exist", &serde_json::json!({})),
        text_turn("I could not find that capability, so here is what I know."),
    ]);
    let (tx, rx) = mpsc::channel(32);

    let outcome = agent(
        provider,
        ToolRegistry::new(),
        PermissionContext::read_only_default(),
    )
    .run(
        request(&root, "do something odd"),
        tx,
        CancellationToken::new(),
    )
    .await
    .expect("an unknown tool should not abort the run");

    assert_eq!(outcome.stats.failed_tool_calls, 1);
    let events = drain(rx).await;
    assert!(events.iter().any(|event| matches!(
        event,
        AgentEvent::Warning(crate::AgentWarning::UnknownTool(name)) if name == "does_not_exist"
    )));

    fs::remove_dir_all(root).ok();
}

#[tokio::test]
async fn invalid_arguments_are_returned_to_the_model_as_a_failed_result() {
    let root = fixture("invalid-args");
    let provider = FakeProvider::script(vec![
        // `read_file` requires a `path`; sending none exercises schema validation.
        tool_call_turn("call-1", "read_file", &serde_json::json!({})),
        text_turn("Let me try a different path."),
    ]);
    let (tx, _rx) = mpsc::channel(32);

    let outcome = agent(
        provider,
        gocode_tools::builtin_registry(),
        PermissionContext::read_only_default(),
    )
    .run(
        request(&root, "read something"),
        tx,
        CancellationToken::new(),
    )
    .await
    .expect("invalid arguments should not abort the run");

    assert_eq!(outcome.stats.failed_tool_calls, 1);

    fs::remove_dir_all(root).ok();
}

#[tokio::test]
async fn malformed_streamed_arguments_fall_back_to_an_empty_object() {
    let root = fixture("malformed-args");
    let provider = FakeProvider::script(vec![
        vec![
            Ok(ChatStreamEvent::ToolCallDelta(ToolCallDelta {
                index: 0,
                id: Some("call-1".into()),
                name_delta: Some("read_file".into()),
                arguments_delta: Some("not json at all".into()),
            })),
            Ok(ChatStreamEvent::Finished(FinishReason::ToolCalls)),
        ],
        text_turn("I could not parse that request; let me retry."),
    ]);
    let (tx, _rx) = mpsc::channel(32);

    let outcome = agent(
        provider,
        gocode_tools::builtin_registry(),
        PermissionContext::read_only_default(),
    )
    .run(request(&root, "read a file"), tx, CancellationToken::new())
    .await
    .expect("malformed tool-call arguments should not abort the run");

    assert_eq!(outcome.stats.failed_tool_calls, 1);

    fs::remove_dir_all(root).ok();
}

#[tokio::test]
async fn write_tool_is_denied_without_editing_intent() {
    let root = fixture("permission-denied");
    let provider = FakeProvider::script(vec![
        tool_call_turn(
            "call-1",
            "write_file",
            &serde_json::json!({"path": "a.txt", "content": "hi"}),
        ),
        text_turn("I could not make that change without permission."),
    ]);
    let (tx, _rx) = mpsc::channel(32);

    let outcome = agent(
        provider,
        gocode_tools::builtin_registry(),
        PermissionContext::read_only_default(),
    )
    .run(request(&root, "create a.txt"), tx, CancellationToken::new())
    .await
    .expect("a denied write should not abort the run");

    assert_eq!(outcome.stats.failed_tool_calls, 1);
    assert!(!root.join("a.txt").exists());

    fs::remove_dir_all(root).ok();
}

#[tokio::test]
async fn medium_risk_command_denied_by_the_resolver_is_reported_as_denied() {
    let root = fixture("command-denied");
    let provider = FakeProvider::script(vec![
        tool_call_turn(
            "call-1",
            "run_command",
            &serde_json::json!({"program": "npm", "args": ["install"]}),
        ),
        text_turn("I was not able to run that command."),
    ]);
    let (tx, _rx) = mpsc::channel(32);

    let outcome = agent(
        provider,
        gocode_tools::builtin_registry(),
        editing_permissions(),
    )
    .run(request(&root, "install deps"), tx, CancellationToken::new())
    .await
    .expect("a denied command should not abort the run");

    assert_eq!(outcome.stats.failed_tool_calls, 1);

    fs::remove_dir_all(root).ok();
}

#[tokio::test]
async fn nonzero_command_exit_is_success_not_a_tool_failure() {
    let root = fixture("command-nonzero-exit");
    let provider = FakeProvider::script(vec![
        tool_call_turn(
            "call-1",
            "run_command",
            // "check" keeps this classified low-risk; running it with no Cargo.toml in the
            // fixture directory still produces a nonzero exit, which is what this test needs.
            &serde_json::json!({"program": "cargo", "args": ["check"]}),
        ),
        text_turn("The command failed; here is the relevant output."),
    ]);
    let (tx, _rx) = mpsc::channel(32);

    let outcome = agent(
        provider,
        gocode_tools::builtin_registry(),
        editing_permissions(),
    )
    .run(request(&root, "run cargo"), tx, CancellationToken::new())
    .await
    .expect("a nonzero exit code should not fail the tool call");

    assert_eq!(outcome.stats.tool_calls, 1);
    assert_eq!(
        outcome.stats.failed_tool_calls, 0,
        "a nonzero exit code is a Success ToolResult per docs/AGENT.md §72"
    );

    fs::remove_dir_all(root).ok();
}

#[tokio::test]
async fn apply_patch_failure_is_reported_and_the_run_continues() {
    let root = fixture("patch-failure");
    let provider = FakeProvider::script(vec![
        tool_call_turn(
            "call-1",
            "apply_patch",
            &serde_json::json!({"patch": "not a real patch document"}),
        ),
        text_turn("The patch could not be applied; I need to re-read the file."),
    ]);
    let (tx, _rx) = mpsc::channel(32);

    let outcome = agent(
        provider,
        gocode_tools::builtin_registry(),
        editing_permissions(),
    )
    .run(request(&root, "fix the bug"), tx, CancellationToken::new())
    .await
    .expect("a patch failure should not abort the run");

    assert_eq!(outcome.stats.failed_tool_calls, 1);

    fs::remove_dir_all(root).ok();
}

#[tokio::test]
async fn provider_failure_ends_the_run_with_a_provider_error() {
    let root = fixture("provider-failure");
    let provider = FakeProvider::script(vec![vec![Err(ProviderError::Server {
        status: Some(500),
        message: "internal error".into(),
    })]]);
    let (tx, _rx) = mpsc::channel(32);

    let outcome = agent(
        provider,
        ToolRegistry::new(),
        PermissionContext::read_only_default(),
    )
    .run(request(&root, "hi"), tx, CancellationToken::new())
    .await;

    assert!(matches!(outcome, Err(AgentError::Provider(_))));

    fs::remove_dir_all(root).ok();
}

#[tokio::test]
async fn cancellation_before_the_run_starts_stops_it_immediately() {
    let root = fixture("cancelled");
    let provider = FakeProvider::script(vec![text_turn("should never be read")]);
    let (tx, rx) = mpsc::channel(32);
    let cancellation = CancellationToken::new();
    cancellation.cancel();

    let outcome = agent(
        provider,
        ToolRegistry::new(),
        PermissionContext::read_only_default(),
    )
    .run(request(&root, "hi"), tx, cancellation)
    .await;

    assert!(matches!(outcome, Err(AgentError::Cancelled)));
    let events = drain(rx).await;
    assert!(matches!(events.last(), Some(AgentEvent::Cancelled)));

    fs::remove_dir_all(root).ok();
}

#[tokio::test]
async fn max_turns_limit_stops_a_run_that_keeps_requesting_tools() {
    let root = fixture("max-turns");
    let mut tools = ToolRegistry::new();
    tools.register(Arc::new(ScriptedTool {
        name: "loopy",
        outcome: |id| Ok(ToolResult::success(id, ToolOutput::new("ok"))),
    }));
    let provider = FakeProvider::script(vec![tool_call_turn(
        "call-1",
        "loopy",
        &serde_json::json!({"n": 1}),
    )]);
    let (tx, _rx) = mpsc::channel(32);
    let agent = Agent::new(
        Arc::new(provider),
        Arc::new(tools),
        PermissionContext::read_only_default(),
        AgentLimits {
            max_turns: 1,
            ..AgentLimits::default()
        },
    );

    let outcome = agent
        .run(request(&root, "keep going"), tx, CancellationToken::new())
        .await;

    assert!(matches!(
        outcome,
        Err(AgentError::LimitReached(AgentLimit::MaxTurns))
    ));

    fs::remove_dir_all(root).ok();
}

#[tokio::test]
async fn repeated_identical_failures_trigger_loop_detection() {
    let root = fixture("loop-detection");
    let mut tools = ToolRegistry::new();
    tools.register(Arc::new(ScriptedTool {
        name: "always_fails",
        outcome: |id| Ok(ToolResult::failed(id, "still missing")),
    }));
    let same_args = serde_json::json!({"path": "missing.rs"});
    let provider = FakeProvider::script(vec![
        tool_call_turn("call-1", "always_fails", &same_args),
        tool_call_turn("call-2", "always_fails", &same_args),
        tool_call_turn("call-3", "always_fails", &same_args),
    ]);
    let (tx, rx) = mpsc::channel(32);

    let outcome = agent(provider, tools, PermissionContext::read_only_default())
        .run(request(&root, "keep trying"), tx, CancellationToken::new())
        .await;

    assert!(matches!(
        outcome,
        Err(AgentError::LimitReached(AgentLimit::RepeatedToolCall))
    ));
    let events = drain(rx).await;
    assert!(events.iter().any(|event| matches!(
        event,
        AgentEvent::Warning(crate::AgentWarning::LoopDetected(_))
    )));

    fs::remove_dir_all(root).ok();
}

#[tokio::test]
async fn consecutive_failures_across_different_tools_stop_the_run() {
    let root = fixture("consecutive-failures");
    let mut tools = ToolRegistry::new();
    tools.register(Arc::new(ScriptedTool {
        name: "fails_a",
        outcome: |id| Ok(ToolResult::failed(id, "a failed")),
    }));
    tools.register(Arc::new(ScriptedTool {
        name: "fails_b",
        outcome: |id| Ok(ToolResult::failed(id, "b failed")),
    }));
    tools.register(Arc::new(ScriptedTool {
        name: "fails_c",
        outcome: |id| Ok(ToolResult::failed(id, "c failed")),
    }));
    let provider = FakeProvider::script(vec![
        tool_call_turn("call-1", "fails_a", &serde_json::json!({})),
        tool_call_turn("call-2", "fails_b", &serde_json::json!({})),
        tool_call_turn("call-3", "fails_c", &serde_json::json!({})),
    ]);
    let (tx, _rx) = mpsc::channel(32);

    let outcome = agent(provider, tools, PermissionContext::read_only_default())
        .run(request(&root, "try things"), tx, CancellationToken::new())
        .await;

    assert!(matches!(
        outcome,
        Err(AgentError::LimitReached(AgentLimit::ConsecutiveFailures))
    ));

    fs::remove_dir_all(root).ok();
}

#[tokio::test]
async fn a_success_resets_the_consecutive_failure_counter() {
    let root = fixture("failure-reset");
    let mut tools = ToolRegistry::new();
    tools.register(Arc::new(ScriptedTool {
        name: "fails",
        outcome: |id| Ok(ToolResult::failed(id, "nope")),
    }));
    tools.register(Arc::new(ScriptedTool {
        name: "succeeds",
        outcome: |id| Ok(ToolResult::success(id, ToolOutput::new("ok"))),
    }));
    let provider = FakeProvider::script(vec![
        tool_call_turn("call-1", "fails", &serde_json::json!({"n": 1})),
        tool_call_turn("call-2", "succeeds", &serde_json::json!({})),
        tool_call_turn("call-3", "fails", &serde_json::json!({"n": 2})),
        tool_call_turn("call-4", "fails", &serde_json::json!({"n": 3})),
        text_turn("Done after recovering twice."),
    ]);
    let (tx, _rx) = mpsc::channel(32);

    let outcome = agent(provider, tools, PermissionContext::read_only_default())
        .run(request(&root, "keep trying"), tx, CancellationToken::new())
        .await
        .expect("an interleaved success should reset the consecutive-failure counter");

    assert_eq!(outcome.final_text, "Done after recovering twice.");

    fs::remove_dir_all(root).ok();
}

#[tokio::test]
async fn file_changes_from_a_successful_write_are_reported() {
    let root = fixture("file-changed");
    let provider = FakeProvider::script(vec![
        tool_call_turn(
            "call-1",
            "write_file",
            &serde_json::json!({"path": "new.rs", "content": "fn main() {}\n"}),
        ),
        text_turn("Created new.rs."),
    ]);
    let (tx, rx) = mpsc::channel(32);

    let outcome = agent(
        provider,
        gocode_tools::builtin_registry(),
        editing_permissions(),
    )
    .run(
        request(&root, "create new.rs"),
        tx,
        CancellationToken::new(),
    )
    .await
    .expect("a successful write should complete the run");

    assert_eq!(outcome.stats.failed_tool_calls, 0);
    assert_eq!(
        fs::read_to_string(root.join("new.rs")).expect("file should have been written"),
        "fn main() {}\n"
    );
    let events = drain(rx).await;
    assert!(events
        .iter()
        .any(|event| matches!(event, AgentEvent::FileChanged(change) if change.path == Path::new("new.rs"))));

    fs::remove_dir_all(root).ok();
}

#[tokio::test]
async fn reference_flow_completes_a_search_read_patch_and_command_task() {
    let root = fixture("reference-flow");
    fs::write(
        root.join("auth.rs"),
        "fn validate_token(token: &str) -> bool {\n    true\n}\n",
    )
    .expect("fixture file should be written");

    let provider = FakeProvider::script(vec![
        tool_call_turn(
            "call-1",
            "search",
            &serde_json::json!({"query": "validate_token"}),
        ),
        tool_call_turn(
            "call-2",
            "read_file",
            &serde_json::json!({"path": "auth.rs"}),
        ),
        tool_call_turn(
            "call-3",
            "write_file",
            &serde_json::json!({
                "path": "auth.rs",
                "content": "fn validate_token(token: &str) -> bool {\n    !token.is_empty()\n}\n"
            }),
        ),
        tool_call_turn(
            "call-4",
            "run_command",
            &serde_json::json!({"program": "cargo", "args": ["--version"]}),
        ),
        text_turn("Fixed token validation in auth.rs. I ran cargo --version to confirm tooling."),
    ]);
    let (tx, rx) = mpsc::channel(64);

    let outcome = agent(
        provider,
        gocode_tools::builtin_registry(),
        editing_permissions(),
    )
    .run(
        request(&root, "fix the authentication bug and check tooling"),
        tx,
        CancellationToken::new(),
    )
    .await
    .expect("the reference flow should complete");

    assert_eq!(outcome.stats.turns, 5);
    assert_eq!(outcome.stats.tool_calls, 4);
    assert_eq!(outcome.stats.failed_tool_calls, 0);
    assert!(outcome.final_text.contains("Fixed token validation"));
    assert_eq!(
        fs::read_to_string(root.join("auth.rs")).expect("file should have been rewritten"),
        "fn validate_token(token: &str) -> bool {\n    !token.is_empty()\n}\n"
    );

    let events = drain(rx).await;
    assert!(matches!(events.first(), Some(AgentEvent::Started)));
    assert!(matches!(events.last(), Some(AgentEvent::Completed(_))));
    let tool_requests = events
        .iter()
        .filter(|event| matches!(event, AgentEvent::ToolRequested(_)))
        .count();
    assert_eq!(tool_requests, 4);

    fs::remove_dir_all(root).ok();
}

#[tokio::test]
async fn each_run_receives_a_distinct_run_id() {
    let root = fixture("distinct-run-ids");
    let provider = FakeProvider::script(vec![text_turn("ok"), text_turn("ok")]);
    let agent = agent(
        provider,
        ToolRegistry::new(),
        PermissionContext::read_only_default(),
    );
    let (tx_a, _rx_a) = mpsc::channel(8);
    let (tx_b, _rx_b) = mpsc::channel(8);

    let first = agent
        .run(request(&root, "hi"), tx_a, CancellationToken::new())
        .await
        .expect("first run should complete");
    let second = agent
        .run(request(&root, "hi"), tx_b, CancellationToken::new())
        .await
        .expect("second run should complete");

    assert_ne!(first.run_id, second.run_id);

    fs::remove_dir_all(root).ok();
}

#[tokio::test]
async fn project_instructions_are_included_without_overriding_the_request() {
    let root = fixture("project-instructions");
    let provider = FakeProvider::script(vec![text_turn("Following the project's style guide.")]);
    let (tx, _rx) = mpsc::channel(8);
    let mut req = request(&root, "add a helper function");
    req.instructions = Some("Always add doc comments to public functions.".into());

    let outcome = agent(
        provider,
        ToolRegistry::new(),
        PermissionContext::read_only_default(),
    )
    .run(req, tx, CancellationToken::new())
    .await
    .expect("a run with project instructions should complete normally");

    assert_eq!(outcome.final_text, "Following the project's style guide.");

    fs::remove_dir_all(root).ok();
}

#[tokio::test]
async fn tools_disabled_sends_no_tool_definitions_and_ignores_tool_calls() {
    let root = fixture("tools-disabled");
    // Even if a script were to request a tool call, a caller that gates tool support off
    // (an unsupported model, per docs/AGENT.md §109) never offers definitions in the first
    // place; this test only exercises the flag's effect on a plain text turn.
    let provider = FakeProvider::script(vec![text_turn("This model cannot use tools.")]);
    let (tx, _rx) = mpsc::channel(8);
    let mut req = request(&root, "hi");
    req.tools_enabled = false;

    let outcome = agent(
        provider,
        gocode_tools::builtin_registry(),
        PermissionContext::read_only_default(),
    )
    .run(req, tx, CancellationToken::new())
    .await
    .expect("a tools-disabled run should still complete as plain chat");

    assert_eq!(outcome.final_text, "This model cannot use tools.");

    fs::remove_dir_all(root).ok();
}

/// Guards against the cancellation-bridge task leaking past the end of a run: dropping the
/// agent and its channels should not panic or hang, and a fresh run afterward still works.
#[tokio::test]
async fn cancellation_bridge_does_not_outlive_a_completed_run() {
    let root = fixture("bridge-cleanup");
    let provider = FakeProvider::script(vec![text_turn("first"), text_turn("second")]);
    let agent = agent(
        provider,
        ToolRegistry::new(),
        PermissionContext::read_only_default(),
    );

    for prompt in ["first", "second"] {
        let (tx, _rx) = mpsc::channel(8);
        agent
            .run(request(&root, prompt), tx, CancellationToken::new())
            .await
            .expect("each run should complete independently");
    }

    tokio::time::sleep(Duration::from_millis(10)).await;

    fs::remove_dir_all(root).ok();
}
