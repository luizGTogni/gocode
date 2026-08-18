use std::{
    collections::BTreeSet,
    future::Future,
    path::{Path, PathBuf},
    pin::Pin,
    sync::Arc,
};

/// Risk classification the permission engine assigns to a `run_command` request.
///
/// This is an illustrative heuristic, not a hardcoded allowlist: unknown programs default to
/// [`CommandRisk::Medium`] so they are neither silently trusted nor silently blocked.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum CommandRisk {
    /// Read-only or well-understood build/test/lint commands.
    Low,
    /// Commands whose effects are plausible but not clearly safe.
    Medium,
    /// Commands that delete data, touch remote state, or affect the system outside the project.
    High,
}

/// A reusable approval category. Command approvals intentionally use risk level rather than the
/// exact command text, so "always allow medium commands" does not silently authorize high-risk
/// commands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum PermissionScope {
    Read,
    Write,
    Command(CommandRisk),
}

impl PermissionScope {
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Read => "leitura",
            Self::Write => "alterações de arquivo",
            Self::Command(CommandRisk::Low) => "comandos de baixo risco",
            Self::Command(CommandRisk::Medium) => "comandos de risco médio",
            Self::Command(CommandRisk::High) => "comandos de alto risco",
        }
    }
}

/// The action a tool is requesting permission for.
#[derive(Debug, Clone)]
pub enum PermissionAction {
    /// A read-only filesystem or Git inspection.
    ReadOnly,
    /// A file create, modify, or delete inside the workspace.
    Write { path: PathBuf },
    /// An external process invocation.
    Command {
        program: String,
        args: Vec<String>,
        cwd: PathBuf,
        risk: CommandRisk,
    },
}

/// Reason a request was denied, shown to the model so it can adapt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PermissionReason(pub String);

/// A concrete confirmation prompt, ready for a permission-modal integration point.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PermissionRequest {
    /// Short summary of the action, e.g. `run: npm install`.
    pub summary: String,
    /// Working directory the action would run or write in.
    pub working_directory: PathBuf,
    /// Category used when the user selects “Allow always”.
    pub scope: PermissionScope,
}

/// The user's outcome for an interactive permission prompt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PermissionResponse {
    AllowOnce,
    AllowAlways,
    Deny,
}

/// The permission engine's decision for one action.
#[derive(Debug, Clone)]
pub enum PermissionDecision {
    /// Proceed without user confirmation.
    Allow,
    /// Proceed only if the user approves this specific request.
    Ask(PermissionRequest),
    /// Never proceed; carries a model-facing explanation.
    Deny(PermissionReason),
}

const HIGH_RISK_PROGRAMS: &[&str] = &[
    "rm", "del", "erase", "rmdir", "format", "shutdown", "reboot", "diskpart", "sudo", "su",
    "chmod", "chown", "curl", "wget", "ssh", "scp", "git",
];
const HIGH_RISK_PATTERNS: &[&str] = &[
    "rm -rf",
    "rm -r",
    "-force",
    "--force",
    "reset --hard",
    "push",
    "checkout",
    "clean -f",
    "> /dev",
    "format ",
];
const LOW_RISK_PROGRAMS: &[&str] = &[
    "cargo",
    "npm",
    "pnpm",
    "yarn",
    "go",
    "pytest",
    "python",
    "python3",
    "node",
    "make",
    "git-status",
];
const LOW_RISK_ARG_HINTS: &[&str] = &[
    "test",
    "check",
    "build",
    "fmt",
    "lint",
    "vet",
    "status",
    "diff",
    "--version",
    "-v",
];

