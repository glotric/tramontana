//! The RunPod Serverless adapter, against a pretend RunPod on loopback.
//!
//! Offline and free: no request leaves this machine. What the real RunPod
//! does with these requests is not tested here — see
//! `docs/evidence/0072-rented-gpus-serverless.md`.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "tests assert on known-good values; a panic is the failure report"
)]

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;
use tramontana_adapters_compute::RunPodServerless;
use tramontana_adapters_model::{OpenAiCompatibleProvider, ProviderAuth};
use tramontana_core::contracts::{
    ComputeSpec, ErrorCode, ErrorEnvelope, Health, LeaseId, LeaseStatus, ModelCapability,
    ModelDescriptor, ModelEvent, ModelLocation, ProviderConfig, SecretRef, SecretStore,
};
use tramontana_core::ports::{ComputeProvisioner, ModelProvider, SecretResolver};
use tramontana_core::testing::contract_suites::model_request;
use tramontana_core::testing::release::locally_authorized;

const KEY: &str = "rp_synthetic_key_for_tests";

#[derive(Debug)]
struct FixedSecret;

#[async_trait]
impl SecretResolver for FixedSecret {
    async fn resolve(&self, _reference: &SecretRef) -> Result<String, ErrorEnvelope> {
        Ok(KEY.to_owned())
    }
}

fn credential() -> SecretRef {
    SecretRef {
        key: "runpod.apiKey".to_owned(),
        store: SecretStore::Platform,
    }
}

#[derive(Debug, Clone)]
struct Seen {
    method: String,
    path: String,
    authorization: String,
    body: Value,
}

/// A pretend RunPod: control API and serving endpoint on one socket.
#[derive(Debug, Default)]
struct Pretend {
    seen: Vec<Seen>,
    endpoints: HashMap<String, Value>,
    /// Status for every request on a path with this prefix.
    fail: Option<(String, u16)>,
    workers_min: u32,
}

impl Pretend {
    fn answer(&mut self, method: &str, path: &str, body: &Value) -> (u16, String) {
        if let Some((prefix, status)) = &self.fail
            && path.starts_with(prefix.as_str())
        {
            return (
                *status,
                r#"{"error":"echo rp_synthetic_key_for_tests"}"#.to_owned(),
            );
        }
        match (method, path.strip_prefix("/v1").unwrap_or(path)) {
            ("POST", "/templates") => (200, json!({ "id": "tpl-1" }).to_string()),
            ("POST", "/endpoints") => {
                let endpoint = json!({
                    "id": "ep-1",
                    "templateId": body["templateId"],
                    "workersMin": self.workers_min,
                });
                self.endpoints.insert("ep-1".to_owned(), endpoint.clone());
                (200, endpoint.to_string())
            }
            ("GET", "/endpoints/ep-1") => self
                .endpoints
                .get("ep-1")
                .map_or((404, String::new()), |endpoint| (200, endpoint.to_string())),
            ("PATCH", "/endpoints/ep-1") => (200, "{}".to_owned()),
            ("DELETE", "/endpoints/ep-1") => {
                self.endpoints.remove("ep-1");
                (204, String::new())
            }
            ("DELETE", "/templates/tpl-1") => (204, String::new()),
            ("GET", billing) if billing.starts_with("/billing/endpoints?endpointId=ep-1") => (
                200,
                json!([{ "amount": 0.0125, "endpointId": "ep-1" }, { "amount": 0.5 }]).to_string(),
            ),
            _ => (404, String::new()),
        }
    }
}

