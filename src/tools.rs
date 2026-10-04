//! Tool sets: describing tools to a model, and answering its calls.
//!
//! [`Toolbox`] is the trait; [`Tools`] is a plain registry of handlers for callers who need
//! nothing more. Neither runs a loop: they answer calls, and feeding the answers back to the
//! model is the caller's code.
//!
//! ```
//! use serde::Deserialize;
//! use serde_json::json;
//! use svir::prelude::*;
//!
//! #[derive(Deserialize)]
//! struct Lookup {
//!     value: i64,
//! }
//!
//! let lookup = Tool::new("lookup", "Look up a value.").schema(json!({
//!     "type": "object",
//!     "properties": {"value": {"type": "integer"}},
//!     "required": ["value"]
//! }));
//! let tools = Tools::new().add_tool(lookup, |args: Lookup| async move {
//!     (args.value * 2).to_string()
//! });
//!
//! let request = Request::new("m").tools(&tools).user("What is 21 doubled?");
//! assert_eq!(request.tools[0].name, "lookup");
//! ```

use std::{fmt, future::Future, pin::Pin};

use serde::de::DeserializeOwned;

use crate::{Tool, ToolCall, ToolResult};

/// Anything that can describe tools to a model and answer its calls.
pub trait Toolbox {
    /// The tools, as they are described to the model.
    fn tools(&self) -> Vec<Tool>;

    /// Answers one call. A failure is a result too, made with [`ToolResult::error`]: the model is
    /// told what went wrong.
    fn call(&self, call: &ToolCall) -> impl Future<Output = ToolResult> + Send;

    /// Answers calls one after another, in order.
    fn call_all(&self, calls: &[ToolCall]) -> impl Future<Output = Vec<ToolResult>> + Send
    where
        Self: Sync,
    {
        async move {
            let mut results = Vec::with_capacity(calls.len());
            for call in calls {
                results.push(self.call(call).await);
            }

            results
        }
    }
}

/// What a tool handler may return.
///
/// A string is the result as it is; a [`serde_json::Value`] is written as JSON; a `Result` is its
/// value, or its error told to the model.
pub trait ToolOutput {
    /// The result for the model, or what went wrong.
    fn into_content(self) -> Result<String, String>;
}

impl ToolOutput for String {
    fn into_content(self) -> Result<String, String> {
        Ok(self)
    }
}

impl ToolOutput for &str {
    fn into_content(self) -> Result<String, String> {
        Ok(self.to_owned())
    }
}

impl ToolOutput for serde_json::Value {
    fn into_content(self) -> Result<String, String> {
        Ok(self.to_string())
    }
}

impl<T: ToolOutput, E: fmt::Display> ToolOutput for Result<T, E> {
    fn into_content(self) -> Result<String, String> {
        self.map_err(|error| error.to_string())?.into_content()
    }
}

/// The future of a handler: the result for the model, or what went wrong.
type Answer = Pin<Box<dyn Future<Output = Result<String, String>> + Send>>;

/// A handler behind a trait object, taking the raw arguments. Handlers differ in type, so a
/// registry of them has to erase it; a tool call is not a hot path.
type Handler = Box<dyn Fn(&str) -> Answer + Send + Sync>;

/// A registry of tools and their handlers.
///
/// It is built explicitly, tool by tool: different requests and agents use different tool sets.
/// A handler takes its arguments as a type that deserializes from the JSON the model wrote, so
/// arguments that do not fit the type never reach it: the model is told they are invalid.
#[derive(Default)]
pub struct Tools {
    entries: Vec<(Tool, Handler)>,
}

impl Tools {
    /// An empty registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds `tool`, which describes itself with its own schema, and its handler. A tool with a
    /// name already registered replaces the earlier one.
    pub fn add_tool<A, F, Fut, R>(mut self, tool: Tool, handler: F) -> Self
    where
        A: DeserializeOwned + Send + 'static,
        F: Fn(A) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = R> + Send + 'static,
        R: ToolOutput,
    {
        let handler: Handler = Box::new(move |arguments: &str| {
            // A model calling a tool without arguments may send an empty string.
            let arguments = if arguments.trim().is_empty() {
                "{}"
            } else {
                arguments
            };
            match serde_json::from_str::<A>(arguments) {
                Ok(arguments) => {
                    let answer = handler(arguments);
                    Box::pin(async move { answer.await.into_content() })
                }
                Err(error) => {
                    let invalid = format!("invalid arguments: {error}");
                    Box::pin(std::future::ready(Err(invalid)))
                }
            }
        });

        self.entries.retain(|(known, _)| known.name != tool.name);
        self.entries.push((tool, handler));
        self
    }

