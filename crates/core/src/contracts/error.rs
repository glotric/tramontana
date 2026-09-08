//! Normalized errors.
//!
//! `CORE_CONTRACTS.md` §12: adapters map vendor failures onto this stable set
//! before they cross a boundary, so neither the interface nor a persisted
//! event depends on a provider SDK's error type.
//!
//! The hard rule for this module is what must *not* appear: no secret, no full
//! prompt or file content, no unredacted command output, no raw SDK body, and
//! no absolute private path may reach `message` or `details`. Detail that
//! would breach that lives in local diagnostics, referenced by
//! [`ErrorEnvelope::diagnostic_ref`].

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::{CorrelationId, SCHEMA_VERSION};

/// Stable error categories.
///
/// Adapters choose the closest category rather than inventing one; a new
/// variant is a contract change (`CORE_CONTRACTS.md` §15).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub enum ErrorCode {
    /// A payload failed validation at a boundary.
    InvalidRequest,
    /// The addressed record does not exist.
    NotFound,
    /// The operation conflicted with concurrent state, such as a stale sequence.
    Conflict,
    /// Policy denied the operation outright.
    PermissionDenied,
    /// The operation needs an approval that has not been given.
    ApprovalRequired,
    /// A standing policy blocked the operation.
    PolicyBlocked,
    /// The selected model or worker cannot do what was asked.
    CapabilityUnsupported,
    /// The provider rejected the credential.
    ProviderAuthentication,
    /// The provider applied a rate limit.
    ProviderRateLimited,
    /// The provider is unreachable or unhealthy.
    ProviderUnavailable,
    /// The selected worker is unreachable or unhealthy.
    WorkerUnavailable,
    /// A tool ran and failed.
    ToolFailed,
    /// A deadline elapsed.
    Timeout,
    /// The operation was cancelled.
    Cancelled,
    /// A size, count, depth, or budget limit was exceeded.
    LimitExceeded,
    /// A protocol peer sent something malformed.
    ProtocolError,
    /// Durable storage failed.
    PersistenceError,
    /// Stored data needs a migration that has not run.
    MigrationRequired,
    /// Work stopped with an action whose replay safety is unknown.
    InterruptedUnsafe,
    /// An unexpected internal failure.
    Internal,
}

/// A safe, user-presentable error.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ErrorEnvelope {
    /// Contract schema version.
    pub schema_version: u32,
    /// Stable category.
    pub code: ErrorCode,
    /// Safe, user-presentable message. Never carries secrets or content.
    pub message: String,
    /// Whether retrying the identical request could succeed.
    pub retryable: bool,
    /// Hint for how long to wait before retrying, when the source supplies one.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub retry_after_ms: Option<u64>,
    /// Correlates this failure with the request and events around it.
    pub correlation_id: CorrelationId,
    /// Reference to fuller local diagnostics, which are redacted separately.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub diagnostic_ref: Option<String>,
}

impl ErrorEnvelope {
    /// Build an envelope with `retryable` inferred from the code.
    #[must_use]
    pub fn new(code: ErrorCode, message: impl Into<String>, correlation_id: CorrelationId) -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            code,
            message: message.into(),
            retryable: code.is_retryable(),
            retry_after_ms: None,
            correlation_id,
            diagnostic_ref: None,
        }
    }
}

impl ErrorCode {
    /// Whether retrying the identical request could plausibly succeed.
    ///
    /// This is about the *category*, not a promise: a caller still needs an
    /// idempotency key before retrying anything with side effects.
    #[must_use]
    pub fn is_retryable(self) -> bool {
        matches!(
            self,
            Self::Conflict
                | Self::ProviderRateLimited
                | Self::ProviderUnavailable
                | Self::WorkerUnavailable
                | Self::Timeout
                | Self::PersistenceError
        )
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    reason = "tests assert on known-good literals; a panic is the failure report"
)]
mod tests {
    use super::*;

    #[test]
    fn error_codes_serialize_as_camel_case() {
        assert_eq!(
            serde_json::to_string(&ErrorCode::ProviderRateLimited).unwrap(),
            "\"providerRateLimited\""
        );
        assert_eq!(
            serde_json::to_string(&ErrorCode::InterruptedUnsafe).unwrap(),
            "\"interruptedUnsafe\""
        );
    }

    #[test]
    fn a_denied_request_is_not_advertised_as_retryable() {
        // Retrying a denial unchanged just asks the user the same question
        // again; the agent must change something or stop.
        for code in [
            ErrorCode::PermissionDenied,
            ErrorCode::PolicyBlocked,
            ErrorCode::ApprovalRequired,
            ErrorCode::Cancelled,
            ErrorCode::InvalidRequest,
        ] {
            assert!(!code.is_retryable(), "{code:?} must not be retryable");
        }
    }

    #[test]
    fn transient_failures_are_retryable() {
        for code in [
            ErrorCode::Timeout,
            ErrorCode::ProviderUnavailable,
            ErrorCode::ProviderRateLimited,
            ErrorCode::Conflict,
        ] {
            assert!(code.is_retryable(), "{code:?} should be retryable");
        }
    }

    #[test]
    fn optional_fields_are_omitted_when_absent() {
        let envelope = ErrorEnvelope::new(
            ErrorCode::Timeout,
            "the model did not respond within the deadline",
            CorrelationId::new("corr-1"),
        );
        let json = serde_json::to_value(&envelope).unwrap();
        let object = json.as_object().unwrap();
        assert!(!object.contains_key("retryAfterMs"));
        assert!(!object.contains_key("diagnosticRef"));
        assert_eq!(object["schemaVersion"], SCHEMA_VERSION);
        assert_eq!(object["retryable"], true);
    }
}
