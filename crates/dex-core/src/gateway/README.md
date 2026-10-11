# Core gateway (#848 spike → #1072 entegrasyonu)

Core sahipli `127.0.0.1:0` HTTP/1 ingress, `/v1/messages` üzerinden çalışır.
`Gateway::register` yerel capability'yi tek provider/account/model route'una bağlar; `execute`
Messages girdisini taşır/çevirir, başarılı JSON'u veya sınıflandırılmış `Fault`u
döndürür. Streaming olayları callback'e gider. Hatalar callback'te **bir** Anthropic
error envelope üretir; chain'in kullanacağı provider/account/model/request_id,
HTTP status ve varsa gerçek Retry-After bilgisi `Fault`ta bulunur. Callback'i
JSON/SSE yanıtını `ingress.rs` taşır. İş/oturum bağı DB'den doğrulanır; iş bitişi,
SessionEnd, PTY kapanışı, hesap değişimi ve Core shutdown yerel jetonu revoke eder.
16 eşzamanlı bağlantı; 64 KiB header, 8 MiB body ve 10 sn giriş okuma sınırı vardır.

## Sözleşme

- `AnthropicMessages`: `/v1/messages`, seçilmiş model, bearer veya x-api-key;
  gövde (beta/cache/stream dahil) korunur. Yalnız anthropic-beta/version başlıkları
  upstream'e aktarılır; istemcinin auth başlıkları kullanılmaz. SSE terminal
  message_delta, message_stop gelene kadar tutulur.
- `OpenaiResponses`: `/v1/responses`, yalnız upstream bearer, stream=true,
  store=false; function schemas, text/image, tool_use/result, summary/opaque
  encrypted reasoning ve medium/high/xhigh eşlenir. API-key profili flat tools;
  SIWC profilleri developer `additional_tools` veya `dex` namespace kullanır.
  SIWC yasak alanları allowlist üretimiyle dışarıda kalır, system→developer olur.
- `OpenaiChat`: `/v1/chat/completions`, yalnız upstream bearer; text/image,
  function/tool history, argument fragmentleri, reasoning_effort ve paralellik.
  Sol bu rotada reddedilir; Responses kullanılması gerekir.

Responses yalnız `response.completed` ile başarılı biter; failed/incomplete/EOF
ve hatalı arguments başarısızdır. Chat finish_reason yanında `[DONE]` ister.
Paralel upstream tool blokları Anthropic'e sırayla açılır; sonraki blok öncekinin
kapanmasını bekler. stream=false istemci için OpenAI upstream yine SSE kullanır,
başarı tam Anthropic JSON'a birikir. Usage/cache sayıları fake fixture'dan eşlenir.

Revoke devam eden isteği ilk izleyen okuma/olayda keser; egress toplam deadline 120 sn.
Başarılı terminal frame'in yetki kontrolünden geçmesi completion commit noktasıdır;
ondan sonraki revoke gelecek isteklere uygulanır, başarıdan sonra error üretilmez.
İki eşzamanlı istek ayrı reducer ve ayrı credential taşır. Retry, fallback, araç
icrası yoktur. SIWC refresh, hesap/istemci bağını koruyan merkezî seri broker'dadır.

Replay signature Raine'in taşınabilir opaque-reasoning yaklaşımının dar uyarlamasıdır:
`dex:responses:v1:` + JSON envelope (provider/account/model scope + reasoning item).
Scope'u farklı, yabancı veya çözülemeyen replay atılır; oturumun diğer içeriği sürer.
Blob çözülmez ve gateway tarafından diske yazılmaz. Bu format
Raine/native Anthropic signature'ıyla wire-compatible değildir; kriptografik imza
veya kimlik doğrulama kanıtı değildir. Secret/prompt içeren tipler Debug/Serialize
implement etmez. Modelin istemciye gönderdiği içerik bellek içinde kalır.

## Provenance / lisans