/// Classifies a `run_command` request using conservative, illustrative heuristics.
///
/// Callers should treat this as a starting point: it recognizes common low-risk development
/// commands and common high-risk destructive patterns, and defaults everything else to
/// [`CommandRisk::Medium`].
#[must_use]
pub fn classify_command_risk(program: &str, args: &[String], shell: bool) -> CommandRisk {
    let program_lower = program.to_ascii_lowercase();
    let joined_args = args.join(" ").to_ascii_lowercase();

    if shell {
        return CommandRisk::High;
    }

    if program_lower == "git" {
        return if joined_args.starts_with("status")
            || joined_args.starts_with("diff")
            || joined_args.starts_with("log")
            || joined_args.starts_with("show")
        {
            CommandRisk::Low
        } else {
            CommandRisk::High
        };
    }

    if HIGH_RISK_PROGRAMS
        .iter()
        .any(|candidate| program_lower == *candidate)
        || HIGH_RISK_PATTERNS
            .iter()
            .any(|pattern| joined_args.contains(pattern))
    {
        return CommandRisk::High;
    }

    if LOW_RISK_PROGRAMS
        .iter()
        .any(|candidate| program_lower == *candidate)
        && (args.is_empty()
            || LOW_RISK_ARG_HINTS
                .iter()
                .any(|hint| joined_args.contains(hint)))
    {
        return CommandRisk::Low;
    }

    CommandRisk::Medium
}

/// Future returned by [`PermissionResolver::resolve`].
pub type ResolveFuture<'a> = Pin<Box<dyn Future<Output = PermissionResponse> + Send + 'a>>;

/// Resolves an interactive `Ask` decision to a boolean outcome.
///
/// The concrete implementation is the permission-modal integration point: a TUI-backed resolver
/// prompts the user, while tests use a scripted resolver.
pub trait PermissionResolver: Send + Sync {
    /// Returns the user's selection for `request`.
    fn resolve<'a>(&'a self, request: &'a PermissionRequest) -> ResolveFuture<'a>;
}

/// A [`PermissionResolver`] that denies every `Ask` decision.
///
/// This is the safe default when no interactive surface is attached: the tool layer never
/// silently performs an action the policy flagged as needing confirmation.
#[derive(Debug, Clone, Copy, Default)]
pub struct AlwaysDenyResolver;

impl PermissionResolver for AlwaysDenyResolver {
    fn resolve<'a>(&'a self, _request: &'a PermissionRequest) -> ResolveFuture<'a> {
        Box::pin(async { PermissionResponse::Deny })
    }
}

/// Decides `Allow` / `Ask` / `Deny` for one action, independent of how `Ask` gets resolved.
pub trait PermissionPolicy: Send + Sync {
    /// Evaluates the requested action.
    fn evaluate(&self, action: &PermissionAction) -> PermissionDecision;
}

/// Evaluates a command request by risk, shared by every policy in this module: low is allowed,
/// medium asks for confirmation, high is denied outright (the MVP exposes no dedicated commit,
/// push, reset, checkout, or generic network tool, so high-risk commands stay behind an explicit
/// deny rather than a confirmable prompt).
fn evaluate_command_risk(
    program: &str,
    args: &[String],
    cwd: &Path,
    risk: CommandRisk,
) -> PermissionDecision {
    match risk {
        CommandRisk::Low => PermissionDecision::Allow,
        CommandRisk::Medium => PermissionDecision::Ask(PermissionRequest {
            summary: format!("run: {program} {}", args.join(" "))
                .trim()
                .to_string(),
            working_directory: cwd.to_path_buf(),
            scope: PermissionScope::Command(risk),
        }),
        CommandRisk::High => PermissionDecision::Deny(PermissionReason(format!(
            "{program} is classified high-risk and requires an explicit, narrower tool"
        ))),
    }
}

fn request_for(action: &PermissionAction) -> PermissionRequest {
    match action {
        PermissionAction::ReadOnly => PermissionRequest {
            summary: "read project files".into(),
            working_directory: PathBuf::from("."),
            scope: PermissionScope::Read,
        },
        PermissionAction::Write { path } => PermissionRequest {
            summary: format!("write: {}", path.display()),
            working_directory: path
                .parent()
                .map_or_else(|| path.clone(), Path::to_path_buf),
            scope: PermissionScope::Write,
        },
        PermissionAction::Command {
            program,
            args,
            cwd,
            risk,
        } => PermissionRequest {
            summary: format!("run: {program} {}", args.join(" "))
                .trim()
                .to_string(),
            working_directory: cwd.clone(),
            scope: PermissionScope::Command(*risk),
        },
    }
}

