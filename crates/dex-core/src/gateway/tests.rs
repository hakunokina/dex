use super::*;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::thread;
use std::time::Duration;

struct Captured {
    path: String,
    headers: HashMap<String, String>,
    body: Value,
}
fn fake(
    status: u16,
    content_type: &str,
    body: String,
) -> (SocketAddr, thread::JoinHandle<Captured>) {
    fake_with(status, content_type, body, vec![])
}
fn fake_with(
    status: u16,
    content_type: &str,
    body: String,
    extra: Vec<(&'static str, &'static str)>,
) -> (SocketAddr, thread::JoinHandle<Captured>) {
    fake_gated(status, content_type, body, extra, None)
}
fn fake_gated(
    status: u16,
    content_type: &str,
    body: String,
    extra: Vec<(&'static str, &'static str)>,
    gate: Option<Arc<std::sync::Barrier>>,
) -> (SocketAddr, thread::JoinHandle<Captured>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let content_type = content_type.to_owned();
    let handle = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        let mut bytes = Vec::new();
        let end = loop {
            let mut b = [0; 1];
            stream.read_exact(&mut b).unwrap();
            bytes.push(b[0]);
            if bytes.ends_with(b"\r\n\r\n") {
                break bytes.len();
            }
        };
        let text = String::from_utf8(bytes[..end].to_vec()).unwrap();
        let mut lines = text.lines();
        let path = lines
            .next()
            .unwrap()
            .split_whitespace()
            .nth(1)
            .unwrap()
            .to_owned();
        let headers: HashMap<_, _> = lines
            .filter_map(|s| s.split_once(':'))
            .map(|(k, v)| (k.to_ascii_lowercase(), v.trim().to_owned()))
            .collect();
        let len = headers
            .get("content-length")
            .map(|s| s.parse::<usize>().unwrap())
            .unwrap_or(0);
        let mut payload = vec![0; len];
        stream.read_exact(&mut payload).unwrap();
        let body_value = if len == 0 {
            Value::Null
        } else {
            serde_json::from_slice(&payload).unwrap()
        };
        if let Some(gate) = gate {
            gate.wait();
        }
        let chunked = extra
            .iter()
            .any(|(k, v)| *k == "Transfer-Encoding" && *v == "chunked");
        let mut head = format!(
            "HTTP/1.1 {status} Fixture\r\nContent-Type: {content_type}\r\nConnection: close\r\n"
        );
        if !chunked {
            head.push_str(&format!("Content-Length: {}\r\n", body.len()));
        }
        for (k, v) in extra {
            head.push_str(&format!("{k}: {v}\r\n"));
        }
        head.push_str("\r\n");
        stream.write_all(head.as_bytes()).unwrap();
        // Split wire reads inside UTF-8, SSE delimiters and JSON arguments.
        for bytes in body.as_bytes().chunks(3) {
            if chunked && write!(stream, "{:x}\r\n", bytes.len()).is_err() {
                break;
            }
            if stream.write_all(bytes).is_err() {
                break;
            }
            if chunked && stream.write_all(b"\r\n").is_err() {
                break;
            }
        }
        if chunked {
            let _ = stream.write_all(b"0\r\n\r\n");
        }
        Captured {
            path,
            headers,
            body: body_value,
        }
    });
    (addr, handle)
}
fn route(address: SocketAddr, protocol: Protocol) -> Route {
    Route {
        endpoint: Endpoint::Loopback(address),
        protocol,
        profile: Profile::ApiKey,
        alias: "alias".into(),
        model: "native-model".into(),
        provider: "fake".into(),
        account: "account-a".into(),
        auth: UpstreamAuth::Bearer("synthetic-upstream".into()),
    }
}
fn request(stream: bool) -> Value {
    json!({"model":"alias", "max_tokens":512, "stream":stream, "messages":[{"role":"user", "content":"synthetic prompt"}]})
}
#[test]
fn chat_without_tools_omits_tool_controls() {
    let r = route("127.0.0.1:1".parse().unwrap(), Protocol::OpenaiChat);
    let mut req = request(false);
    req["tool_choice"] = json!({"type":"auto"});
    let body = request::translate(&r, &req).unwrap();
    assert!(body.get("parallel_tool_calls").is_none());
    assert!(body.get("tool_choice").is_none());
}
#[test]
fn unknown_response_event_keeps_stream_alive() {
    let r = route("127.0.0.1:1".parse().unwrap(), Protocol::OpenaiResponses);
    let mut state = stream::Stream::new(&r);
    assert!(state
        .feed(&json!({"type":"response.queued","secret":"never log this"}).to_string())
        .unwrap()
        .is_empty());
    state.feed(&json!({"type":"response.completed","response":{"status":"completed","output":[{"type":"message","content":[{"type":"output_text","text":"ok"}]}]}}).to_string()).unwrap();
    assert_eq!(state.message().unwrap()["content"][0]["text"], "ok");
}
#[test]
fn chat_null_continuation_keeps_tool_identity() {
    let r = route("127.0.0.1:1".parse().unwrap(), Protocol::OpenaiChat);
    let mut state = stream::Stream::new(&r);
    for delta in [
        json!({"index":0,"id":"call","function":{"name":"Read","arguments":"{"}}),
        json!({"index":0,"id":null,"function":{"name":null,"arguments":"}"}}),
    ] {
        state
            .feed(&json!({"choices":[{"index":0,"delta":{"tool_calls":[delta]}}]}).to_string())
            .unwrap();
    }
    state
        .feed(&json!({"choices":[{"index":0,"delta":{},"finish_reason":"tool_calls"}]}).to_string())
        .unwrap();
    state.feed("[DONE]").unwrap();
    assert_eq!(state.message().unwrap()["content"][0]["id"], "call");
}
#[test]
fn foreign_reasoning_is_dropped_without_crossing_scope() {
    let mut r = route("127.0.0.1:1".parse().unwrap(), Protocol::OpenaiResponses);
    let signature = request::signature(
        &r,
        &json!({"type":"reasoning","id":"r","encrypted_content":"opaque"}),
    )
    .unwrap();
    r.account = "different".into();
    for sig in [
        signature,
        "native-anthropic".into(),
        "dex:responses:v1:invalid".into(),
    ] {
        let mut req = request(false);
        req["messages"] = json!([{"role":"assistant","content":[{"type":"thinking","thinking":"foreign","signature":sig},{"type":"text","text":"keep"}]}]);
        let body = request::translate(&r, &req).unwrap();
        assert_eq!(body["input"].as_array().unwrap().len(), 1);
        assert_eq!(body["input"][0]["content"][0]["text"], "keep");
        assert!(!body.to_string().contains("opaque"));
    }
}
#[test]
fn upstream_reasoning_without_encryption_is_protocol_fault() {
    let r = route("127.0.0.1:1".parse().unwrap(), Protocol::OpenaiResponses);
    let mut state = stream::Stream::new(&r);
    let fault = state.feed(&json!({"type":"response.output_item.done","output_index":0,"item":{"type":"reasoning","id":"r","summary":[]}}).to_string()).err().unwrap();
    assert_eq!(fault.kind, ErrorKind::Protocol);
}
#[test]
fn refusal_is_a_text_reply() {
    let r = route("127.0.0.1:1".parse().unwrap(), Protocol::OpenaiResponses);
    let mut state = stream::Stream::new(&r);
    state.feed(&json!({"type":"response.completed","response":{"status":"completed","output":[{"type":"message","content":[{"type":"refusal","refusal":"Cannot help"}]}]}}).to_string()).unwrap();
    assert_eq!(
        state.message().unwrap()["content"][0]["text"],
        "Cannot help"
    );
}
#[test]
fn downstream_errors_use_anthropic_types_and_sanitized_quota_signal() {
    for (kind, expected) in [
        (ErrorKind::Quota, "rate_limit_error"),
        (ErrorKind::RateLimit, "rate_limit_error"),
        (ErrorKind::Unavailable, "overloaded_error"),
        (ErrorKind::UpstreamAuth, "authentication_error"),
        (ErrorKind::InvalidRequest, "invalid_request_error"),
    ] {
        let event = Fault::new(kind).event();
        assert_eq!(event["error"]["type"], expected);
        if kind == ErrorKind::Quota {
            assert_eq!(
                crate::chain::classify("rate_limit", event["error"]["message"].as_str().unwrap()),
                crate::chain::Kind::Permanent
            );
        }
    }
}
#[test]
fn response_read_budget_survives_the_old_three_second_deadline() {
    assert_eq!(transport::READ_BUDGET, Duration::from_secs(120));
    let gate = Arc::new(std::sync::Barrier::new(2));
    let (addr, server) = fake_gated(
        200,
        "text/event-stream",
        text_response("late"),
        vec![],
        Some(gate.clone()),
    );
    let release = thread::spawn(move || {
        thread::sleep(Duration::from_millis(3200));
        gate.wait();
    });
    let gateway = Gateway::default();
    gateway
        .register("slow".into(), route(addr, Protocol::OpenaiResponses))
        .unwrap();
    let result = gateway.execute("slow", &request(false), &[], |_| {});
    release.join().unwrap();
    server.join().unwrap();
    assert_eq!(result.unwrap().message["content"][0]["text"], "late");
}
#[test]
fn loopback_ingress_reaches_upstream_and_revokes_local_token() {
    let svc = ingress::Service::start().unwrap();
    let (addr, server) = fake(200, "text/event-stream", text_response("ingress"));
    svc.gateway
        .register(
            "ingress-token".into(),
            route(addr, Protocol::OpenaiResponses),
        )
        .unwrap();
    let client = transport::client().unwrap();
    let send = || {
        client
            .post(format!("{}/v1/messages", svc.base_url()))
            .header("x-api-key", "ingress-token")
            .json(&request(false))
            .send()
            .unwrap()
    };
    let response = send();
    assert_eq!(response.status().as_u16(), 200);
    assert_eq!(
        response.json::<Value>().unwrap()["content"][0]["text"],
        "ingress"
    );
    let captured = server.join().unwrap();
    assert_eq!(
        captured.headers["authorization"],
        "Bearer synthetic-upstream"
    );
    svc.gateway.revoke("ingress-token");
    assert_eq!(send().status().as_u16(), 401);
    svc.shutdown();
    assert!(client.get(svc.base_url()).send().is_err());
}
#[test]
fn ingress_sse_cancellation_keeps_job_lease_and_parallel_requests_alive() {
    let root = std::env::temp_dir().join(format!("dex-gateway-cancel-{}", std::process::id()));
    let core = crate::Core::start(
        crate::Paths {
            data_dir: root.join("data"),
            config_dir: root.join("config"),
        },
        crate::Events::new(crate::events::NullSink),
    )
    .unwrap();
    core.gateway.auth.seed_for_test();
    crate::db::with(&core.db, |c| {
        crate::teams::update_agent(c, "kaan", crate::teams::AgentPatch {
            provider: Some("gateway".into()), model: Some("allowed".into()), ..Default::default()
        })?;
        c.execute("INSERT INTO delegations(id,from_agent,to_agent,team_id,prompt,status,created_at,updated_at) VALUES(42,'timur','kaan','default','synthetic','running',0,0)", []).map_err(|e|e.to_string())?;
        c.execute("INSERT INTO agent_sessions(agent_id,session_id,engine,updated_at,delegation_id) VALUES('kaan','synthetic-session','claude',0,42)", []).map_err(|e|e.to_string())?;
        Ok(())
    }).unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    core.gateway
        .endpoint_for_test(listener.local_addr().unwrap());
    let (release_first, first_gate) = std::sync::mpsc::channel();
    let (release_parallel, parallel_gate) = std::sync::mpsc::channel();
    let (started, parallel_started) = std::sync::mpsc::channel();
    let (finished, first_finished) = std::sync::mpsc::channel();
    let upstream = thread::spawn(move || {
        let mut workers = vec![];
        let mut gates = vec![Some(first_gate), Some(parallel_gate), None].into_iter();
        for index in 0..3 {
            let (mut socket, _) = listener.accept().unwrap();
            let gate = gates.next().unwrap();
            let started = started.clone();
            let finished = finished.clone();
            workers.push(thread::spawn(move || {
                use std::io::BufRead;
                socket.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
                socket.set_write_timeout(Some(Duration::from_secs(5))).unwrap();
                let mut reader = std::io::BufReader::new(socket.try_clone().unwrap());
                let mut length = 0;
                loop {
                    let mut line = String::new();
                    assert_ne!(reader.read_line(&mut line).unwrap(), 0, "incomplete HTTP headers");
                    if line == "\r\n" { break; }
                    if let Some(value) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                        length = value.trim().parse().unwrap();
                    }
                }
                reader.read_exact(&mut vec![0; length]).unwrap();
                socket.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n").unwrap();
                if index == 0 {
                    socket.write_all(b"data: {\"type\":\"response.created\",\"response\":{\"id\":\"cancel\"}}\n\n").unwrap();
                } else if index == 1 {
                    started.send(()).unwrap();
                }
                if let Some(gate) = gate { gate.recv_timeout(Duration::from_secs(5)).unwrap(); }
                if index == 0 {
                    // No terminal frame: normal completion cannot disguise a lost
                    // cancellation. Exceed the bounded downstream queue as well.
                    let mut events = vec![json!({"type":"response.output_item.added","output_index":0,"item":{"type":"message","id":"cancel_message"}})];
                    events.extend((0..64).map(|_| json!({"type":"response.output_text.delta","output_index":0,"delta":"cancel"})));
                    let _ = socket.write_all(sse(events).as_bytes());
                    // The canceled request must finish/close egress before reuse is checked.
                    let mut byte = [0];
                    let _ = socket.read(&mut byte).unwrap();
                    finished.send(()).unwrap();
                } else {
                    let _ = socket.write_all(text_response("still alive").as_bytes());
                }
            }));
        }
        for worker in workers {
            worker.join().unwrap();
        }
    });
    let env: HashMap<_, _> = core
        .gateway_spawn_env("kaan", "allowed")
        .unwrap()
        .into_iter()
        .collect();
    let mut req = request(true);
    req["model"] = json!("allowed");
    let url = format!("{}/v1/messages", env["ANTHROPIC_BASE_URL"]);
    let token = env["ANTHROPIC_AUTH_TOKEN"].clone();
    let mut canceled = transport::client()
        .unwrap()
        .post(&url)
        .header("x-api-key", &token)
        .json(&req)
        .send()
        .unwrap();
    let mut byte = [0];
    canceled.read_exact(&mut byte).unwrap();
    let parallel_url = url.clone();
    let parallel_token = token.clone();
    let parallel_req = req.clone();
    let parallel = thread::spawn(move || {
        transport::client()
            .unwrap()
            .post(parallel_url)
            .header("x-api-key", parallel_token)
            .json(&parallel_req)
            .send()
            .unwrap()
            .text()
            .unwrap()
    });
    parallel_started
        .recv_timeout(Duration::from_secs(5))
        .unwrap();
    drop(canceled);
    release_first.send(()).unwrap();
    first_finished.recv_timeout(Duration::from_secs(5)).unwrap();
    release_parallel.send(()).unwrap();
    let parallel_body = parallel.join().unwrap();
    assert!(parallel_body.contains("message_stop"), "{parallel_body}");
    assert!(!parallel_body.contains("event: error"), "{parallel_body}");
    let reused: HashMap<_, _> = core
        .gateway_spawn_env("kaan", "allowed")
        .unwrap()
        .into_iter()
        .collect();
    assert_eq!(reused["ANTHROPIC_AUTH_TOKEN"], token);
    req["stream"] = json!(false);
    let second = transport::client()
        .unwrap()
        .post(url)
        .header("x-api-key", token)
        .json(&req)
        .send()
        .unwrap();
    assert_eq!(second.status().as_u16(), 200);
    assert_eq!(
        second.json::<Value>().unwrap()["content"][0]["text"],
        "still alive"
    );
    upstream.join().unwrap();
    core.shutdown();
}
#[test]
fn siwc_start_binds_pkce_state_nonce_host_and_callback() {
    use base64::Engine;
    use sha2::{Digest, Sha256};
    let auth = auth::Auth::for_test();
    let started = auth.begin("http://127.0.0.1:9876", None).unwrap();
    let url = reqwest::Url::parse(&started.authorization_url).unwrap();
    let query: HashMap<_, _> = url
        .query_pairs()
        .map(|(k, v)| (k.into_owned(), v.into_owned()))
        .collect();
    assert_eq!(query["client_id"], "dynamic_agent_client");
    assert_eq!(query["agent_name_hint"], "Dex");
    let uuid = query["ext_agent_host_id"]
        .strip_prefix("urn:uuid:")
        .unwrap();
    assert_eq!(uuid.len(), 36);
    let groups: Vec<_> = uuid.split('-').collect();
    assert_eq!(
        groups.iter().map(|g| g.len()).collect::<Vec<_>>(),
        [8, 4, 4, 4, 12]
    );
    assert!(groups.iter().all(|g| g
        .bytes()
        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))));
    assert_eq!(uuid.as_bytes()[14], b'4');
    assert!(b"89ab".contains(&uuid.as_bytes()[19]));
    assert_eq!(query["redirect_uri"], "http://127.0.0.1:9876/auth/callback");
    assert_eq!(query["resource"], "https://api.openai.com/v1");
    let pending = auth.pending_for_test();
    assert_eq!(query["state"], pending.state);
    assert_eq!(query["nonce"], pending.nonce);
    assert_eq!(
        query["code_challenge"],
        base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(Sha256::digest(pending.verifier.as_bytes()))
    );
    assert_eq!(auth.status().unwrap().state, "signing_in");
    assert!(auth
        .complete(
            "http://127.0.0.1:9876/auth/callback?state=wrong&code=secret&client_id=oaiapp_test"
        )
        .is_err());
    assert_eq!(auth.status().unwrap().state, "signing_in");
}
#[test]
fn siwc_start_migrates_legacy_host_once() {
    let auth = auth::Auth::for_test();
    auth.seed_for_test();
    auth.set_host_for_test("dex-host-legacy");
    let started = auth.begin("http://127.0.0.1:9876", None).unwrap();
    let url = reqwest::Url::parse(&started.authorization_url).unwrap();
    let host = url
        .query_pairs()
        .find(|(k, _)| k == "ext_agent_host_id")
        .unwrap()
        .1
        .into_owned();
    assert!(host.starts_with("urn:uuid:"));
    assert_eq!(auth.host_for_test(), host);
    let started = auth.begin("http://127.0.0.1:9876", None).unwrap();
    let url = reqwest::Url::parse(&started.authorization_url).unwrap();
    assert_eq!(
        url.query_pairs()
            .find(|(k, _)| k == "ext_agent_host_id")
            .unwrap()
            .1,
        host
    );
    assert_eq!(auth.host_for_test(), host);
    let status = auth.status().unwrap();
    assert_eq!(status.accounts.len(), 1);
    assert_eq!(status.active_account_id, Some("a".repeat(64)));
    assert!(status.accounts[0].signed_in);
}
#[test]
fn siwc_start_preserves_registered_uuid_host() {
    let auth = auth::Auth::for_test();
    let host = "urn:uuid:123e4567-e89b-42d3-a456-426614174000";
    auth.set_host_for_test(host);
    let started = auth.begin("http://127.0.0.1:9876", None).unwrap();
    let url = reqwest::Url::parse(&started.authorization_url).unwrap();
    assert_eq!(
        url.query_pairs()
            .find(|(k, _)| k == "ext_agent_host_id")
            .unwrap()
            .1,
        host
    );
    assert_eq!(auth.host_for_test(), host);
}
pub(super) fn synthetic_jwt(claims: Value) -> (String, Value) {
    use base64::Engine;
    let fixture: Value = serde_json::from_str(include_str!("test-key.json")).unwrap();
    let key = ring::signature::RsaKeyPair::from_pkcs8(
        &base64::engine::general_purpose::STANDARD
            .decode(fixture["pkcs8"].as_str().unwrap())
            .unwrap(),
    )
    .unwrap();
    let enc = &base64::engine::general_purpose::URL_SAFE_NO_PAD;
    let signed = format!(
        "{}.{}",
        enc.encode(json!({"alg":"RS256","kid":"fixture"}).to_string()),
        enc.encode(claims.to_string())
    );
    let mut signature = vec![0; key.public().modulus_len()];
    key.sign(
        &ring::signature::RSA_PKCS1_SHA256,
        &ring::rand::SystemRandom::new(),
        signed.as_bytes(),
        &mut signature,
    )
    .unwrap();
    (
        format!("{signed}.{}", enc.encode(signature)),
        json!({"keys":[{"kid":"fixture","alg":"RS256","kty":"RSA","use":"sig","n":fixture["n"],"e":fixture["e"]}]}),
    )
}
#[test]
fn siwc_id_token_signature_audience_identity_and_nonce_are_verified() {
    let claims = json!({"iss":"https://auth.openai.com","aud":"oaiapp_test","sub":"subject","nonce":"nonce","exp":auth::now()+600,"iat":auth::now(),"email":"fixture@example.invalid"});
    let (token, jwks) = synthetic_jwt(claims.clone());
    assert_eq!(
        auth::validate_id(&token, &jwks, "oaiapp_test", Some("nonce"))
            .unwrap()
            .subject,
        "subject"
    );
    for (client, nonce) in [("other", Some("nonce")), ("oaiapp_test", Some("other"))] {
        assert!(auth::validate_id(&token, &jwks, client, nonce).is_err());
    }
    for (field, value) in [
        ("iss", json!("https://attacker.invalid")),
        ("exp", json!(auth::now() - 1)),
        ("sub", json!("")),
    ] {
        let mut changed = claims.clone();
        changed[field] = value;
        let (token, _) = synthetic_jwt(changed);
        assert!(auth::validate_id(&token, &jwks, "oaiapp_test", Some("nonce")).is_err());
    }
    let tampered = format!("{token}A");
    assert!(auth::validate_id(&tampered, &jwks, "oaiapp_test", Some("nonce")).is_err());
}
#[test]
fn siwc_official_exchange_models_refresh_and_logout_are_scoped() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let auth = auth::Auth::for_test_endpoint(&base);
    auth.begin("http://127.0.0.1:9876", None).unwrap();
    let pending = auth.pending_for_test();
    let (id, jwks) = synthetic_jwt(
        json!({"iss":"https://auth.openai.com","aud":"oaiapp_test","sub":"subject","nonce":pending.nonce,"iat":auth::now(),"exp":auth::now()+600,
            "https://api.openai.com/auth":{"chatgpt_plan_type":"plus"}}),
    );
    let upstream = thread::spawn(move || {
        let responses = [
            json!({"issuer":"https://auth.openai.com","authorization_endpoint":"https://auth.openai.com/api/accounts/authorize","token_endpoint":format!("{base}/token"),"jwks_uri":format!("{base}/jwks"),"revocation_endpoint":format!("{base}/revoke")}),
            json!({"access_token":"access-one","refresh_token":"refresh-one","id_token":id,"token_type":"Bearer","expires_in":3600,"scope":"openid offline_access resource.invoke chatgpt.tokens.use.direct"}),
            jwks.clone(),
            json!({"models":[{"slug":"allowed","display_name":"Allowed","visibility":"list"},{"slug":"hidden","visibility":"hidden"}]}),
            jwks,
            json!({"access_token":"access-two","refresh_token":"refresh-two","token_type":"Bearer","expires_in":3600}),
            Value::Null,
        ];
        let mut captured = vec![];
        for value in responses {
            let (mut socket, _) = listener.accept().unwrap();
            socket
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut reader = std::io::BufReader::new(socket.try_clone().unwrap());
            use std::io::BufRead;
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            let path = line.split_whitespace().nth(1).unwrap().to_owned();
            let mut headers = HashMap::new();
            loop {
                line.clear();
                reader.read_line(&mut line).unwrap();
                if line == "\r\n" {
                    break;
                }
                let (k, v) = line.trim_end().split_once(':').unwrap();
                headers.insert(k.to_ascii_lowercase(), v.trim().to_owned());
            }
            let mut bytes = vec![
                0;
                headers
                    .get("content-length")
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(0)
            ];
            reader.read_exact(&mut bytes).unwrap();
            let body = String::from_utf8(bytes).unwrap();
            captured.push((path, headers, body));
            let json = if value.is_null() {
                String::new()
            } else {
                value.to_string()
            };
            write!(socket,"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{json}",json.len()).unwrap();
        }
        captured
    });
    auth.complete(&format!(
        "http://127.0.0.1:9876/auth/callback?state={}&code=synthetic-code&client_id=oaiapp_test",
        pending.state
    ))
    .unwrap();
    let status = auth.status().unwrap();
    assert_eq!(status.state, "ready");
    let view = serde_json::to_value(&status).unwrap();
    assert_eq!(view["accounts"][0]["plan"], "plus");
    let expires = view["accounts"][0]["expiresAt"].as_u64().unwrap();
    assert!((auth::now() + 3595) * 1000 <= expires && expires <= (auth::now() + 3600) * 1000);
    assert!(!view.to_string().contains("access-one"));
    assert!(!view.to_string().contains("refresh-one"));
    let account = status.active_account_id.unwrap();
    let models = auth.models().unwrap();
    assert_eq!(models.len(), 1);
    assert_eq!(models[0].id, "allowed");
    auth.expire_for_test();
    assert_eq!(auth.access(&account).unwrap(), "access-two");
    assert_eq!(
        serde_json::to_value(auth.status().unwrap()).unwrap()["accounts"][0]["plan"],
        "plus"
    );
    assert!(auth.access("other-account").is_err());
    assert!(auth.logout(Some(&account)).unwrap().remote_revoked);
    assert_eq!(auth.status().unwrap().state, "signed_out");
    assert_eq!(auth.status().unwrap().accounts.len(), 1);
    let signed_out = serde_json::to_value(auth.status().unwrap()).unwrap();
    assert!(signed_out["accounts"][0].get("plan").is_none());
    assert!(signed_out["accounts"][0].get("expiresAt").is_none());
    let captured = upstream.join().unwrap();
    assert!(captured[1].2.contains("grant_type=authorization_code"));
    assert!(captured[1].2.contains("client_id=oaiapp_test"));
    assert!(!captured[1].2.contains("dynamic_agent_client"));
    assert_eq!(captured[3].1["authorization"], "Bearer access-one");
    assert_eq!(captured[4].0, "/jwks");
    assert!(captured[5].2.contains("refresh_token=refresh-one"));
    assert!(!captured[5].2.contains("scope="));
    assert!(captured[6].2.contains("token=refresh-two"));
}
#[test]
fn gateway_worker_environment_contains_only_local_auth_and_selected_models() {
    let env =
        crate::providers::gateway_env("http://127.0.0.1:9876", "local-capability", "allowed-model");
    let values: HashMap<_, _> = env.into_iter().collect();
    assert_eq!(values["ANTHROPIC_AUTH_TOKEN"], "local-capability");
    assert_eq!(values["ANTHROPIC_BASE_URL"], "http://127.0.0.1:9876");
    for name in [
        "ANTHROPIC_MODEL",
        "ANTHROPIC_DEFAULT_OPUS_MODEL",
        "ANTHROPIC_DEFAULT_SONNET_MODEL",
        "ANTHROPIC_DEFAULT_HAIKU_MODEL",
        "CLAUDE_CODE_SUBAGENT_MODEL",
    ] {
        assert_eq!(values[name], "allowed-model");
    }
    assert!(!values.contains_key("OPENAI_API_KEY"));
}
#[test]
fn siwc_model_http_failures_keep_retryable_grants_and_block_rejected_auth() {
    for (status, code, ready) in [
        (429, "gateway_auth_rate_limited", true),
        (503, "gateway_auth_unavailable", true),
        (401, "gateway_login_required", false),
    ] {
        let (address, server) = fake(status, "text/plain", "raw credential".into());
        let auth = auth::Auth::for_test_endpoint(&format!("http://{address}"));
        auth.seed_for_test();
        assert_eq!(auth.models().err().as_deref(), Some(code));
        assert_eq!(auth.status().unwrap().active_account_id.is_some(), ready);
        server.join().unwrap();
    }
}
#[test]
fn core_gateway_rpcs_reject_remote_login_and_register_local_lifecycle() {
    let root = std::env::temp_dir().join(format!("dex-gateway-core-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    let paths = crate::Paths {
        data_dir: root.join("data"),
        config_dir: root.join("config"),
    };
    let core = crate::Core::start_bare(paths, crate::Events::new(crate::events::NullSink)).unwrap();
    assert_eq!(
        core.gateway_auth_start(None).err().unwrap(),
        "giriş yalnız yerel Dex'ten"
    );
    let base = core.gateway.base_url();
    core.shutdown();
    assert!(transport::client().unwrap().get(base).send().is_err());
    drop(core);
    // The pre-existing hook receiver retains Core/SQLite until the test process exits.
    // Its fixture is inside the task's TEMP; only gateway listener closure is asserted here.
}
#[test]
fn siwc_callback_listener_waits_for_fragmented_requests_under_load() {
    use std::io::BufRead;
    thread::scope(|scope| {
        let clients: Vec<_> = (0..8)
            .map(|_| {
                scope.spawn(|| {
                    let auth = Arc::new(auth::Auth::for_test());
                    let (finished, received) = std::sync::mpsc::channel();
                    let started = auth
                        .start_login(None, move |ok| finished.send(ok).unwrap())
                        .unwrap();
                    let authorize = reqwest::Url::parse(&started.authorization_url).unwrap();
                    let query: HashMap<_, _> = authorize.query_pairs().into_owned().collect();
                    let callback = reqwest::Url::parse(&query["redirect_uri"]).unwrap();
                    let address = format!(
                        "{}:{}",
                        callback.host_str().unwrap(),
                        callback.port().unwrap()
                    );
                    let mut socket = std::net::TcpStream::connect(&address).unwrap();
                    socket
                        .set_read_timeout(Some(Duration::from_secs(3)))
                        .unwrap();
                    socket
                        .set_write_timeout(Some(Duration::from_secs(3)))
                        .unwrap();
                    // Accept must wait for bytes, then for the rest of the HTTP headers.
                    thread::sleep(Duration::from_millis(100));
                    write!(
                        socket,
                        "GET {}?state={}&error=access_denied HTTP/1.1\r\n",
                        callback.path(),
                        query["state"]
                    )
                    .unwrap();
                    socket.flush().unwrap();
                    thread::sleep(Duration::from_millis(100));
                    write!(socket, "Host: {address}\r\n\r\n").unwrap();
                    socket.flush().unwrap();
                    let mut reader = std::io::BufReader::new(socket);
                    let mut line = String::new();
                    reader.read_line(&mut line).unwrap();
                    assert_eq!(line, "HTTP/1.1 400 Bad Request\r\n");
                    let mut length = None;
                    loop {
                        line.clear();
                        assert!(reader.read_line(&mut line).unwrap() > 0);
                        if line == "\r\n" {
                            break;
                        }
                        if let Some(value) = line.strip_prefix("Content-Length: ") {
                            length = Some(value.trim().parse::<usize>().unwrap());
                        }
                    }
                    let mut body = vec![0; length.unwrap()];
                    reader.read_exact(&mut body).unwrap();
                    assert_eq!(
                        String::from_utf8(body).unwrap(),
                        "Giriş tamamlanamadı. Dex'teki giriş durumunu kontrol et."
                    );
                    assert!(!received.recv_timeout(Duration::from_secs(3)).unwrap());
                    assert_eq!(
                        auth.status().unwrap().error.as_deref(),
                        Some("gateway_auth_denied")
                    );
                })
            })
            .collect();
        for client in clients {
            client.join().unwrap();
        }
    });
}
#[test]
fn siwc_callback_listener_is_loopback_single_use_and_closes_after_denial() {
    let auth = Arc::new(auth::Auth::for_test());
    let started = auth.start_login(None, |_| {}).unwrap();
    let authorize = reqwest::Url::parse(&started.authorization_url).unwrap();
    let query: HashMap<_, _> = authorize
        .query_pairs()
        .map(|(k, v)| (k.into_owned(), v.into_owned()))
        .collect();
    let callback = &query["redirect_uri"];
    assert!(callback.starts_with("http://127.0.0.1:"));
    let client = transport::client().unwrap();
    assert_eq!(
        client
            .get(format!("{callback}?state=wrong&error=access_denied"))
            .send()
            .unwrap()
            .status()
            .as_u16(),
        400
    );
    assert_eq!(auth.status().unwrap().state, "signing_in");
    assert_eq!(
        client
            .get(format!(
                "{callback}?state={}&error=access_denied",
                query["state"]
            ))
            .send()
            .unwrap()
            .status()
            .as_u16(),
        400
    );
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    while client.get(callback).send().is_ok() {
        assert!(std::time::Instant::now() < deadline);
        thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(
        auth.status().unwrap().error.as_deref(),
        Some("gateway_auth_denied")
    );
}
#[test]
fn core_job_token_reaches_fake_siwc_and_completion_revokes_it() {
    let root = std::env::temp_dir().join(format!("dex-gateway-job-{}", std::process::id()));
    let core = crate::Core::start(
        crate::Paths {
            data_dir: root.join("data"),
            config_dir: root.join("config"),
        },
        crate::Events::new(crate::events::NullSink),
    )
    .unwrap();
    core.gateway.auth.seed_for_test();
    let (addr, upstream) = fake(200, "text/event-stream", text_response("job"));
    core.gateway.endpoint_for_test(addr);
    crate::db::with(&core.db,|c| {
        crate::teams::update_agent(c,"kaan",crate::teams::AgentPatch {provider:Some("gateway".into()),model:Some("allowed".into()),..Default::default()})?;
        c.execute("INSERT INTO delegations(id,from_agent,to_agent,team_id,prompt,status,created_at,updated_at) VALUES(42,'timur','kaan','default','synthetic','running',0,0)",[]).map_err(|e|e.to_string())?;
        c.execute("INSERT INTO agent_sessions(agent_id,session_id,engine,updated_at,delegation_id) VALUES('kaan','synthetic-session','claude',0,42)",[]).map_err(|e|e.to_string())?;Ok(())
    }).unwrap();
    let env = core.gateway_spawn_env("kaan", "allowed").unwrap();
    let env: HashMap<_, _> = env.into_iter().collect();
    let token = &env["ANTHROPIC_AUTH_TOKEN"];
    assert_ne!(token, "synthetic-access");
    let client = transport::client().unwrap();
    let mut req = request(false);
    req["model"] = json!("allowed");
    let send = || {
        client
            .post(format!("{}/v1/messages", env["ANTHROPIC_BASE_URL"]))
            .header("authorization", format!("Bearer {token}"))
            .json(&req)
            .send()
            .unwrap()
    };
    assert_eq!(send().json::<Value>().unwrap()["content"][0]["text"], "job");
    assert_eq!(
        upstream.join().unwrap().headers["authorization"],
        "Bearer synthetic-access"
    );
    crate::db::with(&core.db, |c| {
        c.execute("UPDATE delegations SET status='done' WHERE id=42", [])
            .map(|_| ())
            .map_err(|e| e.to_string())
    })
    .unwrap();
    core.events.emit("delegations-changed", &()).unwrap();
    assert_eq!(send().status().as_u16(), 401);
    assert!(core.gateway_spawn_env("kaan", "unlisted").is_err());
    core.shutdown();
}
#[test]
fn ingress_siwc_429_drives_existing_stop_failure_chain_without_raw_error_body() {
    let root = std::env::temp_dir().join(format!("dex-gateway-quota-{}", std::process::id()));
    let core = crate::Core::start(
        crate::Paths {
            data_dir: root.join("data"),
            config_dir: root.join("config"),
        },
        crate::Events::new(crate::events::NullSink),
    )
    .unwrap();
    core.gateway.auth.seed_for_test();
    crate::db::with(&core.db,|c| {
        crate::teams::update_agent(c,"kaan",crate::teams::AgentPatch {provider:Some("gateway".into()),model:Some("allowed".into()),..Default::default()})?;
        c.execute("INSERT INTO delegations(id,from_agent,to_agent,team_id,prompt,status,created_at,updated_at) VALUES(42,'timur','kaan','default','synthetic','running',0,0)",[]).map_err(|e|e.to_string())?;
        c.execute("INSERT INTO agent_sessions(agent_id,session_id,engine,updated_at,delegation_id) VALUES('kaan','synthetic-session','claude',0,42)",[]).map_err(|e|e.to_string())?;Ok(())
    }).unwrap();
    for code in [
        "rate_limit_exceeded",
        "subscription_sharing_usage_limit_exceeded",
    ] {
        let (addr, upstream) = fake(
            429,
            "application/json",
            json!({"error":{"code":code,"message":"raw upstream credential and prompt"}})
                .to_string(),
        );
        core.gateway.revoke_agent("kaan");
        core.gateway.endpoint_for_test(addr);
        let env: HashMap<_, _> = core
            .gateway_spawn_env("kaan", "allowed")
            .unwrap()
            .into_iter()
            .collect();
        let mut req = request(false);
        req["model"] = json!("allowed");
        let response = transport::client()
            .unwrap()
            .post(format!("{}/v1/messages", env["ANTHROPIC_BASE_URL"]))
            .header(
                "authorization",
                format!("Bearer {}", env["ANTHROPIC_AUTH_TOKEN"]),
            )
            .json(&req)
            .send()
            .unwrap();
        assert_eq!(response.status().as_u16(), 429);
        let body = response.json::<Value>().unwrap();
        assert_eq!(body["error"]["type"], "rate_limit_error");
        assert!(!body.to_string().contains("raw upstream"));
        let hook = json!({"error":"rate_limit","last_assistant_message":body["error"]["message"]});
        assert_eq!(
            core.chain_plan("kaan", &hook).is_some(),
            code == "subscription_sharing_usage_limit_exceeded"
        );
        upstream.join().unwrap();
    }
    core.shutdown();
}
#[test]
fn sse_read_deadline_remains_120_seconds_despite_continuing_events() {
    use std::io::BufRead;
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let done = Arc::new(AtomicBool::new(false));
    let server_done = done.clone();
    let server = thread::spawn(move || {
        let (mut socket, _) = listener.accept().unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        socket
            .set_write_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let mut reader = std::io::BufReader::new(socket.try_clone().unwrap());
        let mut length = 0;
        loop {
            let mut line = String::new();
            assert_ne!(reader.read_line(&mut line).unwrap(), 0, "incomplete HTTP headers");
            if line == "\r\n" {
                break;
            }
            if let Some(value) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                length = value.trim().parse().unwrap();
            }
        }
        let mut payload = vec![0; length];
        reader.read_exact(&mut payload).unwrap();
        socket
            .write_all(
                b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n",
            )
            .unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(135);
        while !server_done.load(Ordering::SeqCst) && std::time::Instant::now() < deadline {
            if socket
                .write_all(b"data: {\"type\":\"response.queued\"}\n\n")
                .is_err()
            {
                break;
            }
            thread::sleep(Duration::from_secs(1));
        }
    });
    let gateway = Gateway::default();
    gateway
        .register("deadline".into(), route(address, Protocol::OpenaiResponses))
        .unwrap();
    let start = std::time::Instant::now();
    let mut events = vec![];
    let result = gateway.execute("deadline", &request(true), &[], |e| events.push(e));
    let elapsed = start.elapsed();
    done.store(true, Ordering::SeqCst);
    server.join().unwrap();
    assert_eq!(result.err().unwrap().kind, ErrorKind::Transport);
    assert!(elapsed >= Duration::from_secs(119) && elapsed < Duration::from_secs(135));
    assert_eq!(events.iter().filter(|e| e["type"] == "error").count(), 1);
    assert!(!events.iter().any(|e| e["type"] == "message_stop"));
}
#[test]
fn chain_gateway_needs_a_valid_siwc_grant_and_explicit_order() {
    let root = std::env::temp_dir().join(format!("dex-gateway-chain-{}", std::process::id()));
    let core = crate::Core::start(
        crate::Paths {
            data_dir: root.join("data"),
            config_dir: root.join("config"),
        },
        crate::Events::new(crate::events::NullSink),
    )
    .unwrap();
    assert!(!core.chain_has_token("gateway"));
    crate::db::with(&core.db, |c| {
        assert!(!crate::chain::order(c).contains(&"gateway".to_owned()));
        crate::chain::set_order(c, &["anthropic".into(), "gateway".into(), "codex".into()])?;
        assert_eq!(
            crate::chain::pick_next(c, "anthropic", &|p| core.chain_has_token(p))?.as_deref(),
            Some("codex")
        );
        Ok(())
    })
    .unwrap();
    core.gateway.auth.seed_for_test();
    assert!(core.chain_has_token("gateway"));
    core.gateway.auth.invalidate_for_test();
    assert!(!core.chain_has_token("gateway"));
    core.shutdown();
}
#[test]
fn rejected_siwc_auth_is_no_longer_a_chain_candidate() {
    for status in [401, 403] {
        #[derive(Clone, Default)]
        struct Recorded(Arc<Mutex<Vec<String>>>);
        impl crate::EventSink for Recorded {
            fn emit(&self, event: &str, _: Value) -> Result<(), String> {
                self.0.lock().unwrap().push(event.into());
                Ok(())
            }
        }
        let events = Recorded::default();
        let root = std::env::temp_dir().join(format!(
            "dex-gateway-auth-bad-{status}-{}",
            std::process::id()
        ));
        let core = crate::Core::start(
            crate::Paths {
                data_dir: root.join("data"),
                config_dir: root.join("config"),
            },
            crate::Events::new(events.clone()),
        )
        .unwrap();
        core.gateway.auth.seed_for_test();
        let account = core
            .gateway_auth_status()
            .unwrap()
            .active_account_id
            .unwrap();
        assert!(core.chain_has_token("gateway"));
        events.0.lock().unwrap().clear();
        let (address, upstream) = fake(
            status,
            "application/json",
            json!({"error":{"code":"invalid_api_key","message":"raw credential"}}).to_string(),
        );
        let mut selected = route(address, Protocol::OpenaiResponses);
        selected.account = account.clone();
        selected.provider = "gateway".into();
        selected.auth = UpstreamAuth::SiwcAccount(account);
        core.gateway
            .gateway
            .register("bad-auth".into(), selected)
            .unwrap();
        assert_eq!(
            core.gateway
                .gateway
                .execute("bad-auth", &request(false), &[], |_| {})
                .err()
                .unwrap()
                .kind,
            ErrorKind::UpstreamAuth
        );
        upstream.join().unwrap();
        assert!(!core.chain_has_token("gateway"));
        let changes: Vec<_> = events.0.lock().unwrap().iter()
            .filter(|e| e.as_str() == "gateway-auth-changed" || e.as_str() == "providers-changed")
            .cloned().collect();
        assert_eq!(changes, ["gateway-auth-changed", "providers-changed"]);
        core.shutdown();
    }
}
fn native_json() -> Value {
    json!({"id":"msg_fixture", "type":"message", "role":"assistant", "model":"native-model", "content":[{"type":"text","text":"fixture ok"}], "stop_reason":"end_turn", "stop_sequence":null, "usage":{"input_tokens":3,"output_tokens":2}})
}
#[test]
fn native_route_preserves_body_headers_model_and_uses_upstream_auth() {
    let (addr, server) = fake(200, "application/json", native_json().to_string());
    let gateway = Gateway::default();
    gateway
        .register(
            "local-capability".into(),
            route(addr, Protocol::AnthropicMessages),
        )
        .unwrap();
    let mut req = request(false);
    req["system"] =
        json!([{"type":"text","text":"rules","cache_control":{"type":"ephemeral","ttl":"1h"}}]);
    req["betas"] = json!(["fixture-beta"]);
    req["output_config"] = json!({"effort":"xhigh"});
    let mut events = vec![];
    let reply = gateway
        .execute(
            "local-capability",
            &req,
            &[
                ("anthropic-beta", "fixture-beta"),
                ("anthropic-version", "2023-06-01"),
                ("authorization", "Bearer local-capability"),
            ],
            |e| events.push(e),
        )
        .unwrap();
    assert_eq!(reply.message, native_json());
    assert!(events.is_empty());
    let sent = server.join().unwrap();
    assert_eq!(sent.path, "/v1/messages");
    let mut expected = req;
    expected["model"] = json!("native-model");
    assert_eq!(sent.body, expected);
    assert_eq!(sent.headers["authorization"], "Bearer synthetic-upstream");
    assert_eq!(sent.headers["anthropic-beta"], "fixture-beta");
}

fn rich_request() -> Value {
    json!({"model":"alias", "stream":false, "max_tokens":512, "temperature":0.4,"top_p":0.8,
        "metadata":{"user_id":"synthetic"}, "user":"synthetic", "previous_response_id":"old", "conversation":"old", "background":true,"prompt_cache_retention":"24h",
        "system":[{"type":"text","text":"rules"}], "thinking":{"type":"adaptive"}, "output_config":{"effort":"high"},
        "tool_choice":{"type":"auto","disable_parallel_tool_use":false},
        "tools":[{"name":"Read","description":"read", "input_schema":{"type":"object","properties":{"path":{"type":"string"}}}}],
        "messages":[{"role":"user","content":[{"type":"text","text":"read"},{"type":"image","source":{"type":"base64","media_type":"image/png","data":"YQ=="}}]},
        {"role":"system","content":"intermediate rules"},
        {"role":"assistant","content":[{"type":"tool_use","id":"call_a","name":"Read","input":{"path":"a"}}]},
        {"role":"user","content":[{"type":"tool_result","tool_use_id":"call_a","content":"ok"}]}]})
}
#[test]
fn responses_profiles_map_tools_images_history_effort_and_parallel_intent() {
    for profile in [
        Profile::ApiKey,
        Profile::SiwcAdditionalTools,
        Profile::SiwcNamespace,
    ] {
        for effort in ["medium", "high", "xhigh"] {
            let mut route = route("127.0.0.1:1".parse().unwrap(), Protocol::OpenaiResponses);
            route.profile = profile;
            route.model = "gpt-6.1-sol".into();
            let mut input = rich_request();
            input["output_config"]["effort"] = json!(effort);
            let body = request::translate(&route, &input).unwrap();
            assert_eq!(body["model"], "gpt-6.1-sol");
            assert_eq!(body["stream"], true);
            assert_eq!(body["store"], false);
            assert_eq!(body["reasoning"]["effort"], effort);
            assert_eq!(body["parallel_tool_calls"], true);
            let items = body["input"].as_array().unwrap();
            assert!(items.iter().any(|v| v["type"] == "function_call"
                && v["call_id"] == "call_a"
                && v["name"] == "Read"
                && v["arguments"] == "{\"path\":\"a\"}"));
            assert!(items.iter().any(|v| v["type"] == "function_call_output"
                && v["call_id"] == "call_a"
                && v["output"] == "ok"));
            assert!(items
                .iter()
                .any(|v| v["content"].as_array().is_some_and(|a| a
                    .iter()
                    .any(|p| p["image_url"] == "data:image/png;base64,YQ=="))));
            assert!(!items.iter().any(|v| v["role"] == "system"));
            match profile {
                Profile::ApiKey => {
                    assert_eq!(body["max_output_tokens"], 512);
                    assert_eq!(
                        body["tools"][0]["parameters"],
                        input["tools"][0]["input_schema"]
                    );
                }
                Profile::SiwcAdditionalTools => {
                    assert!(items.iter().any(|v| v["type"] == "additional_tools"
                        && v["role"] == "developer"
                        && v["tools"][0]["name"] == "Read"));
                    assert!(body.get("tools").is_none());
                }
                Profile::SiwcNamespace => {
                    assert_eq!(body["tools"][0]["type"], "namespace");
                    assert_eq!(body["tools"][0]["tools"][0]["name"], "Read");
                }
            }
            if profile != Profile::ApiKey {
                for field in [
                    "max_output_tokens",
                    "temperature",
                    "top_p",
                    "metadata",
                    "user",
                    "previous_response_id",
                    "conversation",
                    "background",
                    "prompt_cache_retention",
                ] {
                    assert!(body.get(field).is_none());
                }
            }
            input["tool_choice"]["disable_parallel_tool_use"] = json!(true);
            assert_eq!(
                request::translate(&route, &input).unwrap()["parallel_tool_calls"],
                false
            );
        }
    }
}
#[test]
fn chat_maps_tools_images_results_and_effort_without_responses_fields() {
    let route = route("127.0.0.1:1".parse().unwrap(), Protocol::OpenaiChat);
    let body = request::translate(&route, &rich_request()).unwrap();
    assert_eq!(body["reasoning_effort"], "high");
    assert_eq!(body["stream"], true);
    assert_eq!(
        body["tools"][0]["function"]["parameters"],
        rich_request()["tools"][0]["input_schema"]
    );
    let messages = body["messages"].as_array().unwrap();
    assert!(messages
        .iter()
        .any(|v| v["role"] == "tool" && v["tool_call_id"] == "call_a"));
    assert!(messages
        .iter()
        .any(|v| v["tool_calls"][0]["id"] == "call_a"));
    assert!(messages
        .iter()
        .any(|v| v["content"][1]["image_url"]["url"] == "data:image/png;base64,YQ=="));
    assert!(body.get("input").is_none());
    assert!(body.get("store").is_none());
}

fn sse(events: Vec<Value>) -> String {
    events
        .into_iter()
        .map(|v| {
            format!(
                "event: {}\r\ndata: {}\r\n\r\n",
                v["type"].as_str().unwrap_or("message"),
                v
            )
        })
        .collect()
}
fn fixture(name: &str, body: String) -> String {
    static FIXTURES: Mutex<()> = Mutex::new(());
    let _guard = FIXTURES.lock().unwrap();
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.dex/tmp/848/fixtures");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(name);
    std::fs::write(&path, body).unwrap();
    std::fs::read_to_string(path).unwrap()
}
fn parallel_response() -> String {
    fixture(
        "parallel.sse",
        sse(vec![
            json!({"type":"response.created","response":{"id":"resp_a","status":"in_progress"}}),
            json!({"type":"response.output_item.added","output_index":0,"item":{"type":"reasoning","id":"reason_a","encrypted_content":"opaque-fixture"}}),
            json!({"type":"response.reasoning_summary_text.delta","output_index":0,"delta":"Planı kur"}),
            json!({"type":"response.output_item.done","output_index":0,"item":{"type":"reasoning","id":"reason_a","summary":[{"type":"summary_text","text":"Planı kur"}]}}),
            json!({"type":"response.output_item.added","output_index":1,"item":{"type":"function_call","id":"fc_a","call_id":"call_a","name":"Read","arguments":""}}),
            json!({"type":"response.output_item.added","output_index":2,"item":{"type":"function_call","id":"fc_b","call_id":"call_b","name":"Search","arguments":""}}),
            json!({"type":"response.function_call_arguments.delta","output_index":1,"delta":"{\"path\":"}),
            json!({"type":"response.function_call_arguments.delta","output_index":2,"delta":"{\"query\":"}),
            json!({"type":"response.function_call_arguments.delta","output_index":1,"delta":"\"a\"}"}),
            json!({"type":"response.function_call_arguments.delta","output_index":2,"delta":"\"b\"}"}),
            json!({"type":"response.output_item.done","output_index":2,"item":{"type":"function_call","call_id":"call_b","name":"Search","arguments":"{\"query\":\"b\"}"}}),
            json!({"type":"response.output_item.done","output_index":1,"item":{"type":"function_call","call_id":"call_a","name":"Read","arguments":"{\"path\":\"a\"}"}}),
            json!({"type":"response.completed","response":{"id":"resp_a","status":"completed","usage":{"input_tokens":10,"output_tokens":8,"input_tokens_details":{"cached_tokens":3}}}}),
        ]),
    )
}
fn text_response(text: &str) -> String {
    sse(vec![
        json!({"type":"response.created","response":{"id":"resp_b"}}),
        json!({"type":"response.output_item.added","output_index":0,"item":{"type":"message","id":"m_b"}}),
        json!({"type":"response.output_text.delta","output_index":0,"delta":text}),
        json!({"type":"response.output_item.done","output_index":0,"item":{"type":"message","id":"m_b","content":[{"type":"output_text","text":text}]}}),
        json!({"type":"response.completed","response":{"id":"resp_b","status":"completed","usage":{"input_tokens":15,"output_tokens":3}}}),
    ])
}
fn valid_order(events: &[Value], stop: &str) {
    assert_eq!(events.first().unwrap()["type"], "message_start");
    let mut active = None;
    let mut next = 0;
    for e in events {
        match e["type"].as_str().unwrap() {
            "content_block_start" => {
                assert!(active.is_none());
                assert_eq!(e["index"], next);
                active = Some(next);
                next += 1;
            }
            "content_block_delta" => assert_eq!(e["index"].as_u64(), active),
            "content_block_stop" => {
                assert_eq!(e["index"].as_u64(), active);
                active = None;
            }
            _ => {}
        }
    }
    assert!(active.is_none());
    assert_eq!(events[events.len() - 2]["delta"]["stop_reason"], stop);
    assert_eq!(events.last().unwrap()["type"], "message_stop");
}
#[test]
fn responses_two_turns_parallel_arguments_reasoning_replay_and_nonstream_json() {
    let gateway = Gateway::default();
    let (addr, server) = fake(200, "text/event-stream", parallel_response());
    let mut r = route(addr, Protocol::OpenaiResponses);
    r.profile = Profile::SiwcAdditionalTools;
    r.model = "gpt-6.1-sol".into();
    gateway.register("local-a".into(), r).unwrap();
    let mut events = vec![];
    let first = gateway
        .execute(
            "local-a",
            &request(true),
            &[("anthropic-beta", "do-not-forward")],
            |v| events.push(v),
        )
        .unwrap();
    valid_order(&events, "tool_use");
    assert_eq!(first.message["content"][1]["input"], json!({"path":"a"}));
    assert_eq!(first.message["content"][2]["input"], json!({"query":"b"}));
    assert_eq!(first.message["content"][0]["thinking"], "Planı kur");
    assert!(first.message["content"][0]["signature"]
        .as_str()
        .unwrap()
        .contains("opaque-fixture"));
    assert_eq!(first.message["usage"]["cache_read_input_tokens"], 3);
    let captured = server.join().unwrap();
    assert_eq!(captured.path, "/v1/responses");
    assert!(!captured.headers.contains_key("anthropic-beta"));
    assert_eq!(
        captured.headers["authorization"],
        "Bearer synthetic-upstream"
    );
    gateway.revoke("local-a");
    let (addr, server) = fake(200, "text/event-stream", text_response("İkinci tur"));
    let mut r = route(addr, Protocol::OpenaiResponses);
    r.profile = Profile::SiwcAdditionalTools;
    r.model = "gpt-6.1-sol".into();
    gateway.register("local-b".into(), r).unwrap();
    let mut req = request(false);
    req["messages"].as_array_mut().unwrap().extend([
        json!({"role":"assistant","content":first.message["content"]}),
        json!({"role":"user","content":[{"type":"tool_result","tool_use_id":"call_a","content":"a-result"},{"type":"tool_result","tool_use_id":"call_b","content":"b-result"}]})]);
    events.clear();
    let second = gateway
        .execute("local-b", &req, &[], |v| events.push(v))
        .unwrap();
    assert!(events.is_empty());
    assert_eq!(second.message["content"][0]["text"], "İkinci tur");
    assert_eq!(second.message["stop_reason"], "end_turn");
    let sent = server.join().unwrap();
    let items = sent.body["input"].as_array().unwrap();
    assert!(items.iter().any(|v| v["type"] == "reasoning"
        && v["id"] == "reason_a"
        && v["encrypted_content"] == "opaque-fixture"));
    assert!(items
        .iter()
        .any(|v| v["call_id"] == "call_b" && v["name"] == "Search"));
    assert_eq!(
        items
            .iter()
            .filter(|v| v["type"] == "function_call_output")
            .count(),
        2
    );
    assert_eq!(sent.body["stream"], true);
    assert_eq!(sent.body["store"], false);
}
#[test]
fn chat_stream_parallel_fragments_become_ordered_anthropic_tool_blocks() {
    let events = vec![
        json!({"id":"chat_a","choices":[{"index":0,"delta":{"role":"assistant","tool_calls":[{"index":0,"id":"call_a","function":{"name":"Read","arguments":"{\"path\":"}},{"index":1,"id":"call_b","function":{"name":"Search","arguments":"{\"query\":"}}]},"finish_reason":null}]}),
        json!({"id":"chat_a","choices":[{"index":0,"delta":{"tool_calls":[{"index":1,"function":{"arguments":"\"b\"}"}},{"index":0,"function":{"arguments":"\"a\"}"}}]},"finish_reason":null}]}),
        json!({"id":"chat_a","choices":[{"index":0,"delta":{},"finish_reason":"tool_calls"}]}),
        json!({"id":"chat_a","choices":[],"usage":{"prompt_tokens":10,"completion_tokens":5}}),
    ];
    let body = format!("{}data: [DONE]\n\n", sse(events));
    let (addr, server) = fake(200, "text/event-stream", fixture("chat.sse", body));
    let gateway = Gateway::default();
    gateway
        .register("local-a".into(), route(addr, Protocol::OpenaiChat))
        .unwrap();
    let mut events = vec![];
    let reply = gateway
        .execute("local-a", &request(true), &[], |e| events.push(e))
        .unwrap();
    valid_order(&events, "tool_use");
    assert_eq!(reply.message["content"][0]["input"], json!({"path":"a"}));
    assert_eq!(reply.message["content"][1]["input"], json!({"query":"b"}));
    assert_eq!(reply.message["usage"]["output_tokens"], 5);
    assert_eq!(server.join().unwrap().path, "/v1/chat/completions");
}

fn native_sse() -> String {
    let mut message = native_json();
    message["content"] = json!([]);
    message["stop_reason"] = Value::Null;
    sse(vec![
        json!({"type":"message_start","message":message}),
        json!({"type":"ping"}),
        json!({"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}),
        json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"fixture ok"}}),
        json!({"type":"content_block_stop","index":0}),
        json!({"type":"message_delta","delta":{"stop_reason":"end_turn","stop_sequence":null},"usage":{"output_tokens":2}}),
        json!({"type":"message_stop"}),
    ])
}
#[test]
fn native_sse_preserves_events_and_requires_message_stop() {
    let (addr, server) = fake(
        200,
        "text/event-stream",
        fixture("native.sse", native_sse()),
    );
    let gateway = Gateway::default();
    gateway
        .register("local-a".into(), route(addr, Protocol::AnthropicMessages))
        .unwrap();
    let mut events = vec![];
    let reply = gateway
        .execute("local-a", &request(true), &[], |v| events.push(v))
        .unwrap();
    valid_order(&events, "end_turn");
    assert_eq!(reply.message, native_json());
    assert!(events.iter().any(|e| e["type"] == "ping"));
    server.join().unwrap();
    let body = native_sse().replace(
        "event: message_stop\r\ndata: {\"type\":\"message_stop\"}\r\n\r\n",
        "",
    );
    let (addr, server) = fake(200, "text/event-stream", body);
    let gateway = Gateway::default();
    gateway
        .register("local-a".into(), route(addr, Protocol::AnthropicMessages))
        .unwrap();
    let mut events = vec![];
    let result = gateway.execute("local-a", &request(true), &[], |v| events.push(v));
    assert!(matches!(
        result,
        Err(Fault {
            kind: ErrorKind::Eof,
            ..
        })
    ));
    assert_eq!(events.iter().filter(|v| v["type"] == "error").count(), 1);
    assert!(!events.iter().any(|v| v["type"] == "message_delta"));
    server.join().unwrap();
}
#[test]
fn errors_are_single_classified_signals_without_fake_success_or_fallback() {
    let cases = [
        (
            429,
            json!({"error":{"code":"insufficient_quota","message":"synthetic private detail"}})
                .to_string(),
            ErrorKind::Quota,
        ),
        (
            429,
            json!({"error":{"code":"rate_limit_exceeded"}}).to_string(),
            ErrorKind::RateLimit,
        ),
        (
            503,
            json!({"error":{"code":"subscription_sharing_usage_unavailable"}}).to_string(),
            ErrorKind::Unavailable,
        ),
        (
            401,
            json!({"error":{"code":"invalid_api_key"}}).to_string(),
            ErrorKind::UpstreamAuth,
        ),
        (
            200,
            sse(vec![
                json!({"type":"response.created","response":{"id":"failed"}}),
                json!({"type":"response.failed","response":{"status":"failed","error":{"code":"subscription_sharing_usage_limit_exceeded","message":"private"}}}),
            ]),
            ErrorKind::Quota,
        ),
        (
            200,
            sse(vec![
                json!({"type":"response.incomplete","response":{"status":"incomplete"}}),
            ]),
            ErrorKind::Incomplete,
        ),
        (
            200,
            sse(vec![
                json!({"type":"response.created","response":{"id":"cut"}}),
            ]),
            ErrorKind::Eof,
        ),
        (200, "data: {broken}\n\n".into(), ErrorKind::Protocol),
        (
            200,
            "data: {\"type\":\"response.created\"}".into(),
            ErrorKind::Eof,
        ),
        (
            200,
            sse(vec![
                json!({"type":"response.completed","response":{"status":"completed","output":[]}}),
            ]),
            ErrorKind::Protocol,
        ),
    ];
    for (status, body, expected) in cases {
        let (addr, server) = fake(
            status,
            if status == 200 {
                "text/event-stream"
            } else {
                "application/json"
            },
            body,
        );
        let gateway = Gateway::default();
        gateway
            .register("local-a".into(), route(addr, Protocol::OpenaiResponses))
            .unwrap();
        let mut events = vec![];
        let result = gateway.execute("local-a", &request(true), &[], |v| events.push(v));
        assert!(matches!(result,Err(Fault{kind,..}) if kind==expected));
        assert_eq!(events.iter().filter(|v| v["type"] == "error").count(), 1);
        assert!(!events
            .iter()
            .any(|v| v["type"] == "message_stop" || v.pointer("/delta/stop_reason").is_some()));
        assert!(!events.iter().any(|v| v.to_string().contains("private")));
        assert_eq!(server.join().unwrap().path, "/v1/responses");
    }
}
#[test]
fn scoped_clients_are_concurrent_and_revoke_cancels_inflight_stream() {
    let gateway = Arc::new(Gateway::default());
    let gate = Arc::new(std::sync::Barrier::new(2));
    let (addr_a, server_a) = fake_gated(
        200,
        "text/event-stream",
        text_response("a"),
        vec![],
        Some(gate.clone()),
    );
    let (addr_b, server_b) = fake_gated(
        200,
        "text/event-stream",
        text_response("b"),
        vec![],
        Some(gate),
    );
    let mut a = route(addr_a, Protocol::OpenaiResponses);
    a.model = "model-a".into();
    a.auth = UpstreamAuth::Bearer("synthetic-a".into());
    let mut b = route(addr_b, Protocol::OpenaiResponses);
    b.model = "model-b".into();
    b.account = "account-b".into();
    b.auth = UpstreamAuth::Bearer("synthetic-b".into());
    gateway.register("local-a".into(), a).unwrap();
    gateway.register("local-b".into(), b).unwrap();
    let ga = gateway.clone();
    let gb = gateway.clone();
    let a = thread::spawn(move || {
        ga.execute("local-a", &request(false), &[], |_| {})
            .unwrap()
            .message
    });
    let b = thread::spawn(move || {
        gb.execute("local-b", &request(false), &[], |_| {})
            .unwrap()
            .message
    });
    assert_eq!(a.join().unwrap()["content"][0]["text"], "a");
    assert_eq!(b.join().unwrap()["content"][0]["text"], "b");
    let a = server_a.join().unwrap();
    let b = server_b.join().unwrap();
    assert_eq!(a.body["model"], "model-a");
    assert_eq!(b.body["model"], "model-b");
    assert_eq!(a.headers["authorization"], "Bearer synthetic-a");
    assert_eq!(b.headers["authorization"], "Bearer synthetic-b");
    let (addr, server) = fake(200, "text/event-stream", parallel_response());
    gateway
        .register("revoke-me".into(), route(addr, Protocol::OpenaiResponses))
        .unwrap();
    let mut events = vec![];
    let result = gateway.execute("revoke-me", &request(true), &[], |v| {
        if v["type"] == "message_start" {
            gateway.revoke("revoke-me");
        }
        events.push(v);
    });
    assert!(matches!(
        result,
        Err(Fault {
            kind: ErrorKind::Revoked,
            ..
        })
    ));
    assert_eq!(events.iter().filter(|v| v["type"] == "error").count(), 1);
    assert!(!events.iter().any(|v| v["type"] == "message_stop"));
    server.join().unwrap();
    assert!(matches!(
        gateway.execute("revoke-me", &request(false), &[], |_| {}),
        Err(Fault {
            kind: ErrorKind::Unauthorized,
            ..
        })
    ));
}

