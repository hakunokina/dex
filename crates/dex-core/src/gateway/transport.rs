//! HTTPS egress with explicit destinations and no ambient proxy, redirects or retries.
use super::*;
use reqwest::blocking::Client;
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use std::io::Read;
use std::time::Duration;

pub(super) const READ_BUDGET: Duration = Duration::from_secs(120);
pub(super) fn client() -> Result<Client, Fault> {
    let _ = rustls::crypto::ring::default_provider().install_default();
    Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .retry(reqwest::retry::never())
        .connect_timeout(Duration::from_secs(3))
        .timeout(READ_BUDGET)
        .connection_verbose(false)
        .tls_sslkeylogfile(false)
        .build()
        .map_err(|_| Fault::new(ErrorKind::Transport))
}
pub(super) struct Response {
    pub status: u16,
    pub headers: HashMap<String, String>,
    pub reader: Box<dyn Read>,
}
pub(super) fn secret_header(value: &str) -> Result<HeaderValue, Fault> {
    if value.bytes().any(|b| b < 32 || b == 127) {
        return Err(Fault::new(ErrorKind::InvalidRequest));
    }
    let mut h = HeaderValue::from_str(value).map_err(|_| Fault::new(ErrorKind::InvalidRequest))?;
    h.set_sensitive(true);
    Ok(h)
}
pub(super) fn post(
    route: &Route,
    path: &str,
    body: &Value,
    forwarded: &[(&str, &str)],
    access: Option<&str>,
) -> Result<Response, Fault> {
    let base = match route.endpoint {
        Endpoint::Openai => "https://api.openai.com".to_owned(),
        Endpoint::Loopback(address) if address.ip().is_loopback() => format!("http://{address}"),
        _ => return Err(Fault::new(ErrorKind::Unsupported)),
    };
    let mut headers = HeaderMap::new();
    match &route.auth {
        UpstreamAuth::Bearer(key) => {
            headers.insert("authorization", secret_header(&format!("Bearer {key}"))?);
        }
        UpstreamAuth::ApiKey(key) => {
            headers.insert("x-api-key", secret_header(key)?);
        }
        UpstreamAuth::SiwcAccount(_) => {
            headers.insert(
                "authorization",
                secret_header(&format!(
                    "Bearer {}",
                    access.ok_or_else(|| Fault::new(ErrorKind::UpstreamAuth))?
                ))?,
            );
        }
    }
    if route.protocol == Protocol::AnthropicMessages {
        for &(name, value) in forwarded {
            if name.eq_ignore_ascii_case("anthropic-beta")
                || name.eq_ignore_ascii_case("anthropic-version")
            {
                headers.insert(
                    HeaderName::from_bytes(name.as_bytes())
                        .map_err(|_| Fault::new(ErrorKind::InvalidRequest))?,
                    secret_header(value)?,
                );
            }
        }
    }
    let payload = serde_json::to_vec(body).map_err(|_| Fault::new(ErrorKind::InvalidRequest))?;
    if payload.len() > MAX_BYTES {
        return Err(Fault::new(ErrorKind::Limit));
    }
    let response = client()?
        .post(format!("{base}{path}"))
        // Blocking ClientBuilder::timeout resets on every read. A request timeout also
        // reaches reqwest's async body, enforcing one deadline across continuing SSE.
        .timeout(READ_BUDGET)
        .headers(headers)
        .header("content-type", "application/json")
        .body(payload)
        .send()
        .map_err(|_| Fault::new(ErrorKind::Transport))?;
    let status = response.status().as_u16();
    let headers = ["content-type", "retry-after"]
        .into_iter()
        .filter_map(|name| {
            response
                .headers()
                .get(name)
                .and_then(|v| v.to_str().ok())
                .map(|v| (name.to_owned(), v.to_owned()))
        })
        .collect();
    Ok(Response {
        status,
        headers,
        reader: Box::new(response),
    })
}