/// Extensions treated as project documentation or planning notes rather than source code, for
/// [`PlanPermissionPolicy`]. Illustrative, not exhaustive: an unrecognized extension is treated
/// as code and denied, erring toward protecting source over allowing writes.
const PLAN_WRITABLE_EXTENSIONS: &[&str] = &[
    "md", "markdown", "txt", "text", "rst", "adoc", "json", "yaml", "yml", "toml", "csv", "log",
];

fn is_plan_writable(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| {
            PLAN_WRITABLE_EXTENSIONS
                .iter()
                .any(|allowed| ext.eq_ignore_ascii_case(allowed))
        })
}

/// The MVP default policy described in `docs/TOOLS.md` §25.
///
/// Read-only tools are always allowed. Writes are allowed while editing is enabled for the
/// current task. Commands are evaluated by risk (see [`evaluate_command_risk`]).
#[derive(Debug, Clone, Copy)]
pub struct DefaultPermissionPolicy {
    /// Whether the current task authorizes file-editing tools.
    pub editing_enabled: bool,
}

impl DefaultPermissionPolicy {
    /// Creates the read-only variant of the default policy.
    #[must_use]
    pub const fn read_only() -> Self {
        Self {
            editing_enabled: false,
        }
    }

    /// Creates the editing-enabled variant of the default policy.
    #[must_use]
    pub const fn editing() -> Self {
        Self {
            editing_enabled: true,
        }
    }
}

impl PermissionPolicy for DefaultPermissionPolicy {
    fn evaluate(&self, action: &PermissionAction) -> PermissionDecision {
        match action {
            PermissionAction::ReadOnly => PermissionDecision::Allow,
            PermissionAction::Write { path } => {
                if self.editing_enabled {
                    PermissionDecision::Allow
                } else {
                    PermissionDecision::Deny(PermissionReason(format!(
                        "editing is not authorized for this task; cannot write {}",
                        path.display()
                    )))
                }
            }
            PermissionAction::Command { .. } => PermissionDecision::Allow,
        }
    }
}

/// Approval mode: reads, edits, and low-risk commands proceed; medium and high-risk commands
/// pause unless their risk category has already been allowed for this session.
#[derive(Clone)]
pub struct ApprovePermissionPolicy {
    always_allowed: Arc<std::sync::Mutex<BTreeSet<PermissionScope>>>,
}

impl ApprovePermissionPolicy {
    #[must_use]
    pub fn new(always_allowed: Arc<std::sync::Mutex<BTreeSet<PermissionScope>>>) -> Self {
        Self { always_allowed }
    }
}

impl PermissionPolicy for ApprovePermissionPolicy {
    fn evaluate(&self, action: &PermissionAction) -> PermissionDecision {
        let request = request_for(action);
        if self
            .always_allowed
            .lock()
            .is_ok_and(|allowed| allowed.contains(&request.scope))
        {
            return PermissionDecision::Allow;
        }
        match action {
            PermissionAction::Command {
                risk: CommandRisk::Medium | CommandRisk::High,
                ..
            } => PermissionDecision::Ask(request),
            _ => PermissionDecision::Allow,
        }
    }
}

/// Manual mode: every tool action stops for explicit approval unless its category was allowed
/// for the current session.
#[derive(Clone)]
pub struct ManualPermissionPolicy {
    always_allowed: Arc<std::sync::Mutex<BTreeSet<PermissionScope>>>,
}

impl ManualPermissionPolicy {
    #[must_use]
    pub fn new(always_allowed: Arc<std::sync::Mutex<BTreeSet<PermissionScope>>>) -> Self {
        Self { always_allowed }
    }
}

impl PermissionPolicy for ManualPermissionPolicy {
    fn evaluate(&self, action: &PermissionAction) -> PermissionDecision {
        let request = request_for(action);
        if self
            .always_allowed
            .lock()
            .is_ok_and(|allowed| allowed.contains(&request.scope))
        {
            PermissionDecision::Allow
        } else {
            PermissionDecision::Ask(request)
        }
    }
}

