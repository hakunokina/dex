//! Official public SIWC registration. Never imports Codex credentials or private endpoints.
use super::*;
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use sha2::{Digest, Sha256};
use std::time::{SystemTime, UNIX_EPOCH};

const ISSUER: &str = "https://auth.openai.com";
const AUTHORIZE: &str = "https://auth.openai.com/api/accounts/authorize";
const RESOURCE: &str = "https://api.openai.com/v1";
const SERVICE: &str = "dex-siwc";
pub(super) struct Identity {
    pub subject: String,
    email: Option<String>,
    plan: Option<String>,
}
pub(super) fn validate_id(
    raw: &str,
    jwks: &Value,
    client: &str,
    nonce: Option<&str>,
) -> Result<Identity, String> {
    let bad = || "gateway_id_token_invalid".to_owned();
    if raw.len() > 65536 {
        return Err(bad());
    }
    let parts: Vec<_> = raw.split('.').collect();
    if parts.len() != 3 {
        return Err(bad());
    }
    let decode = |s: &str| URL_SAFE_NO_PAD.decode(s).map_err(|_| bad());
    let header: Value = serde_json::from_slice(&decode(parts[0])?).map_err(|_| bad())?;
    let kid = header["kid"]
        .as_str()
        .filter(|k| !k.is_empty())
        .ok_or_else(bad)?;
    if header["alg"] != "RS256" || header.get("crit").is_some() {
        return Err(bad());
    }
    let keys: Vec<_> = jwks["keys"]
        .as_array()
        .ok_or_else(bad)?
        .iter()
        .filter(|k| k["kid"] == kid)
        .collect();
    if keys.len() != 1 {
        return Err(bad());
    }
    let key = keys[0];
    if key["kty"] != "RSA"
        || key.get("alg").is_some_and(|v| v != "RS256")
        || key.get("use").is_some_and(|v| v != "sig")
    {
        return Err(bad());
    }
    let n = decode(key["n"].as_str().ok_or_else(bad)?)?;
    let e = decode(key["e"].as_str().ok_or_else(bad)?)?;
    ring::signature::RsaPublicKeyComponents { n: &n, e: &e }
        .verify(
            &ring::signature::RSA_PKCS1_2048_8192_SHA256,
            format!("{}.{}", parts[0], parts[1]).as_bytes(),
            &decode(parts[2])?,
        )
        .map_err(|_| bad())?;
    let claims: Value = serde_json::from_slice(&decode(parts[1])?).map_err(|_| bad())?;
    let audience = claims["aud"].as_str().is_some_and(|v| v == client)
        || claims["aud"]
            .as_array()
            .is_some_and(|v| v.iter().any(|v| v == client));
    if claims["iss"] != ISSUER
        || !audience
        || claims["exp"].as_u64().is_none_or(|v| v <= now())
        || claims["iat"].as_u64().is_none_or(|v| v > now() + 60)
        || claims["nbf"].as_u64().is_some_and(|v| v > now() + 60)
        || nonce.is_some_and(|n| claims["nonce"] != n)
        || claims.get("azp").is_some_and(|v| v != client)
        || (claims["aud"].as_array().is_some_and(|v| v.len() > 1) && claims["azp"] != client)
    {
        return Err(bad());
    }
    let subject = claims["sub"]
        .as_str()
        .filter(|s| !s.is_empty() && s.len() <= 512)
        .ok_or_else(bad)?
        .to_owned();
    Ok(Identity {
        subject,
        email: claims["email"]
            .as_str()
            .filter(|s| s.len() <= 320)
            .map(str::to_owned),
        plan: claims["https://api.openai.com/auth"]["chatgpt_plan_type"]
            .as_str()
            .filter(|s| !s.is_empty() && s.len() <= 64)
            .map(str::to_owned),
    })
}

