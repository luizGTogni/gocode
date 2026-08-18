//! Structured, blocking questions that let an agent involve its user in a decision.

use std::{future::Future, pin::Pin, sync::Arc};

use serde::Deserialize;

use crate::contract::{Tool, ToolDefinition, ToolError, ToolFuture, ToolOutput, ToolResult};

/// One short, user-visible alternative in a guided decision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserChoice {
    pub label: String,
    pub summary: String,
    pub advantages: String,
    pub disadvantages: String,
    pub recommended: bool,
}

/// A decision that requires the user's preference before the agent can continue.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserQuestion {
    pub title: String,
    pub context: String,
    pub choices: Vec<UserChoice>,
}

/// UI bridge for a question. Implementations may wait until the user selects an option.
pub trait UserQuestionResolver: Send + Sync {
    fn ask<'a>(
        &'a self,
        question: UserQuestion,
    ) -> Pin<Box<dyn Future<Output = Option<String>> + Send + 'a>>;
}

/// Model-facing tool that pauses an agent run for a concise, structured user decision.
pub struct AskUserTool {
    resolver: Arc<dyn UserQuestionResolver>,
}

impl AskUserTool {
    #[must_use]
    pub fn new(resolver: Arc<dyn UserQuestionResolver>) -> Self {
        Self { resolver }
    }

    /// Placeholder used by non-interactive contexts such as isolated subagents.
    #[must_use]
    pub fn unavailable() -> Self {
        Self::new(Arc::new(UnavailableResolver))
    }
}

struct UnavailableResolver;

impl UserQuestionResolver for UnavailableResolver {
    fn ask<'a>(
        &'a self,
        _question: UserQuestion,
    ) -> Pin<Box<dyn Future<Output = Option<String>> + Send + 'a>> {
        Box::pin(async { None })
    }
}

#[derive(Debug, Deserialize)]
struct AskUserArgs {
    title: String,
    #[serde(default)]
    context: String,
    choices: Vec<AskUserChoice>,
}

#[derive(Debug, Deserialize)]
struct AskUserChoice {
    label: String,
    summary: String,
    advantages: String,
    disadvantages: String,
    #[serde(default)]
    recommended: bool,
}

impl Tool for AskUserTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: crate::contract::ToolName::new("ask_user"),
            description: "Pause only when a meaningful product or implementation choice needs the user's preference. Present 2-4 concise options. Every option must include what it does, one advantage, and one disadvantage; mark at most one recommended option. Do not use this for permission to perform a tool action.".into(),
            input_schema: serde_json::json!({
                "type": "object",
                "required": ["title", "choices"],
                "properties": {
                    "title": {"type": "string", "description": "Short question."},
                    "context": {"type": "string", "description": "One concise sentence explaining why a decision is needed."},
                    "choices": {"type": "array", "minItems": 2, "maxItems": 4, "items": {"type": "object", "required": ["label", "summary", "advantages", "disadvantages"], "properties": {"label": {"type": "string"}, "summary": {"type": "string"}, "advantages": {"type": "string"}, "disadvantages": {"type": "string"}, "recommended": {"type": "boolean"}}}}
                }
            }),
        }
    }

    fn execute(
        &self,
        ctx: crate::contract::ToolContext,
        input: serde_json::Value,
    ) -> ToolFuture<'_> {
        let resolver = Arc::clone(&self.resolver);
        Box::pin(async move {
            let args: AskUserArgs = serde_json::from_value(input)
                .map_err(|error| ToolError::InvalidArguments(error.to_string()))?;
            if args.title.trim().is_empty() || !(2..=4).contains(&args.choices.len()) {
                return Err(ToolError::InvalidArguments(
                    "title is required and choices must contain 2 to 4 entries".into(),
                ));
            }
            if args
                .choices
                .iter()
                .filter(|choice| choice.recommended)
                .count()
                > 1
                || args.choices.iter().any(|choice| {
                    choice.label.trim().is_empty()
                        || choice.summary.trim().is_empty()
                        || choice.advantages.trim().is_empty()
                        || choice.disadvantages.trim().is_empty()
                })
            {
                return Err(ToolError::InvalidArguments("each choice needs concise label, summary, advantages and disadvantages; mark at most one recommended choice".into()));
            }
            let question = UserQuestion {
                title: args.title,
                context: args.context,
                choices: args
                    .choices
                    .into_iter()
                    .map(|choice| UserChoice {
                        label: choice.label,
                        summary: choice.summary,
                        advantages: choice.advantages,
                        disadvantages: choice.disadvantages,
                        recommended: choice.recommended,
                    })
                    .collect(),
            };
            let answer = resolver.ask(question).await.ok_or(ToolError::Cancelled)?;
            Ok(ToolResult::success(
                ctx.call_id,
                ToolOutput::new(format!("The user selected: {answer}")),
            ))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schema_requires_short_option_explanations() {
        let definition = AskUserTool::new(Arc::new(UnusedResolver)).definition();
        let required = definition.input_schema["properties"]["choices"]["items"]["required"]
            .as_array()
            .unwrap();
        assert!(required.iter().any(|item| item == "advantages"));
        assert!(required.iter().any(|item| item == "disadvantages"));
    }

    struct UnusedResolver;
    impl UserQuestionResolver for UnusedResolver {
        fn ask<'a>(
            &'a self,
            _question: UserQuestion,
        ) -> Pin<Box<dyn Future<Output = Option<String>> + Send + 'a>> {
            Box::pin(async { None })
        }
    }
}
