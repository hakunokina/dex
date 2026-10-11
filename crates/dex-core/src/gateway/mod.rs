//! Tokensız protocol contract spike; no Core/CLI/credential-store integration.
//! Translation design adapted from raine/claude-code-proxy v0.1.44 (4b5c058).
//! Copyright (c) 2026 Raine Virta; see LICENSE-raine and README.md.

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{
    atomic::{AtomicBool, AtomicU64, Ordering},
    Arc, Mutex,
};

pub mod auth;
pub(crate) mod ingress;
mod request;
mod stream;
mod transport;
use std::io::Read;
const MAX_BYTES: usize = 8 * 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Protocol {
    AnthropicMessages,
    OpenaiResponses,
    OpenaiChat,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Profile {
    ApiKey,
    SiwcAdditionalTools,
    SiwcNamespace,
}

// Deliberately neither Debug nor Serialize: credentials stay in memory.
pub enum UpstreamAuth {
    Bearer(String),
    ApiKey(String),
    SiwcAccount(String),
}
pub enum Endpoint {
    Openai,
    Loopback(SocketAddr),
}
pub struct Route {
    pub endpoint: Endpoint,
    pub protocol: Protocol,
    pub profile: Profile,
    pub alias: String,
    pub model: String,
    pub provider: String,
    pub account: String,
    pub auth: UpstreamAuth,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorKind {
    Unauthorized,
    Revoked,
    Cancelled,
    InvalidRequest,
    Unsupported,
    Quota,
    RateLimit,
    Unavailable,
    UpstreamAuth,
    Incomplete,
    Eof,
    Protocol,
    Transport,
    Limit,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RequestContext {
    pub request_id: u64,
    pub provider: String,
    pub account: String,
    pub model: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Fault {
    pub kind: ErrorKind,
    pub status: Option<u16>,
    pub retry_after_seconds: Option<u64>,
    pub context: Option<RequestContext>,
}
impl Fault {
    fn new(kind: ErrorKind) -> Self {
        Self {
            kind,
            status: None,
            retry_after_seconds: None,
            context: None,
        }
    }
    fn event(&self) -> Value {
        let kind = match self.kind {
            ErrorKind::Quota | ErrorKind::RateLimit => "rate_limit_error",
            ErrorKind::Unavailable => "overloaded_error",
            ErrorKind::UpstreamAuth | ErrorKind::Unauthorized | ErrorKind::Revoked => {
                "authentication_error"
            }
            ErrorKind::InvalidRequest | ErrorKind::Unsupported | ErrorKind::Limit => {
                "invalid_request_error"
            }
            _ => "api_error",
        };
        let message = if self.kind == ErrorKind::Quota {
            "gateway usage limit exceeded"
        } else {
            "gateway request failed"
        };
        json!({"type":"error", "error":{"type":kind, "message":message, "gateway":{"kind":self.kind,"status":self.status,"retry_after_seconds":self.retry_after_seconds}}})
    }
}

// Payload-bearing results deliberately have no Debug/Serialize implementation.
pub struct Reply {
    pub message: Value,
}
struct Grant {
    route: Route,
    revoked: AtomicBool,
}
#[derive(Default)]
pub struct Gateway {
    grants: Mutex<HashMap<String, Arc<Grant>>>,
    next_request: AtomicU64,
    auth: Option<Arc<auth::Auth>>,
}
impl Gateway {
    fn with_auth(auth: Arc<auth::Auth>) -> Self {
        Self {
            auth: Some(auth),
            ..Default::default()
        }
    }
    pub fn revoke_all(&self) {
        let mut grants = self.grants.lock().unwrap_or_else(|e| e.into_inner());
        for grant in grants.values() {
            grant.revoked.store(true, Ordering::SeqCst);
        }
        grants.clear();
    }
    pub fn register(&self, local_token: String, route: Route) -> Result<(), Fault> {
        if matches!(route.endpoint, Endpoint::Loopback(address) if !address.ip().is_loopback())
            || (matches!(route.endpoint, Endpoint::Openai)
                && (route.protocol != Protocol::OpenaiResponses
                    || route.profile == Profile::ApiKey))
        {
            return Err(Fault::new(ErrorKind::Unsupported));
        }
        if (route.protocol != Protocol::AnthropicMessages
            && !matches!(
                route.auth,
                UpstreamAuth::Bearer(_) | UpstreamAuth::SiwcAccount(_)
            ))
            || (route.profile != Profile::ApiKey && route.protocol != Protocol::OpenaiResponses)
        {
            return Err(Fault::new(ErrorKind::Unsupported));
        }
        let mut grants = self
            .grants
            .lock()
            .map_err(|_| Fault::new(ErrorKind::Unavailable))?;
        if local_token.is_empty() || grants.contains_key(&local_token) {
            return Err(Fault::new(ErrorKind::InvalidRequest));
        }
        grants.insert(
            local_token,
            Arc::new(Grant {
                route,
                revoked: AtomicBool::new(false),
            }),
        );
        Ok(())
    }
    pub fn revoke(&self, local_token: &str) {
        // Poisoned registration/lookup fails closed, but revocation must still
        // invalidate references already held by an in-flight request.
        let mut grants = self
            .grants
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(grant) = grants.remove(local_token) {
            grant.revoked.store(true, Ordering::SeqCst);
        }
    }
    pub fn execute(
        &self,
        local_token: &str,
        request: &Value,
        headers: &[(&str, &str)],
        mut emit: impl FnMut(Value),
    ) -> Result<Reply, Fault> {
        self.execute_cancellable(local_token, request, headers, |event| {
            emit(event);
            true
        })
    }
    // A closed downstream cancels only this request, never its shared grant.
    pub(super) fn execute_cancellable(
        &self,
        local_token: &str,
        request: &Value,
        headers: &[(&str, &str)],
        mut emit: impl FnMut(Value) -> bool,
    ) -> Result<Reply, Fault> {
        let result = (|| {
            let grant = self
                .grants
                .lock()
                .map_err(|_| Fault::new(ErrorKind::Unavailable))?
                .get(local_token)
                .cloned()
                .ok_or_else(|| Fault::new(ErrorKind::Unauthorized))?;
            let route = &grant.route;
            let context = RequestContext {
                request_id: self.next_request.fetch_add(1, Ordering::SeqCst) + 1,
                provider: route.provider.clone(),
                account: route.account.clone(),
                model: route.model.clone(),
            };
            self.run(&grant, request, headers, &mut emit)
                .map_err(|mut fault| {
                    fault.context = Some(context);
                    fault
                })
        })();
        if let Err(fault) = &result {
            if fault.kind == ErrorKind::UpstreamAuth {
                if let (Some(auth), Some(context)) = (&self.auth, &fault.context) {
                    if auth.invalidate(&context.account).is_err() {
                        log::error!("gateway auth invalidation failed");
                    }
                }
            }
            if fault.kind != ErrorKind::Cancelled {
                emit(fault.event());
            }
        }
        result
    }
    fn run(
        &self,
        grant: &Grant,
        request: &Value,
        headers: &[(&str, &str)],
        emit: &mut impl FnMut(Value) -> bool,
    ) -> Result<Reply, Fault> {
        if grant.revoked.load(Ordering::SeqCst) {
            return Err(Fault::new(ErrorKind::Revoked));
        }
        let route = &grant.route;
        if request["model"] != route.alias {
            return Err(Fault::new(ErrorKind::InvalidRequest));
        }
        if request.get("stream").is_some_and(|s| !s.is_boolean()) {
            return Err(Fault::new(ErrorKind::InvalidRequest));
        }
        let body = request::translate(route, request)?;
        let downstream_stream = request["stream"].as_bool().unwrap_or(false);
        let path = match route.protocol {
            Protocol::AnthropicMessages => "/v1/messages",
            Protocol::OpenaiResponses => "/v1/responses",
            Protocol::OpenaiChat => "/v1/chat/completions",
        };
        let access = match &route.auth {
            UpstreamAuth::SiwcAccount(account) => Some(
                self.auth
                    .as_ref()
                    .ok_or_else(|| Fault::new(ErrorKind::UpstreamAuth))?
                    .access(account)
                    .map_err(|code| {
                        Fault::new(match code.as_str() {
                            "gateway_auth_transport" => ErrorKind::Transport,
                            "gateway_auth_rate_limited" => ErrorKind::RateLimit,
                            "gateway_auth_unavailable"
                            | "gateway_secret_unavailable"
                            | "gateway_secret_write_failed" => ErrorKind::Unavailable,
                            "gateway_account_inactive" => ErrorKind::Revoked,
                            "gateway_login_required"
                            | "gateway_account_not_found"
                            | "gateway_account_ineligible"
                            | "gateway_auth_rejected" => ErrorKind::UpstreamAuth,
                            _ => ErrorKind::Protocol,
                        })
                    })?,
            ),
            _ => None,
        };
        if grant.revoked.load(Ordering::SeqCst) {
            return Err(Fault::new(ErrorKind::Revoked));
        }
        let response = transport::post(route, path, &body, headers, access.as_deref())?;
        if response.status != 200 {
            let status = response.status;
            let retry = response
                .headers
                .get("retry-after")
                .and_then(|s| s.parse().ok());
            let mut bytes = vec![];
            response
                .reader
                .take((MAX_BYTES + 1) as u64)
                .read_to_end(&mut bytes)
                .map_err(|_| Fault::new(ErrorKind::Transport))?;
            let body = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
            return Err(classify(Some(status), &body, retry));
        }
        let native = route.protocol == Protocol::AnthropicMessages;
        let is_sse = response
            .headers
            .get("content-type")
            .is_some_and(|s| s.starts_with("text/event-stream"));
        let mut committed = false;
        let message = if native && !is_sse {
            let mut bytes = vec![];
            response
                .reader
                .take((MAX_BYTES + 1) as u64)
                .read_to_end(&mut bytes)
                .map_err(|_| Fault::new(ErrorKind::Transport))?;
            if bytes.len() > MAX_BYTES {
                return Err(Fault::new(ErrorKind::Limit));
            }
            let value: Value =
                serde_json::from_slice(&bytes).map_err(|_| Fault::new(ErrorKind::Protocol))?;
            if value["type"] != "message"
                || !value["content"].is_array()
                || !value["stop_reason"].is_string()
            {
                return Err(Fault::new(ErrorKind::Protocol));
            }
            if downstream_stream {
                return Err(Fault::new(ErrorKind::Protocol));
            }
            value
        } else {
            if !is_sse {
                return Err(Fault::new(ErrorKind::Protocol));
            }
            let mut frames = stream::Frames::new(std::io::BufReader::new(response.reader));
            let mut state = stream::Stream::new(route);
            let mut native_state = stream::Native::new();
            while let Some((_event, data)) = frames.next()? {
                if grant.revoked.load(Ordering::SeqCst) {
                    return Err(Fault::new(ErrorKind::Revoked));
                }
                let events = if native {
                    native_state.feed(&data)?
                } else {
                    state.feed(&data)?
                };
                if grant.revoked.load(Ordering::SeqCst) {
                    return Err(Fault::new(ErrorKind::Revoked));
                }
                // Successful completion is the linearization point. Revocation after it
                // applies to later requests, never adds an error after a success event.
                committed = if native {
                    native_state.complete
                } else {
                    state.complete
                };
                for event in events {
                    if !committed && grant.revoked.load(Ordering::SeqCst) {
                        return Err(Fault::new(ErrorKind::Revoked));
                    }
                    if downstream_stream && !emit(event) {
                        return Err(Fault::new(ErrorKind::Cancelled));
                    }
                }
                if committed {
                    break;
                }
            }
            if native {
                native_state.message()?
            } else {
                state.message()?
            }
        };
        if !committed && grant.revoked.load(Ordering::SeqCst) {
            return Err(Fault::new(ErrorKind::Revoked));
        }
        Ok(Reply { message })
    }
}

fn classify(status: Option<u16>, body: &Value, retry: Option<u64>) -> Fault {
    let error = body
        .get("error")
        .or_else(|| body.pointer("/response/error"))
        .unwrap_or(body);
    let status = status.or_else(|| {
        error["status_code"]
            .as_u64()
            .filter(|s| (400..=599).contains(s))
            .map(|s| s as u16)
    });
    let code = error["code"]
        .as_str()
        .or_else(|| error["type"].as_str())
        .unwrap_or("");
    let kind = match code {
        "insufficient_quota" | "subscription_sharing_usage_limit_exceeded" => ErrorKind::Quota,
        "rate_limit_exceeded" | "rate_limit_error" => ErrorKind::RateLimit,
        "subscription_sharing_usage_unavailable"
        | "server_error"
        | "overloaded_error"
        | "api_error" => ErrorKind::Unavailable,
        "invalid_request_error" => ErrorKind::InvalidRequest,
        "unsupported_capability" => ErrorKind::Unsupported,
        "user_not_eligible" | "invalid_api_key" | "authentication_error" => ErrorKind::UpstreamAuth,
        _ => match status {
            Some(429) => ErrorKind::RateLimit,
            Some(401 | 403) => ErrorKind::UpstreamAuth,
            Some(500..=599) => ErrorKind::Unavailable,
            _ => ErrorKind::Protocol,
        },
    };
    Fault {
        kind,
        status,
        retry_after_seconds: retry,
        context: None,
    }
}

#[cfg(test)]
mod tests;
