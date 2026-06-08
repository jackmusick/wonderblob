use serde::Serialize;
use thiserror::Error;

/// Common error taxonomy every backend maps into (spec: "Error handling").
#[derive(Debug, Error, Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum StorageError {
    #[error("authentication failed: {detail}")]
    AuthFailed { detail: String },
    #[error("not found: {path}")]
    NotFound { path: String },
    #[error("permission denied: {path}")]
    PermissionDenied { path: String },
    #[error("network error: {detail}")]
    Network { detail: String },
    #[error("conflict at {path}: {detail}")]
    Conflict { path: String, detail: String },
    #[error("quota exceeded")]
    QuotaExceeded,
    #[error("operation not supported by this backend: {op}")]
    Unsupported { op: String },
    #[error("{detail}")]
    Other { detail: String },
}

impl StorageError {
    /// Transient errors are retried with backoff; the rest surface immediately.
    pub fn is_retryable(&self) -> bool {
        matches!(self, StorageError::Network { .. })
    }

    pub fn other(e: impl std::fmt::Display) -> Self {
        StorageError::Other {
            detail: e.to_string(),
        }
    }
}

pub type Result<T> = std::result::Result<T, StorageError>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn errors_serialize_with_kind_for_frontend() {
        let e = StorageError::NotFound { path: "/x".into() };
        let json = serde_json::to_value(&e).unwrap();
        assert_eq!(json["kind"], "notFound");
        assert_eq!(json["path"], "/x");
    }

    #[test]
    fn auth_failed_is_not_retryable_but_network_is() {
        assert!(!StorageError::AuthFailed {
            detail: "bad key".into()
        }
        .is_retryable());
        assert!(StorageError::Network {
            detail: "reset".into()
        }
        .is_retryable());
    }
}