#[derive(Clone, Serialize, Deserialize)]
struct Account {
    id: String,
    subject: String,
    client_id: String,
    email: Option<String>,
    #[serde(default)]
    plan: Option<String>,
    tokens: Option<Tokens>,
    #[serde(default)]
    pending_refresh: Option<PendingRefresh>,
    models: Vec<Model>,
    #[serde(default)]
    blocked: bool,
}
#[derive(Clone, Serialize, Deserialize)]
struct Tokens {
    access_token: String,
    refresh_token: String,
    id_token: String,
    expires_at: u64,
    scopes: Vec<String>,
}
#[derive(Clone, Serialize, Deserialize)]
struct PendingRefresh {
    response: Value,
    received_at: u64,
}
#[derive(Default, Serialize, Deserialize)]
struct Record {
    host: String,
    active: Option<String>,
    accounts: Vec<Account>,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Model {
    pub id: String,
    pub label: String,
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountView {
    pub id: String,
    pub label: String,
    pub email: Option<String>,
    pub signed_in: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub plan: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<u64>,
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Status {
    pub state: String,
    pub active_account_id: Option<String>,
    pub accounts: Vec<AccountView>,
    pub models: Vec<Model>,
    pub error: Option<String>,
}
// Not Debug: authorization URLs carry a one-use state and PKCE challenge.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Started {
    pub authorization_url: String,
    pub expires_at: u64,
}
#[derive(Clone)]
pub(super) struct Pending {
    pub state: String,
    pub nonce: String,
    pub verifier: String,
    redirect: String,
    account: Option<String>,
    client: Option<String>,
    expires_at: u64,
}
#[derive(Default)]
struct State {
    pending: Option<Pending>,
    error: Option<String>,
}
#[derive(Clone)]
struct Endpoints {
    discovery: String,
    token: String,
    jwks: String,
    revoke: String,
    models: String,
}
impl Default for Endpoints {
    fn default() -> Self {
        Self {
            discovery: format!("{ISSUER}/.well-known/openid-configuration"),
            token: format!("{ISSUER}/api/accounts/oauth/token"),
            jwks: format!("{ISSUER}/.well-known/jwks.json"),
            revoke: format!("{ISSUER}/api/accounts/oauth/revoke"),
            models: format!("{RESOURCE}/models"),
        }
    }
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LoggedOut {
    pub remote_revoked: bool,
}
#[derive(Default)]
pub(crate) struct Auth {
    gate: Mutex<()>,
    events: std::sync::OnceLock<crate::Events>,
    state: Mutex<State>,
    endpoints: Endpoints,
    discovered: Mutex<bool>,
    generation: AtomicU64,
    #[cfg(test)]
    memory: Option<Mutex<Option<String>>>,
}
pub(super) fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
pub(super) fn random() -> Result<String, String> {
    let mut bytes = [0; 32];
    getrandom::fill(&mut bytes).map_err(|_| "gateway_random_failed")?;
    Ok(URL_SAFE_NO_PAD.encode(bytes))
}
fn random_host() -> Result<String, String> {
    let mut bytes = [0; 16];
    getrandom::fill(&mut bytes).map_err(|_| "gateway_random_failed")?;
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    Ok(format!(
        "urn:uuid:{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
        bytes[8], bytes[9], bytes[10], bytes[11], bytes[12], bytes[13], bytes[14], bytes[15]
    ))
}
impl Auth {
    pub(super) fn bind_events(&self, events: crate::Events) {
        let _ = self.events.set(events);
    }
    fn load(&self) -> Result<Record, String> {
        #[cfg(test)]
        if let Some(memory) = &self.memory {
            return memory
                .lock()
                .unwrap()
                .as_ref()
                .map(|s| serde_json::from_str(s).map_err(|_| "gateway_secret_invalid".into()))
                .unwrap_or_else(|| Ok(Record::default()));
        }
        crate::secrets::record_get(SERVICE, "accounts")
            .map_err(|_| "gateway_secret_unavailable")?
            .map(|s| serde_json::from_str(&s).map_err(|_| "gateway_secret_invalid".into()))
            .unwrap_or_else(|| Ok(Record::default()))
    }
    fn save(&self, record: &Record) -> Result<(), String> {
        let json = serde_json::to_string(record).map_err(|_| "gateway_secret_invalid")?;
        #[cfg(test)]
        if let Some(memory) = &self.memory {
            *memory.lock().unwrap() = Some(json);
            return Ok(());
        }
        crate::secrets::record_set(SERVICE, "accounts", &json)
            .map_err(|_| "gateway_secret_write_failed".into())
    }
    pub(super) fn begin(&self, base: &str, account: Option<&str>) -> Result<Started, String> {
        let _gate = self.gate.lock().map_err(|_| "gateway_auth_unavailable")?;
        let mut record = self.load()?;
        if record.host.is_empty() || record.host.starts_with("dex-host-") {
            record.host = random_host()?;
            self.save(&record)?;
        }
        let selected = account
            .map(|id| {
                record
                    .accounts
                    .iter()
                    .find(|a| a.id == id)
                    .ok_or("gateway_account_not_found")
            })
            .transpose()?;
        let pending = Pending {
            state: random()?,
            nonce: random()?,
            verifier: random()?,
            redirect: format!("{base}/auth/callback"),
            account: account.map(str::to_owned),
            client: selected.map(|a| a.client_id.clone()),
            expires_at: now() + 300,
        };
        let mut url = reqwest::Url::parse(AUTHORIZE).unwrap();
        {
            let mut query = url.query_pairs_mut();
            query.append_pair(
                "client_id",
                pending.client.as_deref().unwrap_or("dynamic_agent_client"),
            );
            if pending.client.is_none() {
                query.append_pair("agent_name_hint", "Dex");
            }
            query
                .append_pair("ext_agent_host_id", &record.host)
                .append_pair("response_type", "code")
                .append_pair("redirect_uri", &pending.redirect)
                .append_pair(
                    "scope",
                    "openid profile email offline_access resource.invoke chatgpt.tokens.use.direct",
                )
                .append_pair("resource", RESOURCE)
                .append_pair("state", &pending.state)
                .append_pair("nonce", &pending.nonce)
                .append_pair("code_challenge_method", "S256")
                .append_pair(
                    "code_challenge",
                    &URL_SAFE_NO_PAD.encode(Sha256::digest(pending.verifier.as_bytes())),
                );
            // Use the browser account selector; never put a retained token into RPC/UI URLs.
        }
        let started = Started {
            authorization_url: url.into(),
            expires_at: pending.expires_at * 1000,
        };
        *self.state.lock().unwrap() = State {
            pending: Some(pending),
            error: None,
        };
        self.generation.fetch_add(1, Ordering::SeqCst);
        Ok(started)
    }
    pub(crate) fn status(&self) -> Result<Status, String> {
        let _gate = self.gate.lock().map_err(|_| "gateway_auth_unavailable")?;
        let record = self.load()?;
        let mut state = self.state.lock().unwrap();
        if state
            .pending
            .as_ref()
            .is_some_and(|p| p.expires_at <= now())
        {
            state.pending = None;
            state.error = Some("gateway_auth_expired".into());
        }
        let active = record.accounts.iter().find(|a| {
            Some(&a.id) == record.active.as_ref()
                && !a.blocked
                && a.tokens.as_ref().is_some_and(valid_grant)
        });
        Ok(Status {
            state: if state.pending.is_some() {
                "signing_in"
            } else if state.error.is_some() {
                "error"
            } else if active.is_some() {
                "ready"
            } else {
                "signed_out"
            }
            .into(),
            active_account_id: active.map(|a| a.id.clone()),
            models: active.map(|a| a.models.clone()).unwrap_or_default(),
            error: state.error.clone(),
            accounts: record
                .accounts
                .iter()
                .map(|a| AccountView {
                    id: a.id.clone(),
                    label: format!(
                        "{} · {}",
                        a.email.as_deref().unwrap_or("ChatGPT"),
                        &a.id[..8]
                    ),
                    email: a.email.clone(),
                    signed_in: !a.blocked && a.tokens.as_ref().is_some_and(valid_grant),
                    plan: a.plan.clone(),
                    expires_at: a.tokens.as_ref().map(|t| t.expires_at.saturating_mul(1000)),
                })
                .collect(),
        })
    }
    pub(super) fn complete(&self, callback: &str) -> Result<(), String> {
        let _gate = self.gate.lock().map_err(|_| "gateway_auth_unavailable")?;
        let url = reqwest::Url::parse(callback).map_err(|_| "gateway_callback_invalid")?;
        let mut params = HashMap::new();
        for (k, v) in url.query_pairs() {
            if params.insert(k.into_owned(), v.into_owned()).is_some() {
                return Err("gateway_callback_invalid".into());
            }
        }
        let pending = self
            .state
            .lock()
            .unwrap()
            .pending
            .clone()
            .ok_or("gateway_auth_not_pending")?;
        let generation = self.generation.load(Ordering::SeqCst);
        let uri = format!(
            "{}://{}:{}{}",
            url.scheme(),
            url.host_str().unwrap_or(""),
            url.port_or_known_default().unwrap_or(0),
            url.path()
        );
        if uri != pending.redirect
            || params.get("state") != Some(&pending.state)
            || pending.expires_at <= now()
        {
            return Err("gateway_callback_invalid".into());
        }
        self.state.lock().unwrap().pending = None;
        let result = (|| {
            if params.contains_key("error") {
                return Err("gateway_auth_denied".into());
            }
            let client = match &pending.client {
                Some(client) if params.get("client_id").is_none_or(|v| v == client) => {
                    client.clone()
                }
                Some(_) => return Err("gateway_callback_invalid".into()),
                None => params
                    .get("client_id")
                    .filter(|v| {
                        !v.is_empty() && v.as_str() != "dynamic_agent_client" && v.len() <= 256
                    })
                    .cloned()
                    .ok_or("gateway_registration_incomplete")?,
            };
            let code = params
                .get("code")
                .filter(|v| !v.is_empty() && v.len() <= 4096)
                .ok_or("gateway_callback_invalid")?;
            self.discover()?;
            let response = call_json(
                transport::client()
                    .map_err(|_| "gateway_auth_transport")?
                    .post(&self.endpoints.token)
                    .form(&[
                        ("grant_type", "authorization_code"),
                        ("client_id", &client),
                        ("code", code),
                        ("code_verifier", &pending.verifier),
                        ("redirect_uri", &pending.redirect),
                        ("resource", RESOURCE),
                    ]),
            )?;
            let jwks = call_json(
                transport::client()
                    .map_err(|_| "gateway_auth_transport")?
                    .get(&self.endpoints.jwks),
            )?;
            let identity = validate_id(
                response["id_token"]
                    .as_str()
                    .ok_or("gateway_id_token_invalid")?,
                &jwks,
                &client,
                Some(&pending.nonce),
            )?;
            let tokens = tokens(&response, None)?;
            let mut record = self.load()?;
            let id = format!(
                "{:x}",
                Sha256::digest(format!("{ISSUER}\0{}\0{client}", identity.subject))
            );
            if pending.account.as_ref().is_some_and(|a| a != &id) {
                return Err("gateway_account_mismatch".into());
            }
            match record.accounts.iter_mut().find(|a| a.id == id) {
                Some(account) => {
                    account.email = identity.email;
                    account.plan = identity.plan;
                    account.tokens = Some(tokens);
                    account.pending_refresh = None;
                    account.models.clear();
                    account.blocked = false;
                }
                None => record.accounts.push(Account {
                    id: id.clone(),
                    subject: identity.subject,
                    client_id: client,
                    email: identity.email,
                    plan: identity.plan,
                    tokens: Some(tokens),
                    pending_refresh: None,
                    models: vec![],
                    blocked: false,
                }),
            }
            record.active = Some(id);
            // Cancellation and the final protected-store commit share one barrier.
            let _state = self.state.lock().unwrap();
            if self.generation.load(Ordering::SeqCst) != generation {
                return Err("gateway_auth_cancelled".into());
            }
            self.save(&record)
        })();
        self.state.lock().unwrap().error = result.as_ref().err().cloned();
        result
    }
    fn discover(&self) -> Result<(), String> {
        let mut checked = self.discovered.lock().unwrap();
        if *checked {
            return Ok(());
        }
        let value = call_json(
            transport::client()
                .map_err(|_| "gateway_auth_transport")?
                .get(&self.endpoints.discovery),
        )?;
        if value["issuer"] != ISSUER
            || value["authorization_endpoint"] != AUTHORIZE
            || value["token_endpoint"] != self.endpoints.token
            || value["jwks_uri"] != self.endpoints.jwks
            || value["revocation_endpoint"] != self.endpoints.revoke
        {
            return Err("gateway_discovery_invalid".into());
        }
        *checked = true;
        Ok(())
    }
    fn access_locked(&self, record: &mut Record, id: &str) -> Result<String, String> {
        if record.active.as_deref() != Some(id) {
            return Err("gateway_account_inactive".into());
        }
        let index = record
            .accounts
            .iter()
            .position(|a| a.id == id)
            .ok_or("gateway_account_not_found")?;
        let account = &record.accounts[index];
        let current = account.tokens.as_ref().ok_or("gateway_login_required")?;
        if account.blocked || !valid_grant(current) {
            return Err("gateway_login_required".into());
        }
        if account.pending_refresh.is_none() && current.expires_at > now() + 60 {
            return Ok(current.access_token.clone());
        }
        self.discover()?;
        // Prepare keys before consuming a potentially one-use refresh token.
        let mut jwks = call_json(
            transport::client()
                .map_err(|_| "gateway_auth_transport")?
                .get(&self.endpoints.jwks),
        )?;
        let refreshing = account.pending_refresh.is_none();
        if refreshing {
            let response = match call_json(
                transport::client()
                    .map_err(|_| "gateway_auth_transport")?
                    .post(&self.endpoints.token)
                    .form(&[
                        ("grant_type", "refresh_token"),
                        ("client_id", &account.client_id),
                        ("refresh_token", &current.refresh_token),
                        ("resource", RESOURCE),
                    ]),
            ) {
                Ok(response) => response,
                Err(code) => {
                    if permanent_auth(&code) {
                        self.block_locked(record, id)?;
                    }
                    return Err(code);
                }
            };
            // Recovery-only slot: inference reads only the verified `tokens` slot.
            record.accounts[index].pending_refresh = Some(PendingRefresh {
                response,
                received_at: now(),
            });
            self.save(record)?;
        }
        let account = &record.accounts[index];
        let pending = account.pending_refresh.as_ref().unwrap();
        let response = &pending.response;
        let mut renewed = tokens(response, account.tokens.as_ref())?;
        renewed.expires_at = pending.received_at + response["expires_in"].as_u64().unwrap();
        let identity = if let Some(id_token) = response["id_token"].as_str() {
            // Signing keys may rotate between prefetch and refresh. A transient
            // retry failure leaves the rotation in the protected store.
            if refreshing && validate_id(id_token, &jwks, &account.client_id, None).is_err() {
                jwks = call_json(
                    transport::client()
                        .map_err(|_| "gateway_auth_transport")?
                        .get(&self.endpoints.jwks),
                )?;
            }
            let identity = validate_id(id_token, &jwks, &account.client_id, None)?;
            if identity.subject != account.subject {
                return Err("gateway_account_mismatch".into());
            }
            Some(identity)
        } else {
            None
        };
        let access = renewed.access_token.clone();
        let refresh_recovered = !refreshing && renewed.expires_at <= now() + 60;
        let account = &mut record.accounts[index];
        account.tokens = Some(renewed);
        account.pending_refresh = None;
        if let Some(identity) = identity {
            account.plan = identity.plan;
        }
        self.save(record)?;
        if refresh_recovered {
            return self.access_locked(record, id);
        }
        Ok(access)
    }
    pub(super) fn access(&self, id: &str) -> Result<String, String> {
        let _gate = self.gate.lock().map_err(|_| "gateway_auth_unavailable")?;
        self.access_locked(&mut self.load()?, id)
    }
    pub(crate) fn models(&self) -> Result<Vec<Model>, String> {
        let _gate = self.gate.lock().map_err(|_| "gateway_auth_unavailable")?;
        let mut record = self.load()?;
        let id = record.active.clone().ok_or("gateway_login_required")?;
        let access = self.access_locked(&mut record, &id)?;
        let bearer = transport::secret_header(&format!("Bearer {access}"))
            .map_err(|_| "gateway_auth_invalid")?;
        let response = match call_json(
            transport::client()
                .map_err(|_| "gateway_auth_transport")?
                .get(&self.endpoints.models)
                .header("authorization", bearer),
        ) {
            Ok(response) => response,
            Err(code) => {
                if permanent_auth(&code) {
                    self.block_locked(&mut record, &id)?;
                }
                return Err(code);
            }
        };
        let rows = response["models"]
            .as_array()
            .filter(|v| v.len() <= 1024)
            .ok_or("gateway_models_invalid")?;
        let models = rows
            .iter()
            .filter(|r| r["visibility"] == "list")
            .map(|r| {
                let id = r["slug"]
                    .as_str()
                    .filter(|s| !s.is_empty() && s.len() <= 256)
                    .ok_or("gateway_models_invalid")?;
                let label = r["display_name"]
                    .as_str()
                    .filter(|s| !s.is_empty() && s.len() <= 256)
                    .ok_or("gateway_models_invalid")?;
                Ok(Model {
                    id: id.into(),
                    label: label.into(),
                })
            })
            .collect::<Result<Vec<_>, String>>()?;
        let account = record
            .accounts
            .iter_mut()
            .find(|a| a.id == id)
            .ok_or("gateway_account_not_found")?;
        account.models = models.clone();
        self.save(&record)?;
        Ok(models)
    }
    pub(crate) fn logout(&self, id: Option<&str>) -> Result<LoggedOut, String> {
        let _gate = self.gate.lock().map_err(|_| "gateway_auth_unavailable")?;
        self.state.lock().unwrap().pending = None;
        let mut record = self.load()?;
        let id = id
            .map(str::to_owned)
            .or_else(|| record.active.clone())
            .ok_or("gateway_account_not_found")?;
        let account = record
            .accounts
            .iter_mut()
            .find(|a| a.id == id)
            .ok_or("gateway_account_not_found")?;
        let refresh = account
            .pending_refresh
            .as_ref()
            .and_then(|p| p.response["refresh_token"].as_str())
            .or_else(|| account.tokens.as_ref().map(|t| t.refresh_token.as_str()));
        let remote_revoked = if let Some(refresh) = refresh {
            self.discover()
                .and_then(|_| transport::client().map_err(|_| "gateway_auth_transport".to_owned()))
                .and_then(|client| {
                    client
                        .post(&self.endpoints.revoke)
                        .timeout(std::time::Duration::from_secs(20))
                        .form(&[
                            ("token", refresh),
                            ("token_type_hint", "refresh_token"),
                            ("client_id", account.client_id.as_str()),
                        ])
                        .send()
                        .map_err(|_| "gateway_auth_transport".into())
                })
                .is_ok_and(|response| response.status().as_u16() == 200)
        } else {
            true
        };
        account.tokens = None;
        account.pending_refresh = None;
        account.plan = None;
        account.models.clear();
        if record.active.as_deref() == Some(&id) {
            record.active = None;
        }
        self.save(&record)?;
        self.state.lock().unwrap().error = None;
        Ok(LoggedOut { remote_revoked })
    }
    pub(super) fn cancel(&self) {
        let mut state = self.state.lock().unwrap();
        self.generation.fetch_add(1, Ordering::SeqCst);
        state.pending = None;
    }
    pub(super) fn invalidate(&self, id: &str) -> Result<(), String> {
        let _gate = self.gate.lock().map_err(|_| "gateway_auth_unavailable")?;
        let mut record = self.load()?;
        self.block_locked(&mut record, id)
    }
    fn block_locked(&self, record: &mut Record, id: &str) -> Result<(), String> {
        let mut changed = false;
        if let Some(account) = record.accounts.iter_mut().find(|a| a.id == id) {
            changed = !account.blocked || !account.models.is_empty();
            account.blocked = true;
            account.models.clear();
        }
        self.save(record)?;
        self.state.lock().unwrap().error = Some("gateway_login_required".into());
        if changed {
            if let Some(events) = self.events.get() {
                let _ = events.emit("gateway-auth-changed", &());
                let _ = events.emit("providers-changed", &());
            }
        }
        Ok(())
    }
    pub(crate) fn start_login(
        self: &Arc<Self>,
        account: Option<&str>,
        finished: impl Fn(bool) + Send + 'static,
    ) -> Result<Started, String> {
        use std::io::{BufRead, Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0")
            .map_err(|_| "gateway_callback_bind_failed")?;
        listener
            .set_nonblocking(true)
            .map_err(|_| "gateway_callback_bind_failed")?;
        let base = format!(
            "http://{}",
            listener
                .local_addr()
                .map_err(|_| "gateway_callback_bind_failed")?
        );
        let started = self.begin(&base, account)?;
        let state = self
            .state
            .lock()
            .unwrap()
            .pending
            .as_ref()
            .unwrap()
            .state
            .clone();
        let weak = Arc::downgrade(self);
        std::thread::spawn(move || {
            while let Some(auth) = weak.upgrade() {
                {
                    let mut current = auth.state.lock().unwrap();
                    if current.pending.as_ref().is_none_or(|p| p.state != state) {
                        break;
                    }
                    if current.pending.as_ref().unwrap().expires_at <= now() {
                        current.pending = None;
                        current.error = Some("gateway_auth_expired".into());
                        drop(current);
                        finished(false);
                        break;
                    }
                }
                let (mut socket, peer) = match listener.accept() {
                    Ok(v) => v,
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        drop(auth);
                        std::thread::sleep(std::time::Duration::from_millis(20));
                        continue;
                    }
                    Err(_) => break,
                };
                if !peer.ip().is_loopback() {
                    continue;
                }
                // Windows accepted sockets inherit the listener's nonblocking mode.
                if socket.set_nonblocking(false).is_err()
                    || socket
                        .set_read_timeout(Some(std::time::Duration::from_secs(3)))
                        .is_err()
                    || socket
                        .set_write_timeout(Some(std::time::Duration::from_secs(3)))
                        .is_err()
                {
                    continue;
                }
                let Ok(copy) = socket.try_clone() else {
                    continue;
                };
                let mut reader = std::io::BufReader::new(copy).take(8193);
                let mut first = String::new();
                if reader.read_line(&mut first).is_err() || first.len() > 8192 {
                    continue;
                }
                let mut headers = String::new();
                loop {
                    let mut line = String::new();
                    if reader.read_line(&mut line).unwrap_or(0) == 0 || line == "\r\n" {
                        break;
                    }
                    headers.push_str(&line);
                }
                let path = first.split_whitespace().nth(1).unwrap_or("");
                let request_ok = first.starts_with("GET /auth/callback?")
                    && first.trim_end().ends_with("HTTP/1.1")
                    && first.len() + headers.len() <= 8192
                    && headers.lines().any(|h| {
                        h.eq_ignore_ascii_case(&format!(
                            "host: {}",
                            base.strip_prefix("http://").unwrap()
                        ))
                    })
                    && !headers
                        .lines()
                        .any(|h| h.to_ascii_lowercase().starts_with("origin:"));
                let result = if request_ok {
                    auth.complete(&format!("{base}{path}"))
                } else {
                    Err("gateway_callback_invalid".into())
                };
                let text = if result.is_ok() {
                    "ChatGPT girişi tamamlandı. Dex'e dön."
                } else {
                    "Giriş tamamlanamadı. Dex'teki giriş durumunu kontrol et."
                };
                let response = format!("HTTP/1.1 {}\r\nContent-Type: text/plain; charset=utf-8\r\nCache-Control: no-store\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{text}",if result.is_ok(){"200 OK"}else{"400 Bad Request"},text.len());
                // A disconnected browser must not prevent the auth completion notification.
                let _ = socket
                    .write_all(response.as_bytes())
                    .and_then(|_| socket.flush());
                if auth.state.lock().unwrap().pending.is_none() {
                    finished(result.is_ok());
                    break;
                }
            }
        });
        Ok(started)
    }
    #[cfg(test)]
    pub(super) fn for_test() -> Self {
        Self {
            memory: Some(Mutex::new(None)),
            ..Default::default()
        }
    }
    #[cfg(test)]
    pub(super) fn pending_for_test(&self) -> Pending {
        self.state.lock().unwrap().pending.clone().unwrap()
    }
    #[cfg(test)]
    pub(super) fn set_host_for_test(&self, host: &str) {
        let mut record = self.load().unwrap();
        record.host = host.into();
        self.save(&record).unwrap();
    }
    #[cfg(test)]
    pub(super) fn host_for_test(&self) -> String {
        self.load().unwrap().host
    }
    #[cfg(test)]
    pub(super) fn for_test_endpoint(base: &str) -> Self {
        Self {
            endpoints: Endpoints {
                discovery: format!("{base}/discovery"),
                token: format!("{base}/token"),
                jwks: format!("{base}/jwks"),
                revoke: format!("{base}/revoke"),
                models: format!("{base}/models"),
            },
            ..Self::for_test()
        }
    }
    #[cfg(test)]
    pub(super) fn expire_for_test(&self) {
        let mut record = self.load().unwrap();
        for a in &mut record.accounts {
            if let Some(t) = &mut a.tokens {
                t.expires_at = 0;
            }
        }
        self.save(&record).unwrap();
    }
    #[cfg(test)]
    pub(super) fn seed_for_test(&self) {
        let id = "a".repeat(64);
        self.save(&Record {
            host: "synthetic-host".into(),
            active: Some(id.clone()),
            accounts: vec![Account {
                id,
                subject: "synthetic-subject".into(),
                client_id: "oaiapp_test".into(),
                email: None,
                plan: None,
                blocked: false,
                pending_refresh: None,
                tokens: Some(Tokens {
                    access_token: "synthetic-access".into(),
                    refresh_token: "synthetic-refresh".into(),
                    id_token: "synthetic-id".into(),
                    expires_at: now() + 3600,
                    scopes: vec![
                        "chatgpt.tokens.use.direct".into(),
                        "resource.invoke".into(),
                        "offline_access".into(),
                    ],
                }),
                models: vec![Model {
                    id: "allowed".into(),
                    label: "Allowed".into(),
                }],
            }],
        })
        .unwrap();
    }
    #[cfg(test)]
    pub(super) fn invalidate_for_test(&self) {
        let mut record = self.load().unwrap();
        record.accounts[0].tokens.as_mut().unwrap().scopes.clear();
        self.save(&record).unwrap();
    }
}
fn valid_grant(tokens: &Tokens) -> bool {
    !tokens.access_token.is_empty()
        && !tokens.refresh_token.is_empty()
        && [
            "chatgpt.tokens.use.direct",
            "resource.invoke",
            "offline_access",
        ]
        .iter()
        .all(|s| tokens.scopes.iter().any(|v| v == s))
}
fn permanent_auth(code: &str) -> bool {
    matches!(
        code,
        "gateway_login_required" | "gateway_account_ineligible" | "gateway_auth_rejected"
    )
}
fn call_json(request: reqwest::blocking::RequestBuilder) -> Result<Value, String> {
    use std::io::Read;
    let response = request
        .timeout(std::time::Duration::from_secs(20))
        .send()
        .map_err(|_| "gateway_auth_transport")?;
    let status = response.status().as_u16();
    let mut bytes = vec![];
    response
        .take(1024 * 1024 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "gateway_auth_transport")?;
    if bytes.len() > 1024 * 1024 {
        return Err("gateway_auth_protocol".into());
    }
    match status {
        429 => return Err("gateway_auth_rate_limited".into()),
        500..=599 => return Err("gateway_auth_unavailable".into()),
        401 => return Err("gateway_login_required".into()),
        _ => {}
    }
    let value: Value = serde_json::from_slice(&bytes).map_err(|_| "gateway_auth_protocol")?;
    if status != 200 {
        return Err(match value["error"]
            .as_str()
            .or_else(|| value["error"]["code"].as_str())
        {
            Some("invalid_grant") => "gateway_login_required",
            Some("user_not_eligible") => "gateway_account_ineligible",
            _ => "gateway_auth_rejected",
        }
        .into());
    }
    Ok(value)
}
fn tokens(response: &Value, old: Option<&Tokens>) -> Result<Tokens, String> {
    let required = |name: &str| {
        response[name]
            .as_str()
            .filter(|s| !s.is_empty() && s.len() <= 65536)
            .map(str::to_owned)
            .ok_or("gateway_token_invalid")
    };
    if response["token_type"] != "Bearer" {
        return Err("gateway_token_invalid".into());
    }
    let scopes = match response["scope"].as_str() {
        Some(s) => s.split_whitespace().map(str::to_owned).collect(),
        None => old
            .map(|t| t.scopes.clone())
            .ok_or("gateway_scope_missing")?,
    };
    if ![
        "chatgpt.tokens.use.direct",
        "resource.invoke",
        "offline_access",
    ]
    .iter()
    .all(|s| scopes.iter().any(|v| v == s))
    {
        return Err("gateway_scope_missing".into());
    }
    let expires = response["expires_in"]
        .as_u64()
        .filter(|n| *n > 0 && *n <= 604800)
        .ok_or("gateway_token_invalid")?;
    Ok(Tokens {
        access_token: required("access_token")?,
        refresh_token: response
            .get("refresh_token")
            .map(|_| required("refresh_token"))
            .unwrap_or_else(|| {
                old.map(|t| t.refresh_token.clone())
                    .ok_or("gateway_token_invalid")
            })?,
        id_token: response
            .get("id_token")
            .map(|_| required("id_token"))
            .unwrap_or_else(|| {
                old.map(|t| t.id_token.clone())
                    .ok_or("gateway_token_invalid")
            })?,
        expires_at: now() + expires,
        scopes,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, Read, Write};
    use std::time::{Duration, Instant};

    struct Recorded(std::sync::mpsc::Sender<(String, Value)>);
    impl crate::EventSink for Recorded {
        fn emit(&self, event: &str, payload: Value) -> Result<(), String> {
            self.0
                .send((event.into(), payload))
                .map_err(|e| e.to_string())
        }
    }
    fn record_events(auth: &Auth) -> std::sync::mpsc::Receiver<(String, Value)> {
        let (tx, rx) = std::sync::mpsc::channel();
        auth.bind_events(crate::Events::new(Recorded(tx)));
        rx
    }
    fn assert_invalidated(auth: &Auth, events: &std::sync::mpsc::Receiver<(String, Value)>) {
        assert_eq!(
            events.try_iter().collect::<Vec<_>>(),
            [
                ("gateway-auth-changed".into(), Value::Null),
                ("providers-changed".into(), Value::Null),
            ]
        );
        let status = auth.status().unwrap();
        assert!(status.active_account_id.is_none());
        assert!(status.models.is_empty());
        assert_eq!(status.error.as_deref(), Some("gateway_login_required"));
    }
    #[test]
    fn invalidate_emits_auth_and_provider_events_once() {
        let auth = Auth::for_test();
        let events = record_events(&auth);
        auth.seed_for_test();
        auth.invalidate(&"a".repeat(64)).unwrap();
        assert_invalidated(&auth, &events);
        auth.invalidate(&"a".repeat(64)).unwrap();
        assert!(events.try_recv().is_err());
    }
    #[test]
    fn refresh_invalid_grant_emits_auth_and_provider_events() {
        let fixture = Fixture::start(|path, _| match path {
            "/jwks" => (200, json!({"keys": []})),
            "/token" => (400, json!({"error": "invalid_grant"})),
            _ => panic!("unexpected auth route"),
        });
        let auth = Auth::for_test_endpoint(&fixture.base);
        *auth.discovered.lock().unwrap() = true;
        let events = record_events(&auth);
        auth.seed_for_test();
        auth.expire_for_test();
        let account = "a".repeat(64);
        assert_eq!(
            auth.access(&account).err().as_deref(),
            Some("gateway_login_required")
        );
        assert_invalidated(&auth, &events);
        // Inference also invalidates the resulting upstream-auth fault.
        auth.invalidate(&account).unwrap();
        assert!(events.try_recv().is_err());
    }

    struct Fixture {
        base: String,
        stop: Arc<AtomicBool>,
        accepted: std::sync::mpsc::Receiver<()>,
        thread: Option<std::thread::JoinHandle<()>>,
    }
    impl Fixture {
        fn start(mut reply: impl FnMut(&str, &str) -> (u16, Value) + Send + 'static) -> Self {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let base = format!("http://{}", listener.local_addr().unwrap());
            listener.set_nonblocking(true).unwrap();
            let stop = Arc::new(AtomicBool::new(false));
            let stopping = stop.clone();
            let (request_accepted, accepted) = std::sync::mpsc::channel();
            let thread = std::thread::spawn(move || {
                let deadline = Instant::now() + Duration::from_secs(15);
                while !stopping.load(Ordering::SeqCst) && Instant::now() < deadline {
                    let (mut socket, _) = match listener.accept() {
                        Ok(v) => v,
                        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                            std::thread::sleep(Duration::from_millis(5));
                            continue;
                        }
                        Err(e) => panic!("{e}"),
                    };
                    // Windows accepted sockets inherit the listener's nonblocking mode.
                    socket.set_nonblocking(false).unwrap();
                    socket
                        .set_read_timeout(Some(Duration::from_secs(2)))
                        .unwrap();
                    let mut reader = std::io::BufReader::new(socket.try_clone().unwrap());
                    let mut line = String::new();
                    reader.read_line(&mut line).unwrap();
                    let path = line.split_whitespace().nth(1).unwrap().to_owned();
                    request_accepted.send(()).unwrap();
                    let mut length = 0;
                    loop {
                        line.clear();
                        assert_ne!(reader.read_line(&mut line).unwrap(), 0, "incomplete HTTP headers");
                        if line == "\r\n" {
                            break;
                        }
                        if let Some(value) =
                            line.to_ascii_lowercase().strip_prefix("content-length:")
                        {
                            length = value.trim().parse().unwrap();
                        }
                    }
                    let mut body = vec![0; length];
                    reader.read_exact(&mut body).unwrap();
                    let (status, value) = reply(&path, &String::from_utf8(body).unwrap());
                    let json = value.to_string();
                    write!(socket, "HTTP/1.1 {status} Fixture\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{json}", json.len()).unwrap();
                }
            });
            Self {
                base,
                stop,
                accepted,
                thread: Some(thread),
            }
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::SeqCst);
            let result = self.thread.take().unwrap().join();
            if !std::thread::panicking() {
                result.unwrap();
            }
        }
    }

    #[test]
    fn fixture_rejects_truncated_headers_without_hanging_on_drop() {
        let fixture = Fixture::start(|_, _| panic!("truncated request must not reach the responder"));
        let mut socket =
            std::net::TcpStream::connect(fixture.base.strip_prefix("http://").unwrap()).unwrap();
        socket.write_all(b"POST /token HTTP/1.1\r\nHost: localhost\r\n").unwrap();
        fixture.accepted.recv_timeout(Duration::from_secs(3)).unwrap();
        socket.shutdown(std::net::Shutdown::Write).unwrap();
        drop(socket);
        let (done, finished) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let failed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| drop(fixture))).is_err();
            done.send(failed).unwrap();
        });
        assert!(finished.recv_timeout(Duration::from_secs(3)).expect("fixture drop hung on header EOF"), "incomplete HTTP headers must fail the fixture");
    }

    #[test]
    fn fixture_handles_fragmented_request() {
        let (sent, received) = std::sync::mpsc::channel();
        let fixture = Fixture::start(move |path, body| {
            sent.send((path.to_owned(), body.to_owned())).unwrap();
            (200, json!({"ok": true}))
        });
        let mut socket =
            std::net::TcpStream::connect(fixture.base.strip_prefix("http://").unwrap()).unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        socket.write_all(b"POST /token HTTP/1.1\r\n").unwrap();
        // Keep headers/body pending: a nonblocking server read exits here.
        assert!(matches!(
            received.recv_timeout(Duration::from_millis(100)),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout)
        ));
        socket
            .write_all(b"Host: localhost\r\nContent-Length: 13\r\n\r\ngrant=fixture")
            .unwrap();
        assert_eq!(
            received.recv_timeout(Duration::from_secs(2)).unwrap(),
            ("/token".into(), "grant=fixture".into())
        );
        // Read the HTTP frame; Windows may reset the connection on peer close.
        let mut reader = std::io::BufReader::new(socket);
        let mut headers = String::new();
        loop {
            let mut line = String::new();
            assert_ne!(reader.read_line(&mut line).unwrap(), 0);
            if line == "\r\n" {
                break;
            }
            headers.push_str(&line);
        }
        assert!(headers.starts_with("HTTP/1.1 200 Fixture\r\n"));
        assert!(headers.contains("\r\nContent-Length: 11\r\n"));
        let mut body = [0; 11];
        reader.read_exact(&mut body).unwrap();
        assert_eq!(&body, b"{\"ok\":true}");
    }

    #[test]
    fn fixture_drop_propagates_server_panic_without_unwinding() {
        assert!(std::panic::catch_unwind(|| {
            let fixture = Fixture {
                base: String::new(),
                stop: Arc::new(AtomicBool::new(false)),
                accepted: std::sync::mpsc::channel().1,
                thread: Some(std::thread::spawn(|| panic!("mock server panic"))),
            };
            drop(fixture);
        })
        .is_err());
    }

    #[test]
    fn fixture_drop_preserves_original_panic() {
        const CHILD: &str = "DEX_AUTH_FIXTURE_UNWIND_CHILD";
        if std::env::var_os(CHILD).is_some() {
            let original = std::panic::catch_unwind(|| {
                let _fixture = Fixture {
                    base: String::new(),
                    stop: Arc::new(AtomicBool::new(false)),
                    accepted: std::sync::mpsc::channel().1,
                    thread: Some(std::thread::spawn(|| panic!("mock server panic"))),
                };
                panic!("original test panic");
            })
            .unwrap_err();
            assert_eq!(
                original.downcast_ref::<&str>(),
                Some(&"original test panic")
            );
            return;
        }

        // A broken Drop aborts the child instead of taking down the core suite.
        let child = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "gateway::auth::tests::fixture_drop_preserves_original_panic",
                "--nocapture",
            ])
            .env(CHILD, "1")
            .output()
            .unwrap();
        assert!(
            child.status.success(),
            "fixture aborted during unwinding: {}\n{}",
            child.status,
            String::from_utf8_lossy(&child.stderr)
        );
    }

    #[test]
    fn refresh_rotation_survives_jwks_503_and_restart_without_exposing_pending_access() {
        let (id, jwks) = super::super::tests::synthetic_jwt(json!({
            "iss": ISSUER, "aud": "oaiapp_test", "sub": "synthetic-subject",
            "iat": now(), "exp": now() + 600,
            "https://api.openai.com/auth": {"chatgpt_plan_type": "pro"}
        }));
        let refreshes = Arc::new(AtomicU64::new(0));
        let calls = refreshes.clone();
        let mut key_calls = 0;
        let fixture = Fixture::start(move |path, body| match path {
            "/jwks" => {
                key_calls += 1;
                match key_calls {
                    1 => (200, json!({"keys": []})),
                    2 => (503, json!({})),
                    _ => (200, jwks.clone()),
                }
            }
            "/token" => {
                assert!(body.contains("refresh_token=synthetic-refresh"));
                if calls.fetch_add(1, Ordering::SeqCst) == 0 {
                    (
                        200,
                        json!({"access_token":"rotated-access", "refresh_token":"rotated-refresh", "id_token":id,
                        "token_type":"Bearer", "expires_in":3600}),
                    )
                } else {
                    (400, json!({"error":"invalid_grant"}))
                }
            }
            _ => panic!("unexpected auth route"),
        });
        let auth = Auth::for_test_endpoint(&fixture.base);
        *auth.discovered.lock().unwrap() = true;
        auth.seed_for_test();
        auth.expire_for_test();
        let account = "a".repeat(64);
        assert_eq!(
            auth.access(&account).err().as_deref(),
            Some("gateway_auth_unavailable")
        );
        let stored = serde_json::to_value(auth.load().unwrap()).unwrap();
        assert_eq!(
            stored["accounts"][0]["tokens"]["access_token"],
            "synthetic-access"
        );
        assert_eq!(
            stored["accounts"][0]["pending_refresh"]["response"]["refresh_token"],
            "rotated-refresh"
        );
        assert_eq!(stored["accounts"][0]["blocked"], false);
        let view = serde_json::to_value(auth.status().unwrap()).unwrap();
        assert!(view["accounts"][0].get("plan").is_none());
        assert!(!view.to_string().contains("rotated-"));
        let restarted = Auth::for_test_endpoint(&fixture.base);
        *restarted.discovered.lock().unwrap() = true;
        *restarted.memory.as_ref().unwrap().lock().unwrap() =
            auth.memory.as_ref().unwrap().lock().unwrap().clone();
        assert_eq!(restarted.access(&account).unwrap(), "rotated-access");
        assert_eq!(refreshes.load(Ordering::SeqCst), 1);
        let stored = serde_json::to_value(restarted.load().unwrap()).unwrap();
        assert!(stored["accounts"][0]["pending_refresh"].is_null());
        assert_eq!(
            stored["accounts"][0]["tokens"]["refresh_token"],
            "rotated-refresh"
        );
        assert_eq!(
            serde_json::to_value(restarted.status().unwrap()).unwrap()["accounts"][0]["plan"],
            "pro"
        );
    }

    #[test]
    fn jwks_prefetch_failure_does_not_consume_refresh_token() {
        let requests = Arc::new(Mutex::new(vec![]));
        let captured = requests.clone();
        let fixture = Fixture::start(move |path, _| {
            captured.lock().unwrap().push(path.to_owned());
            (503, json!({}))
        });
        let auth = Auth::for_test_endpoint(&fixture.base);
        *auth.discovered.lock().unwrap() = true;
        auth.seed_for_test();
        auth.expire_for_test();
        let before = serde_json::to_value(auth.load().unwrap()).unwrap();
        for _ in 0..2 {
            assert_eq!(
                auth.access(&"a".repeat(64)).err().as_deref(),
                Some("gateway_auth_unavailable")
            );
            assert_eq!(serde_json::to_value(auth.load().unwrap()).unwrap(), before);
        }
        assert_eq!(*requests.lock().unwrap(), vec!["/jwks", "/jwks"]);
    }

    #[test]
    fn pending_recovery_preserves_expiry_and_refreshes_expired_access() {
        for age in [120, 4000] {
            let (id, jwks) = super::super::tests::synthetic_jwt(json!({
                "iss": ISSUER, "aud": "oaiapp_test", "sub": "synthetic-subject",
                "iat": now(), "exp": now() + 600
            }));
            let fixture = Fixture::start(move |path, body| match path {
                "/jwks" => (200, jwks.clone()),
                "/token" => {
                    assert!(body.contains("refresh_token=rotated-refresh"));
                    (
                        200,
                        json!({"access_token":"fresh-access", "token_type":"Bearer", "expires_in":3600}),
                    )
                }
                _ => panic!("unexpected auth route"),
            });
            let auth = Auth::for_test_endpoint(&fixture.base);
            *auth.discovered.lock().unwrap() = true;
            auth.seed_for_test();
            auth.expire_for_test();
            let received_at = now() - age;
            let mut record = auth.load().unwrap();
            record.accounts[0].pending_refresh = Some(PendingRefresh {
                received_at,
                response: json!({"access_token":"pending-access", "refresh_token":"rotated-refresh", "id_token":id,
                    "token_type":"Bearer", "expires_in":3600}),
            });
            auth.save(&record).unwrap();
            let access = auth.access(&"a".repeat(64)).unwrap();
            if age < 3600 {
                assert_eq!(access, "pending-access");
                assert_eq!(
                    auth.load().unwrap().accounts[0]
                        .tokens
                        .as_ref()
                        .unwrap()
                        .expires_at,
                    received_at + 3600
                );
            } else {
                assert_eq!(access, "fresh-access");
            }
        }
    }

    #[test]
    fn mismatched_refresh_identity_stays_quarantined_and_logout_revokes_rotated_token() {
        let (id, jwks) = super::super::tests::synthetic_jwt(json!({
            "iss": ISSUER, "aud": "oaiapp_test", "sub": "other-account",
            "iat": now(), "exp": now() + 600,
            "https://api.openai.com/auth": {"chatgpt_plan_type": "untrusted-plan"}
        }));
        let refreshes = Arc::new(AtomicU64::new(0));
        let calls = refreshes.clone();
        let revoked = Arc::new(Mutex::new(String::new()));
        let revocation = revoked.clone();
        let fixture = Fixture::start(move |path, body| match path {
            "/jwks" => (200, jwks.clone()),
            "/token" => {
                calls.fetch_add(1, Ordering::SeqCst);
                (
                    200,
                    json!({"access_token":"untrusted-access", "refresh_token":"rotated-refresh", "id_token":id,
                    "token_type":"Bearer", "expires_in":3600}),
                )
            }
            "/revoke" => {
                *revocation.lock().unwrap() = body.to_owned();
                (200, json!({}))
            }
            _ => panic!("unexpected auth route"),
        });
        let auth = Auth::for_test_endpoint(&fixture.base);
        *auth.discovered.lock().unwrap() = true;
        auth.seed_for_test();
        auth.expire_for_test();
        let account = "a".repeat(64);
        for _ in 0..2 {
            assert_eq!(
                auth.access(&account).err().as_deref(),
                Some("gateway_account_mismatch")
            );
            assert_eq!(
                auth.load().unwrap().accounts[0]
                    .tokens
                    .as_ref()
                    .unwrap()
                    .access_token,
                "synthetic-access"
            );
            let view = serde_json::to_value(auth.status().unwrap()).unwrap();
            assert!(view["accounts"][0].get("plan").is_none());
            assert!(!view.to_string().contains("untrusted-"));
        }
        assert_eq!(refreshes.load(Ordering::SeqCst), 1);
        assert!(auth.logout(Some(&account)).unwrap().remote_revoked);
        assert!(revoked.lock().unwrap().contains("token=rotated-refresh"));
        assert!(auth.load().unwrap().accounts[0].pending_refresh.is_none());
        assert!(auth.load().unwrap().accounts[0].tokens.is_none());
    }

    #[test]
    fn legacy_secret_records_roundtrip_twice_without_changing_accounts_or_grants() {
        let auth = Auth::for_test();
        auth.seed_for_test();
        let mut legacy = serde_json::to_value(auth.load().unwrap()).unwrap();
        legacy["accounts"][0]
            .as_object_mut()
            .unwrap()
            .remove("plan");
        legacy["accounts"][0]
            .as_object_mut()
            .unwrap()
            .remove("pending_refresh");
        *auth.memory.as_ref().unwrap().lock().unwrap() = Some(legacy.to_string());
        for _ in 0..2 {
            let record = auth.load().unwrap();
            assert_eq!(record.accounts.len(), 1);
            assert_eq!(record.active, Some("a".repeat(64)));
            assert_eq!(
                record.accounts[0].tokens.as_ref().unwrap().refresh_token,
                "synthetic-refresh"
            );
            assert!(record.accounts[0].plan.is_none());
            assert!(record.accounts[0].pending_refresh.is_none());
            auth.save(&record).unwrap();
            let view = serde_json::to_value(auth.status().unwrap()).unwrap();
            assert!(view["accounts"][0].get("plan").is_none());
            assert!(view["accounts"][0]["expiresAt"].is_u64());
            assert!(!view.to_string().contains("synthetic-access"));
            assert!(!view.to_string().contains("synthetic-refresh"));
            assert!(!view.to_string().contains("synthetic-id"));
        }
    }
}