struct CapturedLogs(Mutex<Vec<String>>);
static LOGS: CapturedLogs = CapturedLogs(Mutex::new(Vec::new()));
impl log::Log for CapturedLogs {
    fn enabled(&self, _: &log::Metadata<'_>) -> bool {
        true
    }
    fn log(&self, record: &log::Record<'_>) {
        self.0.lock().unwrap().push(record.args().to_string());
    }
    fn flush(&self) {}
}
fn files(dir: &std::path::Path) -> Vec<std::path::PathBuf> {
    if !dir.exists() {
        return vec![];
    }
    let mut out = vec![];
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            out.extend(files(&path));
        } else {
            out.push(path);
        }
    }
    out.sort();
    out
}
#[test]
fn canary_restart_leaves_no_disk_logs_or_output() {
    let credential = ["gateway", "credential", "canary", "848"].join("-");
    let prompt = ["gateway", "prompt", "canary", "848"].join("-");
    if std::env::var_os("DEX_GATEWAY_CANARY_CHILD").is_none() {
        // A fresh test process, twice: a real process restart, no CLI/model launch.
        let cwd =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.dex/tmp/848/canary");
        std::fs::create_dir_all(&cwd).unwrap();
        for _ in 0..2 {
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "gateway::tests::canary_restart_leaves_no_disk_logs_or_output",
                    "--nocapture",
                ])
                .env("DEX_GATEWAY_CANARY_CHILD", "1")
                .current_dir(&cwd)
                .output()
                .unwrap();
            let detail = format!(
                "{}{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            )
            .replace(&credential, "[credential]")
            .replace(&prompt, "[prompt]");
            assert!(output.status.success(), "{detail}");
            for bytes in [&output.stdout, &output.stderr] {
                let output = String::from_utf8_lossy(bytes);
                assert!(!output.contains(&credential));
                assert!(!output.contains(&prompt));
            }
        }
        return;
    }
    log::set_logger(&LOGS).unwrap();
    log::set_max_level(log::LevelFilter::Trace);
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.dex/tmp/848/canary");
    std::fs::create_dir_all(&root).unwrap();
    let before = files(&root);
    let gateway = Gateway::default();
    // Restart has no persisted local grant.
    assert!(matches!(
        gateway.execute("previous-process-token", &request(false), &[], |_| {}),
        Err(Fault {
            kind: ErrorKind::Unauthorized,
            ..
        })
    ));
    let (addr, server) = fake(200, "application/json", native_json().to_string());
    let mut r = route(addr, Protocol::AnthropicMessages);
    r.auth = UpstreamAuth::ApiKey(credential.clone());
    gateway.register("synthetic-local".into(), r).unwrap();
    let mut req = request(false);
    req["messages"][0]["content"] = json!(prompt);
    gateway
        .execute("synthetic-local", &req, &[], |_| {})
        .unwrap();
    server.join().unwrap();
    let body = json!({"error":{"code":"insufficient_quota","message":format!("{credential} {prompt}"),"details":{"token":credential,"request":prompt}}});
    let (addr, server) = fake(429, "application/json", body.to_string());
    let mut r = route(addr, Protocol::OpenaiResponses);
    r.auth = UpstreamAuth::Bearer(credential.clone());
    gateway.register("synthetic-error".into(), r).unwrap();
    let mut emitted = vec![];
    let fault = gateway
        .execute("synthetic-error", &req, &[], |v| emitted.push(v))
        .err()
        .unwrap();
    server.join().unwrap();
    let diagnostics = format!(
        "{fault:?} {} {}",
        serde_json::to_string(&fault).unwrap(),
        json!(emitted)
    );
    assert!(!diagnostics.contains(&credential));
    assert!(!diagnostics.contains(&prompt));
    drop(gateway);
    assert_eq!(before, files(&root));
    let logs = LOGS.0.lock().unwrap().join("\n");
    assert!(!logs.contains(&credential));
    assert!(!logs.contains(&prompt));
    let artifacts = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.dex/tmp/848");
    // Only gateway-owned artifacts. The suite's TEMP contains concurrently locked
    // SQLite files from unrelated Core tests; those are not gateway persistence.
    for name in ["source", "fixtures", "canary"] {
        for path in files(&artifacts.join(name)) {
            let bytes = std::fs::read(path).unwrap();
            let content = String::from_utf8_lossy(&bytes);
            assert!(!content.contains(&credential));
            assert!(!content.contains(&prompt));
        }
    }
}

