//! RunPod Serverless over its REST API, serving `worker-vllm`.
//!
//! A lease is one serverless endpoint and the template it runs. The endpoint
//! is created with **zero always-on workers** and RunPod's own idle timeout
//! set to the spec's idle limit, so RunPod scales it to zero when Tramontana
//! is gone. Stopping deletes both: nothing is left that could bill.
//!
//! The model is served at `https://api.runpod.ai/v2/<endpoint>/openai/v1`,
//! which `OpenAiCompatibleProvider` reaches as an ordinary cloud endpoint.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use async_trait::async_trait;
use reqwest::{Method, StatusCode};
use serde::Deserialize;
use serde_json::{Value, json};
use tramontana_core::contracts::{
    ComputeLease, ComputeSpec, CorrelationId, ErrorCode, ErrorEnvelope, LeaseCost, LeaseId,
    LeaseStatus, ProviderConfig, ProviderId, SecretRef, is_loopback_url,
};
use tramontana_core::ports::{ComputeProvisioner, SecretResolver};

/// RunPod's control API.
const REST_BASE: &str = "https://rest.runpod.io/v1";
/// Where RunPod serves endpoints.
const SERVING_BASE: &str = "https://api.runpod.ai/v2";
/// RunPod accepts an idle timeout of 1 to 3600 seconds.
const IDLE_TIMEOUT_SECS: std::ops::RangeInclusive<u32> = 1..=3_600;

/// What this session started: the model each lease serves and the rate the
/// person accepted for it.
///
/// RunPod's REST API reports what an endpoint has cost but not its price per
/// hour, so the rate is the one the person accepted when starting it. A
/// lease this session did not start has no known rate, and its cost reads as
/// an error — callers fail closed on that.
#[derive(Debug, Clone)]
struct Started {
    model_id: String,
    rate: u64,
}

/// Leases on RunPod Serverless, in the person's own account.
pub struct RunPodServerless {
    client: reqwest::Client,
    rest_base: String,
    serving_base: String,
    credential: SecretRef,
    secrets: Arc<dyn SecretResolver>,
    started: Mutex<HashMap<LeaseId, Started>>,
}

impl std::fmt::Debug for RunPodServerless {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RunPodServerless")
            .field("rest_base", &self.rest_base)
            .finish_non_exhaustive()
    }
}

fn error(code: ErrorCode, message: impl Into<String>) -> ErrorEnvelope {
    ErrorEnvelope::new(code, message, CorrelationId::new("runpod"))
}

