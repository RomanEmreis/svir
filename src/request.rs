//! A request: the model, the conversation, the tools, and how to answer.

use serde::{Deserialize, Serialize};

use crate::{Completion, Message, Tool, ToolResult, Toolbox};

/// How much the model should reason before answering.
///
/// Each wire API maps it explicitly: Chat Completions sends `reasoning_effort`, with `Off` as
/// `none`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum Effort {
    /// No reasoning.
    Off,
    /// A little.
    Low,
    /// Some.
    Medium,
    /// A lot.
    High,
    /// As much as the model offers.
    #[serde(rename = "xhigh")]
    XHigh,
}

/// A request to a model.
///
/// Built with [`Request::new`] and the methods below; the fields are public for reading, for
/// example by a layer.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct Request {
    /// The model, by the ID its server uses.
    pub model: String,
    /// The system prompt. A field rather than a message: each API puts it where it expects it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system: Option<String>,
    /// The conversation, in order.
    #[serde(default)]
    pub messages: Vec<Message>,
    /// Tools the model may call.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<Tool>,
    /// The most tokens to generate.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u64>,
    /// Sampling temperature.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    /// Reasoning effort.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<Effort>,
    /// Whether to ask for token usage. Unset means the client's default, which is on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub include_usage: Option<bool>,
    /// Whether reasoning in earlier answers is sent back, under the field it arrived in.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub send_reasoning: bool,
}

impl Request {
    /// An empty request to `model`.
    pub fn new(model: impl Into<String>) -> Self {
        Self {
            model: model.into(),
            system: None,
            messages: Vec::new(),
            tools: Vec::new(),
            max_tokens: None,
            temperature: None,
            reasoning: None,
            include_usage: None,
            send_reasoning: false,
        }
    }

    /// Sets the system prompt.
    pub fn system(mut self, prompt: impl Into<String>) -> Self {
        self.system = Some(prompt.into());
        self
    }

    /// Adds a user message with this text.
    pub fn user(self, text: impl Into<String>) -> Self {
        self.message(Message::user(text))
    }

    /// Adds a message.
    pub fn message(mut self, message: Message) -> Self {
        self.messages.push(message);
        self
    }

    /// Adds the model's answer, with its text, tool calls, and reasoning.
    pub fn assistant(self, done: Completion) -> Self {
        self.message(Message::from(done))
    }

    /// Adds the result of tool call `call_id`.
    pub fn tool_result(self, call_id: impl Into<String>, content: impl Into<String>) -> Self {
        self.message(Message::tool_result(ToolResult::new(call_id, content)))
    }

    /// Adds tool results, one message each.
    pub fn tool_results(mut self, results: impl IntoIterator<Item = ToolResult>) -> Self {
        self.messages
            .extend(results.into_iter().map(Message::tool_result));
        self
    }

    /// Adds a tool the model may call.
    pub fn tool(mut self, tool: Tool) -> Self {
        self.tools.push(tool);
        self
    }

    /// Adds the tools of a tool set, such as [`Tools`](crate::Tools). The request takes their
    /// descriptions; answering the model's calls stays with the tool set.
    pub fn tools(mut self, toolbox: &(impl Toolbox + ?Sized)) -> Self {
        self.tools.extend(toolbox.tools());
        self
    }

    /// Sets the reasoning effort.
    pub fn reasoning(mut self, effort: Effort) -> Self {
        self.reasoning = Some(effort);
        self
    }

    /// Sets the most tokens to generate.
    pub fn max_tokens(mut self, tokens: u64) -> Self {
        self.max_tokens = Some(tokens);
        self
    }

    /// Sets the sampling temperature.
    pub fn temperature(mut self, temperature: f32) -> Self {
        self.temperature = Some(temperature);
        self
    }

    /// Asks for token usage, or not, overriding the client's default.
    pub fn include_usage(mut self, include: bool) -> Self {
        self.include_usage = Some(include);
        self
    }

    /// Sends reasoning in earlier answers back, or not (the default).
    pub fn send_reasoning(mut self, send: bool) -> Self {
        self.send_reasoning = send;
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{FinishReason, Part, Role, ToolCall};

    #[test]
    fn a_tool_round_trip_reads_as_the_conversation() {
        let mut done = Completion::new(FinishReason::ToolCalls);
        done.calls = vec![
            ToolCall::new("call-a", "lookup", r#"{"value":1}"#),
            ToolCall::new("call-b", "lookup", r#"{"value":2}"#),
        ];

        let request = Request::new("m")
            .system("Be brief.")
            .tool(Tool::new("lookup", "Look up a value."))
            .user("Use lookup")
            .assistant(done)
            .tool_results([
                ToolResult::new("call-a", "42"),
                ToolResult::new("call-b", "43"),
            ]);

        let roles: Vec<_> = request.messages.iter().map(|m| m.role).collect();
        assert_eq!(roles, [Role::User, Role::Assistant, Role::Tool, Role::Tool]);
        assert_eq!(request.messages[1].parts.len(), 2);
        assert_eq!(
            request.messages[3].parts,
            [Part::ToolResult(ToolResult::new("call-b", "43"))]
        );
        assert_eq!(request.system.as_deref(), Some("Be brief."));
    }

    #[test]
    fn nothing_is_set_until_asked() {
        let request = Request::new("m");
        assert_eq!(request.max_tokens, None);
        assert_eq!(request.temperature, None);
        assert_eq!(request.reasoning, None);
        assert_eq!(request.include_usage, None);
        assert!(!request.send_reasoning);

        let request = request
            .max_tokens(16)
            .temperature(0.5)
            .reasoning(Effort::Low)
            .include_usage(false)
            .send_reasoning(true);
        assert_eq!(request.max_tokens, Some(16));
        assert_eq!(request.temperature, Some(0.5));
        assert_eq!(request.reasoning, Some(Effort::Low));
        assert_eq!(request.include_usage, Some(false));
        assert!(request.send_reasoning);
    }
}