/// Plan-mode policy: gathers information and drafts documentation, but cannot touch source code.
///
/// Reads and commands are evaluated exactly as in [`DefaultPermissionPolicy`] — planning still
/// needs to run `cargo check`, grep, or inspect `git status`. Writes are allowed only to files
/// that look like documentation or planning notes (see [`is_plan_writable`]); creating, editing,
/// or deleting a source file is denied outright rather than merely asked about, since the whole
/// point of this mode is that code stays untouched until the plan is approved.
#[derive(Debug, Clone, Copy, Default)]
pub struct PlanPermissionPolicy;

impl PermissionPolicy for PlanPermissionPolicy {
    fn evaluate(&self, action: &PermissionAction) -> PermissionDecision {
        match action {
            PermissionAction::ReadOnly => PermissionDecision::Allow,
            PermissionAction::Write { path } => {
                if is_plan_writable(path) {
                    PermissionDecision::Allow
                } else {
                    PermissionDecision::Deny(PermissionReason(format!(
                        "plan mode only allows writing documentation or notes files; cannot \
                         write {}",
                        path.display()
                    )))
                }
            }
            PermissionAction::Command {
                program,
                args,
                cwd,
                risk,
            } => evaluate_command_risk(program, args, cwd, *risk),
        }
    }
}

/// Legacy policy retained for callers that need every non-read action confirmed.
#[derive(Debug, Clone, Copy, Default)]
pub struct ApproveEverythingPolicy;

impl PermissionPolicy for ApproveEverythingPolicy {
    fn evaluate(&self, action: &PermissionAction) -> PermissionDecision {
        match action {
            PermissionAction::ReadOnly => PermissionDecision::Allow,
            PermissionAction::Write { .. } => PermissionDecision::Ask(request_for(action)),
            PermissionAction::Command { .. } => PermissionDecision::Ask(request_for(action)),
        }
    }
}

/// Permission policy and interactive resolver bundled for one [`crate::ToolContext`].
#[derive(Clone)]
pub struct PermissionContext {
    /// Decides `Allow` / `Ask` / `Deny` for a requested action.
    pub policy: Arc<dyn PermissionPolicy>,
    /// Resolves `Ask` decisions to a user response.
    pub resolver: Arc<dyn PermissionResolver>,
}

impl PermissionContext {
    /// Creates a permission context from an explicit policy and resolver.
    #[must_use]
    pub fn new(policy: Arc<dyn PermissionPolicy>, resolver: Arc<dyn PermissionResolver>) -> Self {
        Self { policy, resolver }
    }

    /// Creates the safe default: MVP read-only policy with every `Ask` denied.
    #[must_use]
    pub fn read_only_default() -> Self {
        Self::new(
            Arc::new(DefaultPermissionPolicy::read_only()),
            Arc::new(AlwaysDenyResolver),
        )
    }

