//! Minimal JSON-RPC 2.0 message types.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Standard and application error codes.
pub mod codes {
    pub const PARSE_ERROR: i64 = -32700;
    pub const INVALID_REQUEST: i64 = -32600;
    pub const METHOD_NOT_FOUND: i64 = -32601;
    pub const INVALID_PARAMS: i64 = -32602;
    pub const INTERNAL_ERROR: i64 = -32603;

    /// The device's capabilities do not allow this method.
    pub const CAPABILITY_DENIED: i64 = -32001;
    /// The client's protocol version is too old.
    pub const UPDATE_REQUIRED: i64 = -32002;
    /// The referenced object does not exist.
    pub const NOT_FOUND: i64 = -32003;
    /// The object is in a state that does not allow the operation.
    pub const INVALID_STATE: i64 = -32004;
    /// `hello` must be the first message on a connection.
    pub const HELLO_REQUIRED: i64 = -32005;
    /// A required component (agent, sandbox, connector) is unavailable.
    pub const UNAVAILABLE: i64 = -32006;
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RpcError {
    pub code: i64,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

impl RpcError {
    pub fn new(code: i64, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            data: None,
        }
    }
    pub fn with_data(mut self, data: Value) -> Self {
        self.data = Some(data);
        self
    }
    pub fn invalid_params(msg: impl Into<String>) -> Self {
        Self::new(codes::INVALID_PARAMS, msg)
    }
    pub fn not_found(msg: impl Into<String>) -> Self {
        Self::new(codes::NOT_FOUND, msg)
    }
    pub fn invalid_state(msg: impl Into<String>) -> Self {
        Self::new(codes::INVALID_STATE, msg)
    }
    pub fn internal(msg: impl Into<String>) -> Self {
        Self::new(codes::INTERNAL_ERROR, msg)
    }
}

impl std::fmt::Display for RpcError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} (code {})", self.message, self.code)
    }
}

impl std::error::Error for RpcError {}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Request {
    pub jsonrpc: String,
    pub id: Value,
    pub method: String,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub params: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Notification {
    pub jsonrpc: String,
    pub method: String,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub params: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Response {
    pub jsonrpc: String,
    pub id: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<RpcError>,
}

impl Request {
    pub fn new(id: impl Into<Value>, method: impl Into<String>, params: Value) -> Self {
        Self {
            jsonrpc: "2.0".into(),
            id: id.into(),
            method: method.into(),
            params,
        }
    }
}

impl Notification {
    pub fn new(method: impl Into<String>, params: Value) -> Self {
        Self {
            jsonrpc: "2.0".into(),
            method: method.into(),
            params,
        }
    }
}

impl Response {
    pub fn ok(id: Value, result: Value) -> Self {
        Self {
            jsonrpc: "2.0".into(),
            id,
            result: Some(result),
            error: None,
        }
    }
    pub fn err(id: Value, error: RpcError) -> Self {
        Self {
            jsonrpc: "2.0".into(),
            id,
            result: None,
            error: Some(error),
        }
    }
    pub fn into_result(self) -> Result<Value, RpcError> {
        match self.error {
            Some(e) => Err(e),
            None => Ok(self.result.unwrap_or(Value::Null)),
        }
    }
}

/// Any JSON-RPC message.
#[derive(Debug, Clone, PartialEq)]
pub enum Message {
    Request(Request),
    Notification(Notification),
    Response(Response),
}

impl Message {
    /// Parse one JSON-RPC message, classifying it by its fields.
    pub fn parse(text: &str) -> Result<Message, RpcError> {
        let v: Value = serde_json::from_str(text)
            .map_err(|e| RpcError::new(codes::PARSE_ERROR, format!("parse error: {e}")))?;
        Self::from_value(v)
    }

    pub fn from_value(v: Value) -> Result<Message, RpcError> {
        let obj = v
            .as_object()
            .ok_or_else(|| RpcError::new(codes::INVALID_REQUEST, "message must be an object"))?;
        let has_method = obj.contains_key("method");
        let has_id = obj.get("id").map(|i| !i.is_null()).unwrap_or(false);
        let bad = |e: serde_json::Error| RpcError::new(codes::INVALID_REQUEST, e.to_string());
        if has_method && has_id {
            Ok(Message::Request(serde_json::from_value(v).map_err(bad)?))
        } else if has_method {
            Ok(Message::Notification(
                serde_json::from_value(v).map_err(bad)?,
            ))
        } else if obj.contains_key("result") || obj.contains_key("error") {
            Ok(Message::Response(serde_json::from_value(v).map_err(bad)?))
        } else {
            Err(RpcError::new(
                codes::INVALID_REQUEST,
                "not a JSON-RPC message",
            ))
        }
    }

    pub fn to_json(&self) -> String {
        match self {
            Message::Request(r) => serde_json::to_string(r),
            Message::Notification(n) => serde_json::to_string(n),
            Message::Response(r) => serde_json::to_string(r),
        }
        .expect("JSON-RPC messages always serialize")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn classifies_messages() {
        let r = Message::parse(r#"{"jsonrpc":"2.0","id":1,"method":"hello","params":{}}"#).unwrap();
        assert!(matches!(r, Message::Request(_)));
        let n = Message::parse(r#"{"jsonrpc":"2.0","method":"event","params":{}}"#).unwrap();
        assert!(matches!(n, Message::Notification(_)));
        let s = Message::parse(r#"{"jsonrpc":"2.0","id":1,"result":null}"#).unwrap();
        assert!(matches!(s, Message::Response(_)));
        let e = Message::parse(r#"{"jsonrpc":"2.0","id":1,"error":{"code":-1,"message":"x"}}"#)
            .unwrap();
        match e {
            Message::Response(r) => assert_eq!(r.into_result().unwrap_err().code, -1),
            _ => panic!(),
        }
        assert!(Message::parse("[]").is_err());
        assert!(Message::parse("{").is_err());
    }

    #[test]
    fn roundtrip() {
        let m = Message::Request(Request::new(7, "session.list", json!({"a":1})));
        assert_eq!(Message::parse(&m.to_json()).unwrap(), m);
    }
}