async fn serve(pretend: Arc<Mutex<Pretend>>) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move {
        loop {
            let (mut socket, _) = listener.accept().await.unwrap();
            let pretend = pretend.clone();
            tokio::spawn(async move {
                let mut raw = Vec::new();
                let mut buffer = [0_u8; 4096];
                let (head, body) = loop {
                    let read = socket.read(&mut buffer).await.unwrap();
                    raw.extend_from_slice(&buffer[..read]);
                    let text = String::from_utf8_lossy(&raw).to_string();
                    if let Some((head, rest)) = text.split_once("\r\n\r\n") {
                        let length = head
                            .lines()
                            .find_map(|line| {
                                let (name, value) = line.split_once(':')?;
                                name.eq_ignore_ascii_case("content-length")
                                    .then(|| value.trim().parse::<usize>().unwrap())
                            })
                            .unwrap_or(0);
                        if rest.len() >= length {
                            break (head.to_owned(), rest.to_owned());
                        }
                    }
                };
                let mut request = head.lines().next().unwrap().split(' ');
                let method = request.next().unwrap().to_owned();
                let path = request.next().unwrap().to_owned();
                let authorization = head
                    .lines()
                    .find_map(|line| {
                        let (name, value) = line.split_once(':')?;
                        name.eq_ignore_ascii_case("authorization")
                            .then(|| value.trim().to_owned())
                    })
                    .unwrap_or_default();
                let body: Value = serde_json::from_str(&body).unwrap_or(Value::Null);

                if path.ends_with("/openai/v1/chat/completions") {
                    let frame = json!({ "choices": [{ "delta": { "content": "hi" }, "finish_reason": null }] });
                    let finish = json!({ "choices": [{ "delta": {}, "finish_reason": "stop" }] });
                    pretend.lock().unwrap().seen.push(Seen {
                        method,
                        path,
                        authorization,
                        body,
                    });
                    let reply = format!(
                        "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\nconnection: close\r\n\r\ndata: {frame}\n\ndata: {finish}\n\ndata: [DONE]\n\n"
                    );
                    socket.write_all(reply.as_bytes()).await.unwrap();
                    return;
                }
                let (status, reply) = {
                    let mut pretend = pretend.lock().unwrap();
                    let answer = pretend.answer(&method, &path, &body);
                    pretend.seen.push(Seen {
                        method,
                        path,
                        authorization,
                        body,
                    });
                    answer
                };
                let reply = format!(
                    "HTTP/1.1 {status} X\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{reply}",
                    reply.len()
                );
                socket.write_all(reply.as_bytes()).await.unwrap();
            });
        }
    });
    base
}

fn spec() -> ComputeSpec {
    ComputeSpec {
        gpu_types: vec!["NVIDIA H100 80GB HBM3".to_owned()],
        image: "runpod/worker-v1-vllm:v2.0.0".to_owned(),
        model_id: "Qwen/Qwen3-32B".to_owned(),
        region: Some("EU-RO-1".to_owned()),
        max_rate_micro_usd_per_hour: 2_700_000,
        idle_limit_secs: 90,
    }
}

async fn runpod() -> (RunPodServerless, Arc<Mutex<Pretend>>, String) {
    let pretend = Arc::new(Mutex::new(Pretend::default()));
    let base = serve(pretend.clone()).await;
    let runpod = RunPodServerless::with_bases(
        &format!("{base}/v1"),
        &format!("{base}/v2"),
        credential(),
        Arc::new(FixedSecret),
    )
    .unwrap();
    (runpod, pretend, base)
}

fn seen(pretend: &Mutex<Pretend>) -> Vec<Seen> {
    pretend.lock().unwrap().seen.clone()
}

#[tokio::test]
async fn an_endpoint_is_created_with_no_always_on_worker_and_runpods_own_idle_limit() {
    let (runpod, pretend, _) = runpod().await;

    let lease = runpod.provision(&spec()).await.unwrap();

    assert_eq!(lease.lease_id, LeaseId::new("ep-1"));
    assert_eq!(lease.rate_micro_usd_per_hour, 2_700_000);
    let seen = seen(&pretend);
    let template = &seen[0];
    assert_eq!(
        (template.method.as_str(), template.path.as_str()),
        ("POST", "/v1/templates")
    );
    assert_eq!(template.authorization, format!("Bearer {KEY}"));
    assert_eq!(template.body["imageName"], "runpod/worker-v1-vllm:v2.0.0");
    assert_eq!(template.body["isServerless"], true);
    assert_eq!(template.body["env"]["MODEL_NAME"], "Qwen/Qwen3-32B");
    let endpoint = &seen[1];
    assert_eq!(endpoint.path, "/v1/endpoints");
    assert_eq!(endpoint.body["workersMin"], 0);
    assert_eq!(endpoint.body["idleTimeout"], 90);
    assert_eq!(endpoint.body["templateId"], "tpl-1");
    assert_eq!(endpoint.body["dataCenterIds"], json!(["EU-RO-1"]));
}