    /// Adds a tool whose input schema is derived from the type of the handler's arguments.
    #[cfg(feature = "schemars")]
    pub fn add<A, F, Fut, R>(
        self,
        name: impl Into<String>,
        description: impl Into<String>,
        handler: F,
    ) -> Self
    where
        A: DeserializeOwned + schemars::JsonSchema + Send + 'static,
        F: Fn(A) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = R> + Send + 'static,
        R: ToolOutput,
    {
        let mut schema = schemars::schema_for!(A).to_value();
        if let Some(schema) = schema.as_object_mut() {
            // Meta-data about the schema itself is of no use to a model.
            schema.remove("$schema");
            schema.remove("title");
        }

        self.add_tool(Tool::new(name, description).schema(schema), handler)
    }
}

impl Toolbox for Tools {
    fn tools(&self) -> Vec<Tool> {
        self.entries.iter().map(|(tool, _)| tool.clone()).collect()
    }

    async fn call(&self, call: &ToolCall) -> ToolResult {
        let answer = match self.entries.iter().find(|(tool, _)| tool.name == call.name) {
            Some((_, handler)) => handler(&call.arguments).await,
            None => Err(format!("no tool named {}", call.name)),
        };

        match answer {
            Ok(content) => ToolResult::new(&call.id, content),
            Err(message) => ToolResult::error(&call.id, message),
        }
    }
}

impl fmt::Debug for Tools {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let names: Vec<&str> = self
            .entries
            .iter()
            .map(|(tool, _)| tool.name.as_str())
            .collect();
        f.debug_struct("Tools").field("tools", &names).finish()
    }
}

#[cfg(test)]
mod tests {
    use serde::Deserialize;
    use serde_json::json;

    use super::*;
    use crate::Request;

    #[derive(Deserialize)]
    #[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
    struct Lookup {
        value: i64,
    }

    fn tools() -> Tools {
        let schema = json!({"type": "object", "properties": {"value": {"type": "integer"}}});
        Tools::new()
            .add_tool(
                Tool::new("double", "Double a value.").schema(schema),
                |args: Lookup| async move { (args.value * 2).to_string() },
            )
            .add_tool(
                Tool::new("fail", "Always fails."),
                |_: serde_json::Value| async { Err::<String, _>("out of order") },
            )
    }

    /// Runs a future that never waits, which is all these handlers are.
    fn now<T>(future: impl Future<Output = T>) -> T {
        let mut future = std::pin::pin!(future);
        let mut cx = std::task::Context::from_waker(std::task::Waker::noop());
        match future.as_mut().poll(&mut cx) {
            std::task::Poll::Ready(value) => value,
            std::task::Poll::Pending => panic!("the future waited"),
        }
    }

    #[test]
    fn a_call_runs_its_handler_with_typed_arguments() {
        let call = ToolCall::new("call-a", "double", r#"{"value":21}"#);
        assert_eq!(now(tools().call(&call)), ToolResult::new("call-a", "42"));
    }

    #[test]
    fn failures_are_results_for_the_model() {
        let tools = tools();
        let failure = |name: &str, arguments: &str| {
            let result = now(tools.call(&ToolCall::new("id", name, arguments)));
            assert!(result.is_error, "{name} {arguments}");
            result.content
        };

        assert_eq!(failure("fail", ""), "out of order");
        assert_eq!(failure("missing", "{}"), "no tool named missing");
        assert!(failure("double", r#"{"value":"x"}"#).starts_with("invalid arguments"));
        assert!(failure("double", "{").starts_with("invalid arguments"));
        assert!(!now(tools.call(&ToolCall::new("id", "double", r#"{"value":1}"#))).is_error);
    }

    #[test]
    fn calls_are_answered_in_order() {
        let calls = [
            ToolCall::new("a", "double", r#"{"value":1}"#),
            ToolCall::new("b", "double", r#"{"value":2}"#),
        ];
        let results = now(tools().call_all(&calls));
        assert_eq!(
            results,
            [ToolResult::new("a", "2"), ToolResult::new("b", "4")]
        );
    }

    #[test]
    fn a_request_takes_its_tools_from_a_toolbox() {
        let request = Request::new("m").tools(&tools());
        let names: Vec<_> = request.tools.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(names, ["double", "fail"]);
    }

    #[test]
    fn a_tool_added_again_replaces_the_earlier_one() {
        let tools = tools().add_tool(
            Tool::new("double", "Triple, really."),
            |args: Lookup| async move { (args.value * 3).to_string() },
        );
        let call = ToolCall::new("id", "double", r#"{"value":2}"#);
        assert_eq!(now(tools.call(&call)).content, "6");
        assert_eq!(tools.tools().len(), 2);
    }

    #[cfg(feature = "schemars")]
    #[test]
    fn a_schema_is_derived_from_the_argument_type() {
        let tools = Tools::new().add("double", "Double a value.", |args: Lookup| async move {
            (args.value * 2).to_string()
        });
        let schema = &tools.tools()[0].input_schema;

        assert_eq!(schema["type"], "object");
        assert_eq!(schema["properties"]["value"]["type"], "integer");
        assert_eq!(schema["required"], json!(["value"]));
        assert_eq!(schema.get("$schema"), None);
        assert_eq!(schema.get("title"), None);
    }
}