    /// Evaluates `action` and resolves any `Ask` decision, returning `Ok(())` when the caller may
    /// proceed and `Err` with a model-facing reason otherwise.
    ///
    /// # Errors
    ///
    /// Returns [`PermissionReason`] when the policy denies the action outright, or when an `Ask`
    /// decision is resolved to a denial.
    pub async fn authorize(
        &self,
        action: PermissionAction,
    ) -> Result<Option<PermissionRequest>, PermissionReason> {
        match self.policy.evaluate(&action) {
            PermissionDecision::Allow => Ok(None),
            PermissionDecision::Deny(reason) => Err(reason),
            PermissionDecision::Ask(request) => match self.resolver.resolve(&request).await {
                PermissionResponse::AllowOnce | PermissionResponse::AllowAlways => {
                    Ok(Some(request))
                }
                PermissionResponse::Deny => {
                    Err(PermissionReason("the user denied this action".into()))
                }
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        AlwaysDenyResolver, ApproveEverythingPolicy, ApprovePermissionPolicy, CommandRisk,
        DefaultPermissionPolicy, ManualPermissionPolicy, PermissionAction, PermissionContext,
        PermissionDecision, PermissionPolicy, PermissionRequest, PermissionResolver,
        PermissionResponse, PermissionScope, PlanPermissionPolicy, ResolveFuture,
        classify_command_risk,
    };
    use std::{path::PathBuf, sync::Arc};

    struct ScriptedResolver(bool);

    impl PermissionResolver for ScriptedResolver {
        fn resolve<'a>(&'a self, _request: &'a PermissionRequest) -> ResolveFuture<'a> {
            let approved = self.0;
            Box::pin(async move {
                if approved {
                    PermissionResponse::AllowOnce
                } else {
                    PermissionResponse::Deny
                }
            })
        }
    }

    #[test]
    fn known_low_risk_development_commands_are_classified_low() {
        assert_eq!(
            classify_command_risk("cargo", &["test".into()], false),
            CommandRisk::Low
        );
        assert_eq!(
            classify_command_risk("git", &["status".into()], false),
            CommandRisk::Low
        );
    }

    #[test]
    fn destructive_patterns_are_classified_high_regardless_of_program() {
        assert_eq!(
            classify_command_risk("git", &["push".into()], false),
            CommandRisk::High
        );
        assert_eq!(
            classify_command_risk("rm", &["-rf".into(), "target".into()], false),
            CommandRisk::High
        );
    }

    #[test]
    fn shell_mode_is_always_high_risk() {
        assert_eq!(
            classify_command_risk("bash", &["-c".into(), "echo hi".into()], true),
            CommandRisk::High
        );
    }

    #[test]
    fn unknown_commands_default_to_medium_risk() {
        assert_eq!(
            classify_command_risk("some-tool", &[], false),
            CommandRisk::Medium
        );
    }

    #[test]
    fn read_only_actions_are_always_allowed() {
        let policy = DefaultPermissionPolicy::read_only();
        assert!(matches!(
            policy.evaluate(&PermissionAction::ReadOnly),
            PermissionDecision::Allow
        ));
    }

    #[test]
    fn writes_are_denied_without_editing_intent() {
        let policy = DefaultPermissionPolicy::read_only();
        let decision = policy.evaluate(&PermissionAction::Write {
            path: PathBuf::from("src/lib.rs"),
        });
        assert!(matches!(decision, PermissionDecision::Deny(_)));
    }

    #[test]
    fn writes_are_allowed_with_editing_intent() {
        let policy = DefaultPermissionPolicy::editing();
        let decision = policy.evaluate(&PermissionAction::Write {
            path: PathBuf::from("src/lib.rs"),
        });
        assert!(matches!(decision, PermissionDecision::Allow));
    }

    #[test]
    fn plan_mode_allows_writing_documentation_but_denies_source_code() {
        let policy = PlanPermissionPolicy;

        let doc_decision = policy.evaluate(&PermissionAction::Write {
            path: PathBuf::from("PLAN.md"),
        });
        assert!(matches!(doc_decision, PermissionDecision::Allow));

        let code_decision = policy.evaluate(&PermissionAction::Write {
            path: PathBuf::from("src/lib.rs"),
        });
        assert!(matches!(code_decision, PermissionDecision::Deny(_)));
    }

    #[test]
    fn plan_mode_still_allows_low_risk_commands_for_gathering_context() {
        let policy = PlanPermissionPolicy;
        let decision = policy.evaluate(&PermissionAction::Command {
            program: "cargo".into(),
            args: vec!["check".into()],
            cwd: PathBuf::from("."),
            risk: CommandRisk::Low,
        });
        assert!(matches!(decision, PermissionDecision::Allow));
    }

    #[test]
    fn approve_mode_asks_before_every_write_and_low_risk_command() {
        let policy = ApproveEverythingPolicy;

        let write_decision = policy.evaluate(&PermissionAction::Write {
            path: PathBuf::from("src/lib.rs"),
        });
        assert!(matches!(write_decision, PermissionDecision::Ask(_)));

        let command_decision = policy.evaluate(&PermissionAction::Command {
            program: "cargo".into(),
            args: vec!["check".into()],
            cwd: PathBuf::from("."),
            risk: CommandRisk::Low,
        });
        assert!(matches!(command_decision, PermissionDecision::Ask(_)));
    }

    #[test]
    fn auto_mode_allows_high_risk_commands_without_an_interruption() {
        let policy = DefaultPermissionPolicy::editing();
        let decision = policy.evaluate(&PermissionAction::Command {
            program: "git".into(),
            args: vec!["push".into()],
            cwd: PathBuf::from("."),
            risk: CommandRisk::High,
        });
        assert!(matches!(decision, PermissionDecision::Allow));
    }

    #[test]
    fn auto_mode_allows_medium_commands_without_a_prompt() {
        let policy = DefaultPermissionPolicy::editing();
        let decision = policy.evaluate(&PermissionAction::Command {
            program: "npm".into(),
            args: vec!["install".into()],
            cwd: PathBuf::from(r"C:\dev\my-project"),
            risk: CommandRisk::Medium,
        });
        assert!(matches!(decision, PermissionDecision::Allow));
    }

    #[test]
    fn approve_mode_asks_for_medium_and_high_commands_only() {
        let allowed = Arc::new(std::sync::Mutex::new(std::collections::BTreeSet::new()));
        let policy = ApprovePermissionPolicy::new(allowed);
        for risk in [CommandRisk::Medium, CommandRisk::High] {
            let decision = policy.evaluate(&PermissionAction::Command {
                program: "npm".into(),
                args: vec!["install".into()],
                cwd: PathBuf::from("."),
                risk,
            });
            assert!(matches!(decision, PermissionDecision::Ask(_)));
        }
        assert!(matches!(
            policy.evaluate(&PermissionAction::Command {
                program: "cargo".into(),
                args: vec!["test".into()],
                cwd: PathBuf::from("."),
                risk: CommandRisk::Low,
            }),
            PermissionDecision::Allow
        ));
    }

    #[test]
    fn allow_always_skips_future_prompts_for_the_same_command_risk() {
        let allowed = Arc::new(std::sync::Mutex::new(
            [PermissionScope::Command(CommandRisk::Medium)]
                .into_iter()
                .collect(),
        ));
        let policy = ApprovePermissionPolicy::new(allowed);
        assert!(matches!(
            policy.evaluate(&PermissionAction::Command {
                program: "npm".into(),
                args: vec!["install".into()],
                cwd: PathBuf::from("."),
                risk: CommandRisk::Medium,
            }),
            PermissionDecision::Allow
        ));
    }

    #[test]
    fn manual_mode_asks_before_a_read() {
        let policy = ManualPermissionPolicy::new(Arc::new(std::sync::Mutex::new(
            std::collections::BTreeSet::new(),
        )));
        assert!(matches!(
            policy.evaluate(&PermissionAction::ReadOnly),
            PermissionDecision::Ask(_)
        ));
    }

    #[tokio::test]
    async fn authorize_denies_ask_decisions_by_default() {
        let ctx = PermissionContext::new(
            Arc::new(ManualPermissionPolicy::new(Arc::new(
                std::sync::Mutex::new(std::collections::BTreeSet::new()),
            ))),
            Arc::new(AlwaysDenyResolver),
        );
        let outcome = ctx
            .authorize(PermissionAction::Command {
                program: "npm".into(),
                args: vec!["install".into()],
                cwd: PathBuf::from("."),
                risk: CommandRisk::Medium,
            })
            .await;

        assert!(outcome.is_err());
    }

    #[tokio::test]
    async fn authorize_allows_ask_decisions_the_resolver_approves() {
        let ctx = PermissionContext::new(
            Arc::new(ManualPermissionPolicy::new(Arc::new(
                std::sync::Mutex::new(std::collections::BTreeSet::new()),
            ))),
            Arc::new(ScriptedResolver(true)),
        );
        let outcome = ctx
            .authorize(PermissionAction::Command {
                program: "npm".into(),
                args: vec!["install".into()],
                cwd: PathBuf::from("."),
                risk: CommandRisk::Medium,
            })
            .await;

        assert!(outcome.is_ok());
    }

    #[tokio::test]
    async fn authorize_denies_ask_decisions_the_resolver_rejects() {
        let ctx = PermissionContext::new(
            Arc::new(ManualPermissionPolicy::new(Arc::new(
                std::sync::Mutex::new(std::collections::BTreeSet::new()),
            ))),
            Arc::new(AlwaysDenyResolver),
        );
        let outcome = ctx
            .authorize(PermissionAction::Command {
                program: "npm".into(),
                args: vec!["install".into()],
                cwd: PathBuf::from("."),
                risk: CommandRisk::Medium,
            })
            .await;

        assert!(outcome.is_err());
    }
}