#[tokio::test]
async fn a_ready_endpoint_is_served_by_the_existing_openai_compatible_adapter() {
    let (runpod, pretend, base) = runpod().await;
    let lease = runpod.provision(&spec()).await.unwrap();

    let LeaseStatus::Ready {
        endpoint:
            ProviderConfig::OpenAiCompatible {
                base_url,
                model_id,
                credential: Some(reference),
            },
        always_on_workers: 0,
    } = runpod.status(&lease.lease_id).await.unwrap()
    else {
        panic!("not ready as expected");
    };
    assert_eq!(base_url, format!("{base}/v2/ep-1/openai"));

    let model = OpenAiCompatibleProvider::new(
        "runpod",
        base_url,
        ModelDescriptor {
            provider_id: "runpod".to_owned(),
            model_id: model_id.clone(),
            display_name: model_id,
            location: ModelLocation::Cloud,
            capabilities: vec![ModelCapability::Text, ModelCapability::Streaming],
            context_limit: None,
            health: Health::Available,
            health_reason_code: None,
        },
        ProviderAuth::Bearer {
            reference,
            resolver: Arc::new(FixedSecret),
        },
    )
    .unwrap();
    let mut events = model
        .stream(
            locally_authorized(model_request("r1")),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    let mut text = String::new();
    while let Some(event) = events.recv().await {
        if let ModelEvent::TextDelta { text: delta, .. } = event {
            text.push_str(&delta);
        }
    }
    assert_eq!(text, "hi");
    let call = seen(&pretend).into_iter().last().unwrap();
    assert_eq!(call.path, "/v2/ep-1/openai/v1/chat/completions");
    assert_eq!(call.authorization, format!("Bearer {KEY}"));
}

#[tokio::test]
async fn always_on_workers_set_in_the_console_are_reported() {
    let (runpod, pretend, _) = runpod().await;
    pretend.lock().unwrap().workers_min = 2;
    let lease = runpod.provision(&spec()).await.unwrap();

    let LeaseStatus::Ready {
        always_on_workers, ..
    } = runpod.status(&lease.lease_id).await.unwrap()
    else {
        panic!("not ready");
    };
    assert_eq!(always_on_workers, 2);
}

#[tokio::test]
async fn stop_terminates_the_endpoint_and_its_template_and_is_idempotent() {
    let (runpod, pretend, _) = runpod().await;
    let lease = runpod.provision(&spec()).await.unwrap();
    pretend.lock().unwrap().seen.clear();

    runpod.stop(&lease.lease_id).await.unwrap();
    let calls: Vec<_> = seen(&pretend)
        .into_iter()
        .map(|seen| format!("{} {}", seen.method, seen.path))
        .collect();
    assert_eq!(
        calls,
        [
            "GET /v1/endpoints/ep-1",
            "PATCH /v1/endpoints/ep-1",
            "DELETE /v1/endpoints/ep-1",
            "DELETE /v1/templates/tpl-1",
        ]
    );
    assert_eq!(
        runpod.status(&lease.lease_id).await.unwrap(),
        LeaseStatus::Stopped
    );

    pretend.lock().unwrap().seen.clear();
    runpod.stop(&lease.lease_id).await.unwrap();
    assert_eq!(seen(&pretend).len(), 1, "a second stop only looks");
}

#[tokio::test]
async fn cost_is_runpods_billing_rounded_up_to_micro_dollars() {
    let (runpod, _, _) = runpod().await;
    let lease = runpod.provision(&spec()).await.unwrap();

    let cost = runpod.cost(&lease.lease_id).await.unwrap();

    assert_eq!(cost.spent_micro_usd, 512_500);
    assert_eq!(cost.rate_micro_usd_per_hour, 2_700_000);
}

#[tokio::test]
async fn cost_that_cannot_be_read_is_an_error_not_an_estimate() {
    let (runpod, pretend, _) = runpod().await;
    let lease = runpod.provision(&spec()).await.unwrap();
    pretend.lock().unwrap().fail = Some(("/v1/billing".to_owned(), 503));

    assert!(runpod.cost(&lease.lease_id).await.is_err());
    // A lease this session did not start has no known rate.
    assert_eq!(
        runpod
            .cost(&LeaseId::new("ep-other"))
            .await
            .unwrap_err()
            .code,
        ErrorCode::NotFound
    );
}

#[tokio::test]
async fn a_failed_endpoint_leaves_no_template_behind() {
    let (runpod, pretend, _) = runpod().await;
    pretend.lock().unwrap().fail = Some(("/v1/endpoints".to_owned(), 500));

    let failure = runpod.provision(&spec()).await.unwrap_err();

    assert_eq!(failure.code, ErrorCode::ProviderUnavailable);
    let last = seen(&pretend).into_iter().last().unwrap();
    assert_eq!(
        (last.method.as_str(), last.path.as_str()),
        ("DELETE", "/v1/templates/tpl-1")
    );
}

#[tokio::test]
async fn a_refused_key_is_an_authentication_error_that_does_not_echo_anything() {
    let (runpod, pretend, _) = runpod().await;
    pretend.lock().unwrap().fail = Some(("/v1/templates".to_owned(), 401));

    let failure = runpod.provision(&spec()).await.unwrap_err();

    assert_eq!(failure.code, ErrorCode::ProviderAuthentication);
    assert!(!failure.message.contains(KEY));
}

#[test]
fn plain_http_to_anything_but_loopback_is_refused() {
    let refused = RunPodServerless::with_bases(
        "http://rest.runpod.io/v1",
        "https://api.runpod.ai/v2",
        credential(),
        Arc::new(FixedSecret),
    )
    .unwrap_err();
    assert_eq!(refused.code, ErrorCode::InvalidRequest);
}
