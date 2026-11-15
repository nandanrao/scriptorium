//! A tool call's arguments, with errors phrased for the model.

use serde_json::{Map, Value};

use crate::ToolError;

pub(crate) struct Args<'a>(&'a Map<String, Value>);

static EMPTY: std::sync::LazyLock<Map<String, Value>> = std::sync::LazyLock::new(Map::new);

impl<'a> Args<'a> {
    /// `null` counts as no arguments (some clients send it for tools without any).
    pub(crate) fn new(v: &'a Value) -> Result<Self, ToolError> {
        match v {
            Value::Object(m) => Ok(Self(m)),
            Value::Null => Ok(Self(&EMPTY)),
            _ => Err(ToolError::Args("the arguments must be a JSON object".into())),
        }
    }

    pub(crate) fn get(&self, k: &str) -> Option<&'a Value> {
        self.0.get(k).filter(|v| !v.is_null())
    }

    pub(crate) fn opt_str(&self, k: &str) -> Result<Option<&'a str>, ToolError> {
        match self.get(k) {
            None => Ok(None),
            Some(v) => v.as_str().map(Some).ok_or_else(|| ToolError::Args(format!("`{k}` must be a string"))),
        }
    }

    pub(crate) fn opt_int(&self, k: &str) -> Result<Option<i64>, ToolError> {
        match self.get(k) {
            None => Ok(None),
            Some(v) => v.as_i64().map(Some).ok_or_else(|| ToolError::Args(format!("`{k}` must be a whole number"))),
        }
    }

    pub(crate) fn bool(&self, k: &str) -> Result<bool, ToolError> {
        match self.get(k) {
            None => Ok(false),
            Some(v) => v.as_bool().ok_or_else(|| ToolError::Args(format!("`{k}` must be true or false"))),
        }
    }
}
