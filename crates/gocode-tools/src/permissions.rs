use std::{future::Future, path::PathBuf, pin::Pin, sync::Arc};

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
pub type ResolveFuture<'a> = Pin<Box<dyn Future<Output = bool> + Send + 'a>>;

/// Resolves an interactive `Ask` decision to a boolean outcome.
///
/// The concrete implementation is the permission-modal integration point: a TUI-backed resolver
/// prompts the user, while tests use a scripted resolver.
pub trait PermissionResolver: Send + Sync {
    /// Returns `true` if the user approved `request`, `false` if they denied it.
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
        Box::pin(async { false })
    }
}

/// Decides `Allow` / `Ask` / `Deny` for one action, independent of how `Ask` gets resolved.
pub trait PermissionPolicy: Send + Sync {
    /// Evaluates the requested action.
    fn evaluate(&self, action: &PermissionAction) -> PermissionDecision;
}

/// The MVP default policy described in `docs/TOOLS.md` §25.
///
/// Read-only tools are always allowed. Writes are allowed while editing is enabled for the
/// current task. Commands are evaluated by risk: low is allowed, medium asks for confirmation,
/// high is denied outright (the MVP exposes no dedicated commit, push, reset, checkout, or
/// generic network tool, so high-risk commands stay behind an explicit deny rather than a
/// confirmable prompt).
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
            PermissionAction::Command {
                program,
                args,
                cwd,
                risk,
            } => match risk {
                CommandRisk::Low => PermissionDecision::Allow,
                CommandRisk::Medium => PermissionDecision::Ask(PermissionRequest {
                    summary: format!("run: {program} {}", args.join(" "))
                        .trim()
                        .to_string(),
                    working_directory: cwd.clone(),
                }),
                CommandRisk::High => PermissionDecision::Deny(PermissionReason(format!(
                    "{program} is classified high-risk and requires an explicit, narrower tool"
                ))),
            },
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
            PermissionDecision::Ask(request) => {
                if self.resolver.resolve(&request).await {
                    Ok(Some(request))
                } else {
                    Err(PermissionReason("the user denied this action".into()))
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        AlwaysDenyResolver, CommandRisk, DefaultPermissionPolicy, PermissionAction,
        PermissionContext, PermissionDecision, PermissionPolicy, PermissionRequest,
        PermissionResolver, ResolveFuture, classify_command_risk,
    };
    use std::{path::PathBuf, sync::Arc};

    struct ScriptedResolver(bool);

    impl PermissionResolver for ScriptedResolver {
        fn resolve<'a>(&'a self, _request: &'a PermissionRequest) -> ResolveFuture<'a> {
            let approved = self.0;
            Box::pin(async move { approved })
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
    fn high_risk_commands_are_denied_outright() {
        let policy = DefaultPermissionPolicy::editing();
        let decision = policy.evaluate(&PermissionAction::Command {
            program: "git".into(),
            args: vec!["push".into()],
            cwd: PathBuf::from("."),
            risk: CommandRisk::High,
        });
        assert!(matches!(decision, PermissionDecision::Deny(_)));
    }

    #[test]
    fn medium_risk_commands_carry_the_working_directory_and_action_in_the_prompt() {
        let policy = DefaultPermissionPolicy::editing();
        let decision = policy.evaluate(&PermissionAction::Command {
            program: "npm".into(),
            args: vec!["install".into()],
            cwd: PathBuf::from(r"C:\dev\my-project"),
            risk: CommandRisk::Medium,
        });
        match decision {
            PermissionDecision::Ask(request) => {
                assert!(request.summary.contains("npm install"));
                assert_eq!(
                    request.working_directory,
                    PathBuf::from(r"C:\dev\my-project")
                );
            }
            other => panic!("expected an Ask decision, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn authorize_denies_ask_decisions_by_default() {
        let ctx = PermissionContext::read_only_default();
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
            Arc::new(DefaultPermissionPolicy::editing()),
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
            Arc::new(DefaultPermissionPolicy::editing()),
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