#[test]
fn chain_faults_keep_request_account_provider_model_without_payload() {
    let gateway = Gateway::default();
    for account in ["account-a", "account-b"] {
        let (addr, server) = fake(
            429,
            "application/json",
            json!({"error":{"code":"subscription_sharing_usage_limit_exceeded"}}).to_string(),
        );
        let mut r = route(addr, Protocol::OpenaiResponses);
        r.account = account.into();
        gateway.register(account.into(), r).unwrap();
        let fault = gateway
            .execute(account, &request(true), &[], |_| {})
            .err()
            .unwrap();
        assert_eq!(fault.kind, ErrorKind::Quota);
        assert_eq!(fault.status, Some(429));
        let context = fault.context.unwrap();
        assert_eq!(context.account, account);
        assert_eq!(context.provider, "fake");
        assert_eq!(context.model, "native-model");
        assert!(context.request_id > 0);
        server.join().unwrap();
    }
}
#[test]
fn invalid_profiles_external_egress_and_unsupported_semantics_fail_closed() {
    let gateway = Gateway::default();
    assert!(matches!(
        gateway.register(
            "external".into(),
            route("192.0.2.1:443".parse().unwrap(), Protocol::OpenaiResponses)
        ),
        Err(Fault {
            kind: ErrorKind::Unsupported,
            ..
        })
    ));
    let mut bad = route("127.0.0.1:1".parse().unwrap(), Protocol::OpenaiChat);
    bad.profile = Profile::SiwcAdditionalTools;
    assert!(matches!(
        gateway.register("siwc-chat".into(), bad),
        Err(Fault {
            kind: ErrorKind::Unsupported,
            ..
        })
    ));
    let mut bad = route("127.0.0.1:1".parse().unwrap(), Protocol::OpenaiResponses);
    bad.profile = Profile::SiwcNamespace;
    bad.auth = UpstreamAuth::ApiKey("synthetic".into());
    assert!(matches!(
        gateway.register("siwc-api-key".into(), bad),
        Err(Fault {
            kind: ErrorKind::Unsupported,
            ..
        })
    ));
    let mut r = route("127.0.0.1:1".parse().unwrap(), Protocol::OpenaiResponses);
    r.model = "gpt-6.1-sol".into();
    for effort in ["none", "minimal"] {
        let mut req = request(false);
        req["output_config"] = json!({"effort":effort});
        assert!(matches!(
            request::translate(&r, &req),
            Err(Fault {
                kind: ErrorKind::Unsupported,
                ..
            })
        ));
    }
    let mut req = request(false);
    req["thinking"] = json!({"type":"disabled"});
    assert!(request::translate(&r, &req)
        .unwrap()
        .get("reasoning")
        .is_none());
    req["context_management"] = json!({"edits":[]});
    assert!(matches!(
        request::translate(&r, &req),
        Err(Fault {
            kind: ErrorKind::Unsupported,
            ..
        })
    ));
    let item = json!({"type":"reasoning","id":"r","encrypted_content":"opaque","summary":[]});
    let signature = request::signature(&r, &item).unwrap();
    r.account = "another-account".into();
    req = request(false);
    req["messages"] = json!([{"role":"assistant","content":[{"type":"thinking","thinking":"summary","signature":signature}]}]);
    let translated = request::translate(&r, &req).unwrap();
    assert_eq!(translated["input"], json!([]));
    assert!(!translated.to_string().contains("opaque"));
}

