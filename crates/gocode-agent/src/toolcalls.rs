//! Assembles streamed tool-call fragments into complete, validated-ready calls.

use std::collections::BTreeMap;

use gocode_core::ToolCallDelta;
use gocode_tools::{ToolCall, ToolCallId, ToolName};

/// Accumulates [`ToolCallDelta`] fragments, correlated by index, into complete [`ToolCall`]s.
///
/// No partial call is ever handed to a tool: [`Self::finish`] is only called once a turn's
/// stream ends, after every fragment for that turn has been applied.
#[derive(Default)]
pub(crate) struct ToolCallAssembler {
    pending: BTreeMap<usize, PendingCall>,
}

#[derive(Default)]
struct PendingCall {
    id: Option<String>,
    name: String,
    arguments: String,
}

impl ToolCallAssembler {
    pub(crate) fn apply(&mut self, delta: ToolCallDelta) {
        let entry = self.pending.entry(delta.index).or_default();
        if let Some(id) = delta.id {
            entry.id = Some(id);
        }
        if let Some(name_delta) = delta.name_delta {
            entry.name.push_str(&name_delta);
        }
        if let Some(arguments_delta) = delta.arguments_delta {
            entry.arguments.push_str(&arguments_delta);
        }
    }

    /// Finalizes every accumulated fragment into a complete tool call, in requested order.
    ///
    /// Arguments that never form valid JSON become an empty object rather than failing the
    /// whole turn: the tool's own schema validation reports the concrete problem back to the
    /// model.
    pub(crate) fn finish(self) -> Vec<ToolCall> {
        self.pending
            .into_values()
            .enumerate()
            .map(|(position, call)| {
                let arguments = if call.arguments.trim().is_empty() {
                    serde_json::json!({})
                } else {
                    serde_json::from_str(&call.arguments).unwrap_or_else(|_| serde_json::json!({}))
                };

                ToolCall {
                    id: ToolCallId::new(call.id.unwrap_or_else(|| format!("call-{position}"))),
                    name: ToolName::new(call.name),
                    arguments,
                }
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::ToolCallAssembler;
    use gocode_core::ToolCallDelta;

    #[test]
    fn assembles_fragments_split_across_multiple_deltas() {
        let mut assembler = ToolCallAssembler::default();
        assembler.apply(ToolCallDelta {
            index: 0,
            id: Some("call-1".into()),
            name_delta: Some("read_file".into()),
            arguments_delta: Some(r#"{"path":"#.into()),
        });
        assembler.apply(ToolCallDelta {
            index: 0,
            id: None,
            name_delta: None,
            arguments_delta: Some(r#""a.rs"}"#.into()),
        });

        let calls = assembler.finish();

        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].id.as_str(), "call-1");
        assert_eq!(calls[0].name.as_str(), "read_file");
        assert_eq!(calls[0].arguments, serde_json::json!({"path": "a.rs"}));
    }

    #[test]
    fn preserves_requested_order_across_indices() {
        let mut assembler = ToolCallAssembler::default();
        assembler.apply(ToolCallDelta {
            index: 1,
            id: Some("call-b".into()),
            name_delta: Some("search".into()),
            arguments_delta: Some("{}".into()),
        });
        assembler.apply(ToolCallDelta {
            index: 0,
            id: Some("call-a".into()),
            name_delta: Some("list_files".into()),
            arguments_delta: Some("{}".into()),
        });

        let calls = assembler.finish();

        assert_eq!(calls[0].id.as_str(), "call-a");
        assert_eq!(calls[1].id.as_str(), "call-b");
    }

    #[test]
    fn malformed_arguments_become_an_empty_object_instead_of_failing() {
        let mut assembler = ToolCallAssembler::default();
        assembler.apply(ToolCallDelta {
            index: 0,
            id: Some("call-1".into()),
            name_delta: Some("read_file".into()),
            arguments_delta: Some("not json".into()),
        });

        let calls = assembler.finish();

        assert_eq!(calls[0].arguments, serde_json::json!({}));
    }
}
