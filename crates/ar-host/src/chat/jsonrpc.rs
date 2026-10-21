//! JSON-RPC 2.0 lines, as Codex's app-server and ACP agents speak them over stdio. The adapters
//! keep their own table of what each request id was for, so nothing here waits: a reply is just
//! another line the adapter's loop handles.

use serde_json::{Value, json};

/// One line from the agent.
#[derive(Debug)]
pub enum Incoming {
    /// The answer to one of our requests.
    Response {
        id: u64,
        result: Result<Value, String>,
    },
    /// The agent asks us something and waits for an answer.
    Request {
        id: Value,
        method: String,
        params: Value,
    },
    Notification {
        method: String,
        params: Value,
    },
}

pub fn parse(line: &str) -> Option<Incoming> {
    let v: Value = serde_json::from_str(line).ok()?;
    let method = v.get("method").and_then(Value::as_str).map(str::to_string);
    let params = v.get("params").cloned().unwrap_or(Value::Null);
    match (method, v.get("id")) {
        (Some(method), Some(id)) if !id.is_null() => Some(Incoming::Request { id: id.clone(), method, params }),
        (Some(method), _) => Some(Incoming::Notification { method, params }),
        (None, Some(id)) => {
            let id = id.as_u64()?;
            let result = match v.get("error") {
                Some(e) if !e.is_null() => {
                    Err(e.get("message").and_then(Value::as_str).map(str::to_string).unwrap_or_else(|| e.to_string()))
                }
                _ => Ok(v.get("result").cloned().unwrap_or(Value::Null)),
            };
            Some(Incoming::Response { id, result })
        }
        _ => None,
    }
}

pub fn request(id: u64, method: &str, params: Value) -> String {
    json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}).to_string()
}

pub fn notification(method: &str, params: Option<Value>) -> String {
    match params {
        Some(p) => json!({"jsonrpc": "2.0", "method": method, "params": p}).to_string(),
        None => json!({"jsonrpc": "2.0", "method": method}).to_string(),
    }
}

pub fn response(id: &Value, result: Value) -> String {
    json!({"jsonrpc": "2.0", "id": id, "result": result}).to_string()
}

pub fn error(id: &Value, code: i64, message: &str) -> String {
    json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}}).to_string()
}

/// "Method not found", for requests this client does not serve.
pub fn unsupported(id: &Value, method: &str) -> String {
    error(id, -32601, &format!("{method} is not supported by this client"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lines_are_classified() {
        assert!(matches!(parse(r#"{"id":3,"result":{"a":1}}"#), Some(Incoming::Response { id: 3, result: Ok(_) })));
        assert!(matches!(
            parse(r#"{"id":4,"error":{"code":-1,"message":"no"}}"#),
            Some(Incoming::Response { id: 4, result: Err(m) }) if m == "no"
        ));
        assert!(matches!(parse(r#"{"id":"x","method":"m","params":{}}"#), Some(Incoming::Request { .. })));
        assert!(matches!(parse(r#"{"method":"n"}"#), Some(Incoming::Notification { .. })));
        assert!(parse("not json").is_none());
    }
}