#[test]
fn all_three_routes_use_selected_model_auth_and_effort_over_loopback() {
    for protocol in [
        Protocol::AnthropicMessages,
        Protocol::OpenaiResponses,
        Protocol::OpenaiChat,
    ] {
        for effort in ["medium", "high", "xhigh"] {
            let mut req = rich_request();
            req["output_config"]["effort"] = json!(effort);
            let body = if protocol == Protocol::AnthropicMessages {
                native_json().to_string()
            } else if protocol == Protocol::OpenaiResponses {
                text_response("ok")
            } else {
                format!(
                    "{}data: [DONE]\n\n",
                    sse(vec![
                        json!({"id":"chat","choices":[{"index":0,"delta":{"content":"ok"},"finish_reason":"stop"}]}),
                        json!({"choices":[],"usage":{"prompt_tokens":1,"completion_tokens":1}})
                    ])
                )
            };
            let (addr, server) = fake(
                200,
                if protocol == Protocol::AnthropicMessages {
                    "application/json"
                } else {
                    "text/event-stream"
                },
                body,
            );
            let gateway = Gateway::default();
            let mut r = route(addr, protocol);
            if protocol == Protocol::AnthropicMessages {
                r.auth = UpstreamAuth::ApiKey("synthetic-native-key".into());
            }
            gateway.register("local-never-upstream".into(), r).unwrap();
            gateway
                .execute(
                    "local-never-upstream",
                    &req,
                    &[
                        ("authorization", "Bearer local-never-upstream"),
                        ("x-api-key", "local-never-upstream"),
                        ("anthropic-beta", "beta"),
                    ],
                    |_| {},
                )
                .unwrap();
            let sent = server.join().unwrap();
            assert_eq!(sent.body["model"], "native-model");
            assert!(!sent.body.to_string().contains("local-never-upstream"));
            assert!(!sent
                .headers
                .values()
                .any(|v| v.contains("local-never-upstream")));
            match protocol {
                Protocol::AnthropicMessages => {
                    assert_eq!(sent.path, "/v1/messages");
                    assert_eq!(sent.headers["x-api-key"], "synthetic-native-key");
                    assert_eq!(sent.body["output_config"]["effort"], effort);
                    assert_eq!(sent.headers["anthropic-beta"], "beta");
                }
                Protocol::OpenaiResponses => {
                    assert_eq!(sent.path, "/v1/responses");
                    assert_eq!(sent.body["reasoning"]["effort"], effort);
                    assert_eq!(sent.headers["authorization"], "Bearer synthetic-upstream");
                    assert!(!sent.headers.contains_key("anthropic-beta"));
                }
                Protocol::OpenaiChat => {
                    assert_eq!(sent.path, "/v1/chat/completions");
                    assert_eq!(sent.body["reasoning_effort"], effort);
                    assert_eq!(sent.headers["authorization"], "Bearer synthetic-upstream");
                }
            }
        }
    }
}
#[test]
fn chunked_sse_utf8_multiline_and_retry_metadata_are_bounded() {
    let (addr, server) = fake_with(
        200,
        "text/event-stream",
        text_response("çok parçalı"),
        vec![("Transfer-Encoding", "chunked")],
    );
    let gateway = Gateway::default();
    gateway
        .register("local-a".into(), route(addr, Protocol::OpenaiResponses))
        .unwrap();
    let reply = gateway
        .execute("local-a", &request(false), &[], |_| {})
        .unwrap();
    assert_eq!(reply.message["content"][0]["text"], "çok parçalı");
    server.join().unwrap();
    let wire = b": comment\r\n\r\nevent: ping\r\ndata: {\r\ndata: \"type\":\"ping\"}\r\n\r\n";
    let mut frames = stream::Frames::new(std::io::BufReader::with_capacity(
        1,
        std::io::Cursor::new(wire),
    ));
    let (event, data) = frames.next().unwrap().unwrap();
    assert_eq!(event.as_deref(), Some("ping"));
    assert_eq!(
        serde_json::from_str::<Value>(&data).unwrap(),
        json!({"type":"ping"})
    );
    assert!(frames.next().unwrap().is_none());
    let (addr, server) = fake_with(
        429,
        "application/json",
        json!({"error":{"code":"rate_limit_exceeded"}}).to_string(),
        vec![("Retry-After", "17")],
    );
    let gateway = Gateway::default();
    gateway
        .register("local-a".into(), route(addr, Protocol::OpenaiResponses))
        .unwrap();
    let fault = gateway
        .execute("local-a", &request(false), &[], |_| {})
        .err()
        .unwrap();
    assert_eq!(fault.retry_after_seconds, Some(17));
    server.join().unwrap();
}
#[test]
fn redirects_never_follow_external_location_and_header_injection_is_rejected() {
    let (addr, server) = fake_with(
        302,
        "application/json",
        "{}".into(),
        vec![("Location", "https://192.0.2.1/v1/responses")],
    );
    let gateway = Gateway::default();
    gateway
        .register("local-a".into(), route(addr, Protocol::OpenaiResponses))
        .unwrap();
    assert!(matches!(
        gateway.execute("local-a", &request(false), &[], |_| {}),
        Err(Fault {
            status: Some(302),
            ..
        })
    ));
    server.join().unwrap();
    let r = route("127.0.0.1:1".parse().unwrap(), Protocol::AnthropicMessages);
    gateway.register("header-test".into(), r).unwrap();
    assert!(matches!(
        gateway.execute(
            "header-test",
            &request(false),
            &[("anthropic-beta", "beta\r\nAuthorization: bad")],
            |_| {}
        ),
        Err(Fault {
            kind: ErrorKind::InvalidRequest,
            ..
        })
    ));
}