Kaynak: [raine/claude-code-proxy v0.1.44](https://github.com/raine/claude-code-proxy/tree/4b5c058a3c900031d2b2e094a265a5ade7acad3a),
commit `4b5c058a3c900031d2b2e094a265a5ade7acad3a`, MIT,
Copyright (c) 2026 Raine Virta. Tam telif/lisans bildirimi [LICENSE-raine](LICENSE-raine).

- `request.rs`: upstream request.rs'teki function/input/image, tool_choice,
  parallel_tool_calls, effort ve developer/additional_tools tasarımı yeniden
  yazılarak dar uyarlandı. Global effort/env, Read rewrite, compaction, hosted tools,
  private lite metadata ve parallel=false zorlaması alınmadı.
- `stream.rs`: reducer.rs/live_stream.rs/stream.rs/accumulate.rs'teki request-local
  blok biriktirme, SSE→Anthropic ve JSON accumulator tasarımı dar uyarlandı.
- reasoning_signature.rs'teki erken/done metadata birleştirme ve opaque replay
  fikri scope kontrollü JSON formatıyla yeniden yazıldı; base64 dependency eklenmedi.
- `mod.rs`, loopback-only `transport.rs`, authorization/revoke/error sınıfları ve
  fake fixture testleri Dex için yeni yazıldı. Raine auth/private backend/CLI/TUI/capture
  kodu taşınmadı. Üretim auth/HTTPS/ingress bağımlılıkları Dex entegrasyonudur;
  LICENSE birebir korundu.

## Test ve sınır

`cargo test -p dex-core --lib gateway::tests` gerçek HTTP isteğiyle yalnız
127.0.0.1:0 fake upstream'lere çıkar. Sentetik fixture'lar worktree
`.dex/tmp/848/fixtures` altında testte üretilir. Canary gerçek test sürecini iki kez
başlatır, trace-level log/diagnostic/stdout/stderr ve spike artifact'larını denetler;
gerçek kullanıcı home/keyring/token, Claude/Codex CLI veya model kullanılmaz.

Üretim egress `reqwest` + rustls/ring ile yalnız public
`https://api.openai.com/v1/responses` kullanır. Loopback hedef fake testler içindir.
Proxy/env keşfi, redirect, otomatik retry ve TLS key logging kapalıdır.
İstek düzeyindeki timeout tüm akan SSE için 120 sn; bağlantı kurma sınırı 3 sn.
`ureq` 2.12.1 debug prelude x-api-key'yi maskelemediğinden canary testi önce kırmızı
oldu. Üretim transport hassas Authorization/x-api-key başlıklarını işaretler;
iki yeni süreçte trace canary ile header/prompt sızmadığı doğrulanır.

Fixed thinking.budget_tokens, hosted/deferred tools, context_management, structured
output ve Chat opaque reasoning açık Unsupported
hatasıdır; sessiz placeholder/fallback yoktur. Sol none/minimal reddedilir;
thinking.disabled→none zorlaması yoktur. SIWC namespace/additional_tools burada
wire fixture kanıtıdır; gerçek hesap/modelin kabulü canlı girişten sonra ölçülür.
Bilinmeyen SSE olayları atılır, log yalnız sınırlı olay türü içerir. Refusal metni
Anthropic text'e; kota/rate limit rate_limit_error'a, 503 overloaded_error'a eşlenir.

## SIWC ve host uçları

Resmî [registration/sign-in](https://developers.openai.com/siwc/token-sharing-open-source/sign-in),
[models/inference](https://developers.openai.com/siwc/token-sharing-open-source/models-and-inference),
[sessions](https://developers.openai.com/siwc/token-sharing-open-source/profiles-and-sessions)
sözleşmeleri uygulanır. `dynamic_agent_client`, kalıcı opak host ID, state/nonce/PKCE,
issued client ID ve RS256/JWKS doğrulaması kullanılır; Codex tokenı/özel endpoint yoktur.
Her hesap `(issuer, subject, client_id)` ile ayrılır. Credential kaydı Dex secrets
deposunda iki slotla atomik yenilenir; Windows giriş boyutu için parçalanır. Prompt,
payload, replay ve capture gateway tarafından kalıcı yazılmaz. Test RSA anahtarı
`test-key.json` yalnız sentetik, herkese açık fixture'dır; üretim doğrulaması kullanmaz.

- `gateway_auth_start({accountId?: string})` → `{authorizationUrl, expiresAt}`.
- `gateway_auth_status()` → `{state, activeAccountId, accounts, models, error}`.
  Hesap görünümü `{id, label, email, signedIn, plan?: string, expiresAt?: number}`;
  `plan` yalnız doğrulanmış ID token claim'inden, `expiresAt` erişim jetonunun
  epoch-ms süresinden gelir. Jeton değerleri ve bekleyen rotasyon RPC'ye çıkmaz.
- `gateway_auth_logout({accountId?: string})` → `{remoteRevoked}`.
- `gateway_models()` → `{id, label}[]`; yalnız hesabın görünür katalog öğeleri.

Giriş UI'ı URL'yi sistem tarayıcısında açar. Ayrı `127.0.0.1:0/auth/callback`
dinleyicisi tek kullanımlık ve 5 dk ömürlüdür; başarısız state denemesi hakkı tüketmez.
Sonuç/zaman aşımı `gateway-auth-changed` ve `providers-changed` yayımlar.
Refresh öncesi JWKS hazırlanır; dönen rotasyon doğrulama bitene kadar sır deposunda
bekler ve sonraki denemede toparlanır. Bir SSE isteğinin iptali ortak iş jetonunu
revoke etmez; lease iş/oturum/hesap yaşam döngüsünde sonlandırılır.
Tarayıcı ve Core aynı hostta olmalıdır; uzak dexd giriş uçları
`giriş yalnız yerel Dex'ten` hatası döndürür. Yerel daemon `dexd --local` akışıdır.

Provider `gateway`, engine `claude`; model kataloğu hesap kaynaklıdır. Gateway zincire
yalnız `chain_set_order` ile açık seçimle girer; mevcut varsayılan sıra korunur.
DB migration yoktur. UI ayrı iş; canlı SIWC/model isteği patron girişini bekler.