/// `https://`, or plain `http://` to loopback only (ADR 0009): the API key
/// rides on every request.
fn check_base(url: &str) -> Result<String, ErrorEnvelope> {
    if url.starts_with("https://") || (url.starts_with("http://") && is_loopback_url(url)) {
        Ok(url.trim_end_matches('/').to_owned())
    } else {
        Err(error(
            ErrorCode::InvalidRequest,
            format!("RunPod must be reached over https, got {url:?}"),
        ))
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Created {
    id: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Endpoint {
    #[serde(default)]
    template_id: Option<String>,
    #[serde(default)]
    workers_min: u32,
}

#[derive(Deserialize)]
struct Billed {
    #[serde(default)]
    amount: f64,
}

impl RunPodServerless {
    /// RunPod, with the person's API key by reference.
    ///
    /// # Errors
    ///
    /// [`ErrorCode::Internal`] when an HTTP client cannot be built.
    pub fn new(
        credential: SecretRef,
        secrets: Arc<dyn SecretResolver>,
    ) -> Result<Self, ErrorEnvelope> {
        Self::with_bases(REST_BASE, SERVING_BASE, credential, secrets)
    }

    /// RunPod at other addresses: a test double on loopback.
    ///
    /// # Errors
    ///
    /// [`ErrorCode::InvalidRequest`] for a base that is neither `https://`
    /// nor loopback, or [`ErrorCode::Internal`] when no client can be built.
    pub fn with_bases(
        rest_base: &str,
        serving_base: &str,
        credential: SecretRef,
        secrets: Arc<dyn SecretResolver>,
    ) -> Result<Self, ErrorEnvelope> {
        let client = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(60))
            // A redirect would carry the API key to a host nobody named.
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|failure| {
                error(
                    ErrorCode::Internal,
                    format!("could not build an HTTP client: {failure}"),
                )
            })?;
        Ok(Self {
            client,
            rest_base: check_base(rest_base)?,
            serving_base: check_base(serving_base)?,
            credential,
            secrets,
            started: Mutex::new(HashMap::new()),
        })
    }

    fn started(&self) -> MutexGuard<'_, HashMap<LeaseId, Started>> {
        self.started
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// One call to the control API. `Ok(None)` is a 404.
    async fn call(
        &self,
        method: Method,
        path: &str,
        body: Option<Value>,
    ) -> Result<Option<Value>, ErrorEnvelope> {
        let key = self.secrets.resolve(&self.credential).await?;
        let mut request = self
            .client
            .request(method, format!("{}{path}", self.rest_base))
            .bearer_auth(key);
        if let Some(body) = body {
            request = request.json(&body);
        }
        let response = request.send().await.map_err(|failure| {
            // The URL is ours and carries no secret; the key is a header.
            error(
                ErrorCode::ProviderUnavailable,
                format!("RunPod could not be reached: {failure}"),
            )
        })?;
        let status = response.status();
        if status == StatusCode::NOT_FOUND {
            return Ok(None);
        }
        // The body is not echoed: a provider error can quote the request.
        let code = match status {
            s if s.is_success() => {
                if status == StatusCode::NO_CONTENT {
                    return Ok(Some(Value::Null));
                }
                let text = response.text().await.map_err(|failure| {
                    error(
                        ErrorCode::ProviderUnavailable,
                        format!("RunPod's answer could not be read: {failure}"),
                    )
                })?;
                if text.trim().is_empty() {
                    return Ok(Some(Value::Null));
                }
                return serde_json::from_str(&text).map(Some).map_err(|_| {
                    error(
                        ErrorCode::ProtocolError,
                        "RunPod answered with something other than JSON",
                    )
                });
            }
            StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => ErrorCode::ProviderAuthentication,
            StatusCode::TOO_MANY_REQUESTS => ErrorCode::ProviderRateLimited,
            s if s.is_client_error() => ErrorCode::InvalidRequest,
            _ => ErrorCode::ProviderUnavailable,
        };
        Err(error(code, format!("RunPod refused {path}: HTTP {status}")))
    }

    async fn must(
        &self,
        method: Method,
        path: &str,
        body: Option<Value>,
    ) -> Result<Value, ErrorEnvelope> {
        self.call(method, path, body)
            .await?
            .ok_or_else(|| error(ErrorCode::NotFound, format!("RunPod has no {path}")))
    }

    async fn endpoint(&self, lease: &LeaseId) -> Result<Option<Endpoint>, ErrorEnvelope> {
        self.call(Method::GET, &format!("/endpoints/{lease}"), None)
            .await?
            .map(|value| {
                serde_json::from_value(value).map_err(|_| {
                    error(
                        ErrorCode::ProtocolError,
                        "RunPod described an endpoint oddly",
                    )
                })
            })
            .transpose()
    }
}

fn parse<T: for<'de> Deserialize<'de>>(value: Value, what: &str) -> Result<T, ErrorEnvelope> {
    serde_json::from_value(value).map_err(|_| {
        error(
            ErrorCode::ProtocolError,
            format!("RunPod's {what} was not as expected"),
        )
    })
}

#[async_trait]
impl ComputeProvisioner for RunPodServerless {
    fn id(&self) -> ProviderId {
        ProviderId::new("runpod")
    }

    async fn provision(&self, spec: &ComputeSpec) -> Result<ComputeLease, ErrorEnvelope> {
        let name = format!("tramontana-{}", uuid::Uuid::new_v4());
        let template: Created = parse(
            self.must(
                Method::POST,
                "/templates",
                Some(json!({
                    "name": name,
                    "imageName": spec.image,
                    "isServerless": true,
                    "env": { "MODEL_NAME": spec.model_id },
                })),
            )
            .await?,
            "template",
        )?;

        let mut body = json!({
            "name": name,
            "templateId": template.id,
            "gpuTypeIds": spec.gpu_types,
            // Never an always-on worker: idle must cost nothing (ADR 0072 §2).
            "workersMin": 0,
            "workersMax": 1,
            // RunPod's own scale-to-zero: holds when Tramontana is gone.
            "idleTimeout": spec.idle_limit_secs.clamp(*IDLE_TIMEOUT_SECS.start(), *IDLE_TIMEOUT_SECS.end()),
        });
        if let Some(region) = &spec.region {
            body["dataCenterIds"] = json!([region]);
        }
        let created = match self.must(Method::POST, "/endpoints", Some(body)).await {
            Ok(value) => parse::<Created>(value, "endpoint"),
            Err(failure) => Err(failure),
        };
        let endpoint = match created {
            Ok(endpoint) => endpoint,
            Err(failure) => {
                // Free, but not ours to leave behind.
                let _ = self
                    .call(Method::DELETE, &format!("/templates/{}", template.id), None)
                    .await;
                return Err(failure);
            }
        };

        let lease_id = LeaseId::new(endpoint.id);
        self.started().insert(
            lease_id.clone(),
            Started {
                model_id: spec.model_id.clone(),
                rate: spec.max_rate_micro_usd_per_hour,
            },
        );
        Ok(ComputeLease {
            lease_id,
            provider_id: self.id(),
            rate_micro_usd_per_hour: spec.max_rate_micro_usd_per_hour,
        })
    }

    async fn status(&self, lease: &LeaseId) -> Result<LeaseStatus, ErrorEnvelope> {
        let Some(endpoint) = self.endpoint(lease).await? else {
            return Ok(LeaseStatus::Stopped);
        };
        let Some(started) = self.started().get(lease).cloned() else {
            return Err(error(
                ErrorCode::NotFound,
                "This endpoint was not started by this session",
            ));
        };
        // A serverless endpoint takes requests as soon as it exists; the
        // first one waits for a worker's cold start.
        Ok(LeaseStatus::Ready {
            endpoint: ProviderConfig::OpenAiCompatible {
                base_url: format!("{}/{lease}/openai", self.serving_base),
                model_id: started.model_id,
                credential: Some(self.credential.clone()),
            },
            always_on_workers: endpoint.workers_min,
        })
    }

    async fn stop(&self, lease: &LeaseId) -> Result<(), ErrorEnvelope> {
        let Some(endpoint) = self.endpoint(lease).await? else {
            return Ok(());
        };
        let path = format!("/endpoints/{lease}");
        // Scale to nothing first, so a delete RunPod refuses still leaves
        // nothing running.
        self.call(
            Method::PATCH,
            &path,
            Some(json!({ "workersMin": 0, "workersMax": 0 })),
        )
        .await?;
        // Terminate, never merely stop: nothing left that bills (§3).
        self.call(Method::DELETE, &path, None).await?;
        if let Some(template) = endpoint.template_id {
            let _ = self
                .call(Method::DELETE, &format!("/templates/{template}"), None)
                .await;
        }
        Ok(())
    }

    async fn cost(&self, lease: &LeaseId) -> Result<LeaseCost, ErrorEnvelope> {
        let Some(rate) = self.started().get(lease).map(|started| started.rate) else {
            return Err(error(
                ErrorCode::NotFound,
                "This endpoint's rate is not known to this session",
            ));
        };
        let records: Vec<Billed> = parse(
            self.must(
                Method::GET,
                &format!("/billing/endpoints?endpointId={lease}&bucketSize=day"),
                None,
            )
            .await?,
            "billing",
        )?;
        let dollars: f64 = records.iter().map(|record| record.amount).sum();
        if !dollars.is_finite() || dollars < 0.0 {
            return Err(error(
                ErrorCode::ProtocolError,
                "RunPod's billing made no sense",
            ));
        }
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "checked finite and non-negative; `as` saturates at the top"
        )]
        let spent_micro_usd = (dollars * 1_000_000.0).ceil() as u64;
        Ok(LeaseCost {
            spent_micro_usd,
            rate_micro_usd_per_hour: rate,
        })
    }
}