#[test]
fn malformed_stream_flags_and_openai_api_key_headers_are_rejected_before_connect() {
    let gateway = Gateway::default();
    let mut r = route("127.0.0.1:1".parse().unwrap(), Protocol::OpenaiResponses);
    r.auth = UpstreamAuth::ApiKey("synthetic".into());
    assert!(matches!(
        gateway.register("bad-auth".into(), r),
        Err(Fault {
            kind: ErrorKind::Unsupported,
            ..
        })
    ));
    gateway
        .register(
            "local".into(),
            route("127.0.0.1:1".parse().unwrap(), Protocol::OpenaiResponses),
        )
        .unwrap();
    let mut req = request(false);
    req["stream"] = json!("false");
    assert!(matches!(
        gateway.execute("local", &req, &[], |_| {}),
        Err(Fault {
            kind: ErrorKind::InvalidRequest,
            ..
        })
    ));
}
#[test]
fn malformed_tool_arguments_and_chat_missing_done_never_finish() {
    let body = parallel_response().replace("{\\\"path\\\":\\\"a\\\"}", "{\\\"path\\\":}");
    let (addr, server) = fake(200, "text/event-stream", body);
    let gateway = Gateway::default();
    gateway
        .register("local".into(), route(addr, Protocol::OpenaiResponses))
        .unwrap();
    let mut events = vec![];
    assert!(gateway
        .execute("local", &request(true), &[], |e| events.push(e))
        .is_err());
    assert!(!events.iter().any(|e| e["type"] == "message_stop"));
    server.join().unwrap();
    let body = sse(vec![
        json!({"id":"chat","choices":[{"index":0,"delta":{"content":"partial"},"finish_reason":"stop"}]}),
    ]);
    let (addr, server) = fake(200, "text/event-stream", body);
    let gateway = Gateway::default();
    gateway
        .register("local".into(), route(addr, Protocol::OpenaiChat))
        .unwrap();
    events.clear();
    assert!(matches!(
        gateway.execute("local", &request(true), &[], |e| events.push(e)),
        Err(Fault {
            kind: ErrorKind::Eof,
            ..
        })
    ));
    assert!(!events
        .iter()
        .any(|e| e.pointer("/delta/stop_reason").is_some()));
    server.join().unwrap();
}

