//! The transport abstraction every mediated call flows through.

use serde_json::Value;
use thiserror::Error;

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum RpcError {
    /// The call timed out; whether the provider accepted it is unknown.
    #[error("transport timeout (acceptance unknown)")]
    Timeout,
    #[error("transport failure: {0}")]
    Transport(String),
    #[error("method '{0}' is not supported by this endpoint")]
    MethodUnsupported(String),
}

/// A single JSON-RPC-style endpoint. Implementations are chain-agnostic:
/// `params` in, `result` out.
pub trait RpcTransport: Send + Sync {
    fn name(&self) -> &str;
    fn call(&self, method: &str, params: &Value) -> Result<Value, RpcError>;
}

impl<T: RpcTransport + ?Sized> RpcTransport for &T {
    fn name(&self) -> &str {
        (**self).name()
    }
    fn call(&self, method: &str, params: &Value) -> Result<Value, RpcError> {
        (**self).call(method, params)
    }
}

impl<T: RpcTransport + ?Sized> RpcTransport for std::sync::Arc<T> {
    fn name(&self) -> &str {
        (**self).name()
    }
    fn call(&self, method: &str, params: &Value) -> Result<Value, RpcError> {
        (**self).call(method, params)
    }
}
