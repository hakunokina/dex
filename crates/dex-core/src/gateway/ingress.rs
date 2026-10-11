//! Core-owned HTTP/1 listener. Only literal loopback, bounded body and concurrent requests.
use super::*;
use bytes::Bytes;
use http_body_util::{combinators::UnsyncBoxBody, BodyExt, Full, Limited, StreamBody};
use hyper::{
    body::{Frame, Incoming},
    service::service_fn,
    Request, Response,
};
use hyper_util::rt::{TokioIo, TokioTimer};
use std::convert::Infallible;
use std::sync::Weak;
use tokio::sync::{mpsc, oneshot, Semaphore};

type Body = UnsyncBoxBody<Bytes, Infallible>;
#[derive(Clone)]
struct Lease {
    token: String,
    job: Option<i64>,
    account: String,
    model: String,
}
pub(crate) struct Service {
    pub(super) gateway: Arc<Gateway>,
    port: u16,
    stop: Mutex<Option<oneshot::Sender<()>>>,
    thread: Mutex<Option<std::thread::JoinHandle<()>>>,
    pub(crate) auth: Arc<auth::Auth>,
    core: Mutex<Weak<crate::Core>>,
    leases: Mutex<HashMap<String, Lease>>,
    wake: mpsc::Sender<()>,
    closed: AtomicBool,
    #[cfg(test)]
    test_endpoint: Mutex<Option<SocketAddr>>,
}
impl Service {
    pub(crate) fn start() -> Result<Arc<Self>, String> {
        let listener =
            std::net::TcpListener::bind("127.0.0.1:0").map_err(|_| "gateway_bind_failed")?;
        let port = listener
            .local_addr()
            .map_err(|_| "gateway_bind_failed")?
            .port();
        listener
            .set_nonblocking(true)
            .map_err(|_| "gateway_bind_failed")?;
        let (tx, mut rx) = oneshot::channel();
        let (wake, mut wakes) = mpsc::channel(1);
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|_| "gateway_runtime_failed")?;
        #[cfg(test)]
        let auth = Arc::new(auth::Auth::for_test());
        #[cfg(not(test))]
        let auth = Arc::new(auth::Auth::default());
        let svc = Arc::new(Self {
            gateway: Arc::new(Gateway::with_auth(auth.clone())),
            auth,
            port,
            stop: Mutex::new(Some(tx)),
            thread: Mutex::new(None),
            core: Mutex::new(Weak::new()),
            leases: Mutex::new(HashMap::new()),
            wake,
            closed: AtomicBool::new(false),
            #[cfg(test)]
            test_endpoint: Mutex::new(None),
        });
        let weak = Arc::downgrade(&svc);
        let thread = std::thread::spawn(move || {
            runtime.block_on(async move {
            let Ok(listener) = tokio::net::TcpListener::from_std(listener) else { return; };
            let slots = Arc::new(Semaphore::new(16));
            let mut connections = tokio::task::JoinSet::new();
            loop {
                tokio::select! {
                    _ = &mut rx => break,
                    _ = wakes.recv() => {
                        if let Some(svc)=weak.upgrade() { tokio::task::spawn_blocking(move || svc.revoke_completed()); }
                    },
                    accepted = listener.accept() => {
                        let Ok((stream,peer)) = accepted else { break; };
                        if !peer.ip().is_loopback() { continue; }
                        let Ok(permit) = slots.clone().try_acquire_owned() else { continue; };
                        let weak = weak.clone();
                        connections.spawn(async move {
                            let _permit = permit;
                            let service = service_fn(move |request| serve(weak.clone(),request));
                            let _ = hyper::server::conn::http1::Builder::new().keep_alive(false)
                                .timer(TokioTimer::new()).header_read_timeout(std::time::Duration::from_secs(10))
                                .max_buf_size(65536).serve_connection(TokioIo::new(stream),service).await;
                        });
                    },
                    _ = connections.join_next(), if !connections.is_empty() => {},
                }
            }
            connections.abort_all();
            while connections.join_next().await.is_some() {}
            // Blocking egress has its own 120s bound; shutdown never waits for it.
        });
            runtime.shutdown_background();
        });
        *svc.thread.lock().unwrap() = Some(thread);
        Ok(svc)
    }
    pub(crate) fn base_url(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }
    pub(crate) fn shutdown(&self) {
        self.closed.store(true, Ordering::SeqCst);
        self.auth.cancel();
        self.gateway.revoke_all();
        if let Some(tx) = self.stop.lock().unwrap().take() {
            let _ = tx.send(());
        }
        if let Some(thread) = self.thread.lock().unwrap().take() {
            if thread.thread().id() != std::thread::current().id() {
                let _ = thread.join();
            }
        }
    }
    pub(crate) fn is_open(&self) -> bool {
        !self.closed.load(Ordering::SeqCst)
    }
    pub(crate) fn bind(&self, core: Weak<crate::Core>) {
        if let Some(core) = core.upgrade() {
            self.auth.bind_events(core.events.clone());
        }
        *self.core.lock().unwrap() = core;
    }
    pub(crate) fn revoke_agent(&self, agent: &str) {
        if let Some(lease) = self.leases.lock().unwrap().remove(agent) {
            self.gateway.revoke(&lease.token);
        }
    }
    fn revoke_lease(&self, agent: &str, token: &str) {
        let mut leases = self.leases.lock().unwrap();
        if leases.get(agent).is_some_and(|l| l.token == token) {
            leases.remove(agent);
        }
        self.gateway.revoke(token);
    }
    pub(crate) fn revoke_all(&self) {
        self.leases.lock().unwrap().clear();
        self.gateway.revoke_all();
    }
    fn valid(&self, agent: &str, lease: &Lease) -> bool {
        let Some(core) = self.core.lock().unwrap().upgrade() else {
            return false;
        };
        crate::db::with(&core.db,|c| {
            use rusqlite::OptionalExtension;
            let job=c.query_row("SELECT delegation_id FROM agent_sessions WHERE agent_id=?1",[agent],|r|r.get::<_,Option<i64>>(0)).optional().map_err(|_| "gateway_session_invalid")?;
            if job!=Some(lease.job) {return Ok(false);}
            let a=crate::teams::get_agent(c,agent)?;
            if a.active_engine.as_deref().unwrap_or(&a.engine)!="claude" {return Ok(false);}
            if a.active_provider.as_deref().unwrap_or(&a.provider)!="gateway" || a.active_model.as_deref().unwrap_or(&a.model)!=lease.model {return Ok(false);}
            if let Some(job)=lease.job {
                return c.query_row("SELECT EXISTS(SELECT 1 FROM delegations WHERE id=?1 AND to_agent=?2 AND status IN ('queued','running'))",rusqlite::params![job,agent],|r|r.get(0)).map_err(|_|"gateway_job_invalid".into());
            }
            Ok(a.kind=="leader")
        }).unwrap_or(false)
    }
    fn valid_token(&self, token: &str) -> bool {
        let scoped = self
            .leases
            .lock()
            .unwrap()
            .iter()
            .find(|(_, l)| l.token == token)
            .map(|(a, l)| (a.clone(), l.clone()));
        if let Some((agent, lease)) = scoped {
            if self.valid(&agent, &lease) {
                return true;
            }
            self.revoke_lease(&agent, &lease.token);
            return false;
        }
        // Only standalone fake upstream tests register grants without a Core launch binding.
        cfg!(test)
    }
    fn revoke_completed(&self) {
        let leases = self.leases.lock().unwrap().clone();
        for (agent, lease) in leases {
            if !self.valid(&agent, &lease) {
                self.revoke_lease(&agent, &lease.token);
            }
        }
    }
    pub(crate) fn spawn_env(
        &self,
        core: &crate::Core,
        agent: &str,
        model: &str,
    ) -> Result<Vec<(String, String)>, String> {
        if !self.is_open() {
            return Err("gateway_closed".into());
        }
        let job = crate::db::with(&core.db, |c| {
            c.query_row(
                "SELECT delegation_id FROM agent_sessions WHERE agent_id=?1",
                [agent],
                |r| r.get::<_, Option<i64>>(0),
            )
            .map_err(|_| "gateway_session_invalid".into())
        })?;
        if !self.valid(
            agent,
            &Lease {
                token: String::new(),
                job,
                account: String::new(),
                model: model.into(),
            },
        ) {
            return Err("gateway_job_inactive".into());
        }
        let mut status = self.auth.status()?;
        let account = status.active_account_id.ok_or("gateway_login_required")?;
        if status.models.is_empty() {
            status.models = self.auth.models()?;
        }
        if !status.models.iter().any(|m| m.id == model) {
            return Err("gateway_model_unavailable".into());
        }
        let existing = self
            .leases
            .lock()
            .unwrap()
            .get(agent)
            .filter(|l| l.job == job && l.account == account && l.model == model)
            .cloned();
        let token = if let Some(lease) = existing {
            lease.token
        } else {
            self.revoke_agent(agent);
            let token = auth::random()?;
            #[cfg(test)]
            let endpoint = self
                .test_endpoint
                .lock()
                .unwrap()
                .map(Endpoint::Loopback)
                .unwrap_or(Endpoint::Openai);
            #[cfg(not(test))]
            let endpoint = Endpoint::Openai;
            let route = Route {
                endpoint,
                protocol: Protocol::OpenaiResponses,
                profile: Profile::SiwcAdditionalTools,
                alias: model.into(),
                model: model.into(),
                provider: "gateway".into(),
                account: account.clone(),
                auth: UpstreamAuth::SiwcAccount(account.clone()),
            };
            self.gateway
                .register(token.clone(), route)
                .map_err(|_| "gateway_register_failed")?;
            self.leases.lock().unwrap().insert(
                agent.into(),
                Lease {
                    token: token.clone(),
                    job,
                    account,
                    model: model.into(),
                },
            );
            token
        };
        Ok(crate::providers::gateway_env(
            &self.base_url(),
            &token,
            model,
        ))
    }
    #[cfg(test)]
    pub(super) fn endpoint_for_test(&self, address: SocketAddr) {
        assert!(address.ip().is_loopback());
        *self.test_endpoint.lock().unwrap() = Some(address);
    }
}
struct Tap {
    events: crate::Events,
    service: Weak<Service>,
}
impl crate::EventSink for Tap {
    fn emit(&self, event: &str, payload: Value) -> Result<(), String> {
        if event == "delegations-changed" || event == "teams-changed" {
            if let Some(svc) = self.service.upgrade() {
                let _ = svc.wake.try_send(());
            }
        }
        self.events.emit(event, &payload)
    }
}
pub(crate) fn tap(events: crate::Events, service: &Arc<Service>) -> crate::Events {
    crate::Events::new(Tap {
        events,
        service: Arc::downgrade(service),
    })
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn stale_completion_never_revokes_the_replacement_jobs_token() {
        let svc = Service::start().unwrap();
        svc.leases.lock().unwrap().insert(
            "agent".into(),
            Lease {
                token: "new".into(),
                job: Some(2),
                account: "account".into(),
                model: "model".into(),
            },
        );
        svc.revoke_lease("agent", "old");
        assert_eq!(svc.leases.lock().unwrap()["agent"].token, "new");
        svc.shutdown();
    }
}
impl Drop for Service {
    fn drop(&mut self) {
        self.shutdown();
    }
}
fn json_reply(status: u16, value: Value) -> Response<Body> {
    Response::builder()
        .status(status)
        .header("content-type", "application/json")
        .body(Full::new(Bytes::from(value.to_string())).boxed_unsync())
        .unwrap()
}
fn fault_reply(fault: Fault) -> Response<Body> {
    let status = match fault.kind {
        ErrorKind::Unauthorized | ErrorKind::Revoked | ErrorKind::UpstreamAuth => 401,
        ErrorKind::InvalidRequest | ErrorKind::Unsupported => 400,
        ErrorKind::Limit => 413,
        ErrorKind::Quota | ErrorKind::RateLimit => 429,
        ErrorKind::Unavailable => 529,
        _ => 502,
    };
    let mut reply = json_reply(status, fault.event());
    if let Some(retry) = fault.retry_after_seconds {
        reply
            .headers_mut()
            .insert("retry-after", retry.to_string().parse().unwrap());
    }
    reply
}
async fn serve(
    weak: Weak<Service>,
    request: Request<Incoming>,
) -> Result<Response<Body>, Infallible> {
    let Some(svc) = weak.upgrade() else {
        return Ok(fault_reply(Fault::new(ErrorKind::Unavailable)));
    };
    if request.method() != hyper::Method::POST || request.uri().path() != "/v1/messages" {
        return Ok(json_reply(
            404,
            json!({"type":"error","error":{"type":"not_found_error","message":"gateway route not found"}}),
        ));
    }
    let (parts, body) = request.into_parts();
    if parts.headers.contains_key("origin")
        || parts.headers.get("host").and_then(|h| h.to_str().ok())
            != Some(&format!("127.0.0.1:{}", svc.port))
    {
        return Ok(fault_reply(Fault::new(ErrorKind::Unauthorized)));
    }
    let token = parts
        .headers
        .get("x-api-key")
        .and_then(|h| h.to_str().ok())
        .or_else(|| {
            parts
                .headers
                .get("authorization")
                .and_then(|h| h.to_str().ok())
                .and_then(|h| h.strip_prefix("Bearer "))
        });
    let Some(token) = token
        .filter(|t| !t.is_empty() && t.len() <= 256)
        .map(str::to_owned)
    else {
        return Ok(fault_reply(Fault::new(ErrorKind::Unauthorized)));
    };
    if !svc.gateway.grants.lock().unwrap().contains_key(&token) {
        return Ok(fault_reply(Fault::new(ErrorKind::Unauthorized)));
    }
    if !svc.valid_token(&token) {
        return Ok(fault_reply(Fault::new(ErrorKind::Revoked)));
    }
    let bytes = match tokio::time::timeout(
        std::time::Duration::from_secs(10),
        Limited::new(body, MAX_BYTES).collect(),
    )
    .await
    {
        Ok(Ok(bytes)) => bytes.to_bytes(),
        _ => return Ok(fault_reply(Fault::new(ErrorKind::Limit))),
    };
    let Ok(input) = serde_json::from_slice::<Value>(&bytes) else {
        return Ok(fault_reply(Fault::new(ErrorKind::InvalidRequest)));
    };
    if !svc.valid_token(&token) {
        return Ok(fault_reply(Fault::new(ErrorKind::Revoked)));
    }
    let headers: Vec<_> = ["anthropic-version", "anthropic-beta"]
        .into_iter()
        .filter_map(|name| {
            parts
                .headers
                .get(name)
                .and_then(|h| h.to_str().ok())
                .map(|v| (name.to_owned(), v.to_owned()))
        })
        .collect();
    let gateway = svc.gateway.clone();
    if input["stream"] == true {
        let (tx, rx) = mpsc::channel::<Bytes>(16);
        tokio::task::spawn_blocking(move || {
            let forwarded: Vec<_> = headers
                .iter()
                .map(|(k, v)| (k.as_str(), v.as_str()))
                .collect();
            let _ = gateway.execute_cancellable(&token, &input, &forwarded, |event| {
                let frame = format!(
                    "event: {}\ndata: {event}\n\n",
                    event["type"].as_str().unwrap_or("error")
                );
                tx.blocking_send(Bytes::from(frame)).is_ok()
            });
        });
        let stream = futures_util::stream::unfold(rx, |mut rx| async {
            rx.recv()
                .await
                .map(|b| (Ok::<_, Infallible>(Frame::data(b)), rx))
        });
        Ok(Response::builder()
            .header("content-type", "text/event-stream")
            .header("cache-control", "no-store")
            .body(StreamBody::new(stream).boxed_unsync())
            .unwrap())
    } else {
        let result = tokio::task::spawn_blocking(move || {
            let forwarded: Vec<_> = headers
                .iter()
                .map(|(k, v)| (k.as_str(), v.as_str()))
                .collect();
            gateway.execute(&token, &input, &forwarded, |_| {})
        })
        .await;
        Ok(match result {
            Ok(Ok(reply)) => json_reply(200, reply.message),
            Ok(Err(fault)) => fault_reply(fault),
            Err(_) => fault_reply(Fault::new(ErrorKind::Unavailable)),
        })
    }
}