#[test]
fn revoke_after_terminal_commit_does_not_emit_error_after_success() {
    let (addr, server) = fake(200, "text/event-stream", text_response("done"));
    let gateway = Gateway::default();
    gateway
        .register("local".into(), route(addr, Protocol::OpenaiResponses))
        .unwrap();
    let mut events = vec![];
    let result = gateway.execute("local", &request(true), &[], |v| {
        if v["type"] == "message_delta" {
            gateway.revoke("local");
        }
        events.push(v);
    });
    assert!(result.is_ok());
    assert!(!events.iter().any(|v| v["type"] == "error"));
    valid_order(&events, "end_turn");
    server.join().unwrap();
}

#[test]
fn midstream_server_failures_keep_status_and_are_unavailable() {
    let body = sse(vec![
        json!({"type":"response.failed","response":{"error":{"code":"server_error","status_code":503,"message":"private provider detail"}}}),
    ]);
    let (addr, server) = fake(200, "text/event-stream", body);
    let gateway = Gateway::default();
    gateway
        .register("local".into(), route(addr, Protocol::OpenaiResponses))
        .unwrap();
    let mut events = vec![];
    let fault = gateway
        .execute("local", &request(true), &[], |v| events.push(v))
        .err()
        .unwrap();
    assert_eq!(fault.kind, ErrorKind::Unavailable);
    assert_eq!(fault.status, Some(503));
    assert_eq!(events.len(), 1);
    assert_eq!(events[0]["type"], "error");
    server.join().unwrap();
}
#[test]
fn request_sse_and_replay_size_limits_fail_before_unbounded_work() {
    let gateway = Gateway::default();
    gateway
        .register(
            "local".into(),
            route("127.0.0.1:1".parse().unwrap(), Protocol::AnthropicMessages),
        )
        .unwrap();
    let mut req = request(false);
    req["messages"][0]["content"] = json!("a".repeat(MAX_BYTES));
    assert!(matches!(
        gateway.execute("local", &req, &[], |_| {}),
        Err(Fault {
            kind: ErrorKind::Limit,
            ..
        })
    ));
    let wire = format!("data: {}\n\n", "a".repeat(MAX_BYTES));
    let mut frames = stream::Frames::new(std::io::Cursor::new(wire));
    assert!(matches!(
        frames.next(),
        Err(Fault {
            kind: ErrorKind::Limit,
            ..
        })
    ));
    let r = route("127.0.0.1:1".parse().unwrap(), Protocol::OpenaiResponses);
    let item = json!({"type":"reasoning","id":"r","encrypted_content":"a".repeat(MAX_BYTES)});
    assert!(matches!(
        request::signature(&r, &item),
        Err(Fault {
            kind: ErrorKind::Limit,
            ..
        })
    ));
}

#[test]
fn revoke_still_invalidates_inflight_grant_after_registry_poisoning() {
    let (addr, server) = fake(200, "text/event-stream", text_response("done"));
    let gateway = Gateway::default();
    gateway
        .register("local".into(), route(addr, Protocol::OpenaiResponses))
        .unwrap();
    let result = gateway.execute("local", &request(true), &[], |v| {
        if v["type"] == "message_start" {
            let poisoned = std::panic::catch_unwind(|| {
                let _guard = gateway.grants.lock().unwrap();
                panic!("synthetic registry panic");
            });
            assert!(poisoned.is_err());
            gateway.revoke("local");
        }
    });
    assert!(matches!(
        result,
        Err(Fault {
            kind: ErrorKind::Revoked,
            ..
        })
    ));
    server.join().unwrap();
}
