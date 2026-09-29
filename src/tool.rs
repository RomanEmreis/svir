//! Tools the model may call, and the calls it makes.

use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::{Value, json};

/// A tool the model may call: a name, what it does, and the JSON Schema of its input.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct Tool {
    /// The name the model calls it by.
    pub name: String,
    /// What it does, for the model to decide when to call it.
    pub description: String,
    /// JSON Schema of the arguments.
    pub input_schema: Value,
}

impl Tool {
    /// A tool that takes no arguments until [`schema`](Self::schema) says otherwise.
    pub fn new(name: impl Into<String>, description: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            description: description.into(),
            input_schema: json!({"type": "object", "properties": {}}),
        }
    }

    /// Sets the JSON Schema of the arguments.
    pub fn schema(mut self, schema: Value) -> Self {
        self.input_schema = schema;
        self
    }
}

/// A call the model made: which tool, and the arguments exactly as the model wrote them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct ToolCall {
    /// The server's ID for this call; the result refers to it.
    pub id: String,
    /// The tool called.
    pub name: String,
    /// The arguments as the raw string received. Parse with [`parse`](Self::parse).
    pub arguments: String,
}

impl ToolCall {
    /// A call, as a server would report it.
    pub fn new(
        id: impl Into<String>,
        name: impl Into<String>,
        arguments: impl Into<String>,
    ) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            arguments: arguments.into(),
        }
    }

    /// Parses the arguments into `T`.
    pub fn parse<T: DeserializeOwned>(&self) -> serde_json::Result<T> {
        serde_json::from_str(&self.arguments)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, PartialEq, Deserialize)]
    struct Lookup {
        value: i64,
    }

    #[test]
    fn arguments_parse_on_demand() {
        let call = ToolCall::new("call-a", "lookup", r#"{"value":1}"#);
        assert_eq!(call.parse::<Lookup>().unwrap(), Lookup { value: 1 });
        assert!(ToolCall::new("x", "lookup", "{").parse::<Lookup>().is_err());
    }

    #[test]
    fn a_new_tool_takes_no_arguments() {
        let tool = Tool::new("now", "The current time.");
        assert_eq!(
            tool.input_schema,
            json!({"type": "object", "properties": {}})
        );
    }
}
