# Dex istemci ↔ `dexd` protokolü

**Protokol sürümü: `1`** (`crates/dexd/src/protocol.rs` → `PROTOCOL`). Uyumsuz her değişiklikte artar; istemci sürümü `auth.hello` / `auth.pair` içinde gönderir, farklıysa sunucu `protocol` hatasıyla reddeder.

## Taşıma

- Tek WebSocket bağlantısı, çoklanmış. Varsayılan adres `ws://127.0.0.1:7717` (`dexd --bind ADRES:PORT`).
- İstemci uzak adreslerde `wss://` kullanır. Şifresiz `ws://` yalnız loopback (`localhost`, `127.0.0.0/8`, `::1`) ve Tailscale (`100.64.0.0/10`, `*.ts.net` MagicDNS) adreslerinde kabul edilir; bu adreslerde şema yazılmazsa `ws://` seçilir. Diğer adreslerde (özel LAN dahil) açık `ws://` reddedilir; `wss://` için sunucu önünde TLS sonlandırma gerekir. Sunucu döngü dışı bind'de uyarı basar.
- S4 sunucu seçenekleri: `--root DİZİN` (tekrarlanabilir; istemcinin gezebileceği ve takım klasörü seçebileceği kökler, varsayılan `~/projects`, yoksa açılır) · `--secrets auto|keyring|file` (sağlayıcı jetonlarının yeri; `auto` = anahtarlık cevap veriyorsa o, vermiyorsa — başsız Linux, Secret Service/D-Bus yok — `<data_dir>/secrets/` altında `dex-provider.<id>` dosyaları, dizin 0700, dosya 0600). `file` modunda `$CREDENTIALS_DIRECTORY/<servis>.<hesap>` (systemd `LoadCredential=dex-provider.glm:/yol`) önce okunur ve salt-okunurdur.
- **Metin çerçeveleri** = JSON (aşağıda). **İkili çerçeveler** = PTY baytları.
- Sunucu yalnız doğrulanmış bağlantılara olay yollar.

## Kimlik doğrulama

İlk metin çerçevesi **10 sn içinde** bunlardan biri olmalı; başka her şey (veya zaman aşımı) cevapla birlikte bağlantıyı **4001** koduyla kapatır.
Bu mutlak süre TCP kabulünde başlar ve HTTP Upgrade'i de kapsar (eksik HTTP bağlantısı doğrudan kapatılır); uç başına en çok 32 doğrulanmamış bağlantı, gelen WS frame/mesaj başına 1 MiB kabul edilir. Ağ erişimi kapanınca veya port değişince o ucun tüm oturumları kesilir; masaüstünün yerel ucu etkilenmez.

| Metot | `params` | `result` |
|---|---|---|
| `auth.pair` | `{protocol, code, name?}` | `{protocol, server:"dexd", version, commit, commitTime, methods, device, token}` |
| `auth.hello` | `{protocol, token}` | `{protocol, server:"dexd", version, commit, commitTime, methods, device}` |

#142 (additive, protokol 1 kalır): `commit` = derleme commit'i (kısa sha, git yoksa `unknown`), `commitTime` = commit'in unix saniyesi (0 = bilinmiyor), `methods` = sunucunun cevapladığı bütün metot adları. Aynısı doğrulanmış bağlantıda `server_info` ile de alınır; masaüstünün yerel çekirdeği aynı adlı Tauri komutunu `methods: null` ile verir. İstemci (`src/api/hosts.ts` `hostSupports`) listede olmayan özelliği gizler; alan yoksa (eski dexd) metot denenir, `bilinmeyen metot: <m>` yedek yolu geçerli.

## Yerel çekirdek: `dexd --local` (#150)

Masaüstünün kendi çekirdeği ayrı bir süreç: `dexd --local --data-dir <uygulama veri dizini> [--config-dir …]` (uygulama başlatır: `src-tauri/src/core_proc.rs`; ayrıntı `crates/dexd/src/local.rs`).

- Uygulamanın `dex.db`'si, tohumlu `Core::start`; veri dizini başına tek örnek (`dex.lock`; ikincisi hata verip çıkar).
- Yalnız `127.0.0.1:<rastgele>`; kimlik: her başlangıçta yeni 256 bit jeton, `auth.hello {protocol, token, probe?}` → `device.id = "local"`. Eşleme/cihaz jetonu yok, kök denetimi yok (klasör seçici). Origin yalnız uygulamanın webview'ı ya da yok (#192).
- Keşif: `<data_dir>/dexd.local.json` = `{pid, port, url, tokenFile, commit, commitTime, startedAt}`, jeton ayrı dosyada (`dexd.local.token`); ikisi de yalnız kullanıcıya açık (Windows: korumalı DACL, tek ACE), atomik yazılır, çıkışta silinir.
- Ömür: bağlı pencere yokken **120 sn** sonra ajanları kapatıp çıkar (başlangıçta da sayar); bu sürede bağlanan pencere onu tutar. `probe: true` ile gelen hello (kabuğun "ayakta mı" yoklaması) pencere sayılmaz.
- Yalnız yerel uçta (listede de yalnız orada): `core_shutdown`, `core_restart` (aynı ikilinin yeni örneği; eskisi veri dizinini bırakınca açılır), `tailnet_get`, `tailnet_set {enabled, port}`, `tailnet_pair_code`, `tailnet_devices`, `tailnet_device_revoke {id}` (tailnet dinleyicisi artık bu süreçte; o uçtan bağlananlar eşlenmiş cihaz, kök denetimi açık). Eşlenmiş cihaz bunları çağırırsa `local_only`.

- `device` = `{id, name, createdAt, lastSeen}` (unix sn).
- Kod: sunucuda `dexd pair` → `ABCD-EFGH` (31 harfli alfabe, 0/O/1/I/L yok), **10 dk**, **tek kullanımlık**; tire/küçük harf fark etmez.
- `token`: 256 bit rastgele (64 hex), **yalnız bu cevapta** döner. Sunucu kodları da jetonları da yalnız SHA-256 özetiyle saklar (`<data_dir>/dexd.db`).
- İptal: `dexd devices revoke <id>` → jeton artık doğrulanmaz; açık bağlantı en geç **15 sn** içinde **4003** ile kapanır. `dexd devices list` cihazları listeler.

## İstek / cevap

```json
→ {"id": 7, "method": "task_list", "params": {"teamId": "t1"}}
← {"id": 7, "result": [ ... ]}
← {"id": 7, "error": {"code": "failed", "message": "…"}}
```

- `id` istemcinindir, aynen geri döner. İstekler eşzamanlı işlenir; cevap sırası istek sırası değildir.
- `params` eksikse `{}` sayılır; eksik anahtar `null`'dır (opsiyonel argüman).

| Hata `code` | Anlamı |
|---|---|
| `unauthorized` | Doğrulanmamış bağlantı / geçersiz kod ya da jeton |
| `protocol` | Protokol sürümü uyuşmuyor |
| `bad_request` | Çözülemeyen JSON, eksik/yanlış tipte argüman, kullanımda kanal |
| `unknown_method` | Böyle bir metot yok |
| `failed` | Çekirdek reddetti; `message` kullanıcıya gösterilecek metindir (Tauri `invoke` reddiyle aynı) |
| `internal` | Sunucu içi hata |

## Olaylar

```json
← {"event": "tasks-changed", "payload": null}
```

Çekirdek olayları, masaüstündeki Tauri `emit` konularıyla **aynı ad ve payload**: `teams-changed`, `tasks-changed`, `delegations-changed`, `agent-status`, `dex-hook`, `dex-agent-error`, `dex-first-reply` (FIX-3: `{agentId, late, seconds, text, leader}`), `agent-session` (FIX-4: `{agentId, sessionId, engine}` — `--resume` ile açılabilecek oturum; `sessionId: null` = kayıt silindi), `dex-resume-lost` (FIX-4: `{agentId, sessionId, text}` — kayıtlı oturum bulunamadı, resume'suz yeniden başlatıldı), `dex-chain`, `dex-chain-wait`, `dex-chain-available`, `questions-changed`, `plans-changed`, `proposals-changed`, `notes-changed` (payload: yazılan not `{scope, body, updatedAt}`), `chat-message` (S5-B: `{agentId, message}` — transcript'e eklenen satır; `message` = `{id, seq, role, text, thinking?, ts?, tool?}`), `chat-reset` (S5-B: `{agentId}` — transcript değişti, geçmişi yeniden çek), `chat-queue` (S5-B: `{agentId, messages}` — ajana sırada bekleyen metinler), `delegation-progress` (#2055: worker'ın Claude görev listesinden çalışan işin ilerlemesi, `delegation_progress` satırıyla aynı şekil; `active` = süren madde metni ya da `null`), `leader-reset` (Görev #51: `{agentId, phase, sent, error}` — `phase` = `writing | timeout | restarting`, `null` = bitti/iptal). Her doğrulanmış bağlantıya yayınlanır. `agent-status` payload'ı `{agentId, status, sessionId, engine}`; `status` = `offline | idle | working | waiting`.

Bağlantı düzeyi olaylar (nokta ad alanlı):

| Olay | `payload` |
|---|---|
| `pty.exit` | `{channel, id}` — kanalın PTY'si bitti (ya da `pty_spawn` ile yerine yenisi kondu) |
| `pty.size` | `{channel, id, cols, rows}` — #62: PTY'nin boyutu değişti (çıktıyla aynı sırada); görünüm terminalini bu boyuta çeker |
| `dexd.lagged` | `{missed}` — bağlantı olay kuyruğunun gerisinde kaldı; istemci gösterdiği listeleri yeniden çekmeli |

## Metotlar

Çekirdek metotları, `src-tauri`'deki Tauri komutlarıyla **birebir aynı ad, aynı camelCase argüman adları, aynı sonuç** (`dexd`'deki `methods::tests::covers_every_tauri_command` sapmayı yakalar):

| Alan | Metotlar |
|---|---|
| Görev | `task_list {teamId?}`, `task_create {task}`, `task_update {id, patch}`, `task_delete {id}` |
| Takım | `team_list`, `team_create {team}`, `team_update {id, patch}`, `team_delete {id}`, `team_set_workers_model {teamId, target}` |
| Ajan | `agent_list`, `agent_create {agent}`, `agent_update {id, patch}`, `agent_delete {id}`, `agent_apply_pending {id}`, `agent_start {id, options?}`, `agent_status_list`, `agent_input {id, draft}`, `leader_reset {id, action}`, `role_list` |
| Sohbet (S5-B) | `chat_history {agentId, before?, limit?}` — sayfalı transcript (`{messages, cursor, hasMore}`), `chat_send {agentId, text}` — kapalıysa `agent_start` ile başlatır, boştaysa yapıştırır, meşgulse kuyruğa alır (`started \| sent \| queued`), `chat_screen {agentId}` — PTY'nin görünen ekranı (bekleyen soru) |
| Devretme | `delegation_list`, `delegation_seen {agentId}`, `delegation_stalled {agentId, minutes}`, `delegation_progress` (#2055: `[{agentId, delegationId, total, done, active, updatedAt}]`) — başlatma/kuyruk/teslim sunucu tarafındadır |
| İlk yanıt (FIX-3) | `agent_first_reply_retry {id}`, `agent_first_reply_cancel {id}`, `first_reply_get`, `first_reply_set {provider, secs}` |
| Soru | `question_list`, `question_answer {id, text}` |
| Plan / öneri | `plan_list`, `proposal_list`, `proposal_decide {id, approve, name?}` — plan devamı sunucu açılışında bir kez koşar |
| Sağlayıcı | `provider_list`, `provider_import_routes`, `provider_set_token {id, token}`, `provider_local_set_base_url {url}`, `provider_local_status`, `provider_local_add {name}`, `provider_local_delete {name}` |
| Motor zinciri (DEX-17) | `chain_get` — `{order, exhausted}`, `chain_set_order {order}`, `chain_set_agent {id, off}`, `chain_back {id}`. Varsayılan sıra `anthropic → muse → glm → local`. Ajan kaydında asıl `engine/provider/model` değişmez; geçici hedef `activeEngine/activeProvider/activeModel`, dönüş uygunluğu `chainReturnReady` alanlarındadır. Olaylar: `dex-chain {agentId, from, to, activeEngine, activeProvider, resetAt, text}`, `dex-chain-wait {agentId, text}`, `dex-chain-available {provider, text}`. |
| Ayar | `limits_get`, `limits_set {limits}` |
| Maliyet | `usage_summary {teamId, since?}`, `usage_delegation {id}`, `settings_usage_prices`, `settings_set_usage_prices {prices}` — token ve API eşdeğer maliyeti; [sözleşme ve fiyatlar](usage-cost.md) |
| Notlar (DEX-41) | `note_get {scope}`, `note_set {scope, body}` — `scope` = `"shared"` ya da takım id'si |
| Yerleşim | `layout_load`, `layout_save {layout}` |
| PTY | `pty_spawn` (aşağıda), `pty_write {id, data}`, `pty_resize {id, cols, rows, claim?}` (#62: bağlantının görünüm boyutu; `claim` = patron burada yazdı/odaklandı, boyut bu bağlantının olur — yoksa yalnız boyut sahibi ya da sahipsiz PTY'de uygulanır), `pty_kill {id}` |

S4: `team_create {team.cwd}` ve `team_update {patch.cwd}` sunucuda `--root` kökleri dışındaki bir klasörü `failed` ile reddeder (masaüstü çekirdeğinde bu kısıt yok). Yalnız masaüstünde olan komutlar: `host_token_get {id}`, `host_token_set {id, token}`, `host_token_delete {id}` (istemcinin kendi anahtarlığı; sunucu karşılığı yok).

Bağlantı / oturum metotları (nokta ad alanlı, Tauri karşılığı yok):

| Metot | `params` | `result` |
|---|---|---|
| `pty.list` | — | `[{id, kind, cols, rows, alive, startedAt}]` — çalışan PTY'ler |
| `pty.attach` | `{id, channel}` | `{subscriber, snapshot}` — snapshot'tan sonra kanal canlı çıktıyı alır |
| `pty.detach` | `{channel}` | `null` — kanal susar, süreç çalışmaya devam eder |
| `fs.list` (S4) | `{path}` — `null` = kökler | `{path, parent, entries:[{name, path}]}` — yalnız klasörler, `.` ile başlayanlar ve kök dışına çıkan bağlar hariç; kökte `parent: null`. Kök dışı yol → `failed` |
| `fs.mkdir` (S4) | `{parent, name}` | yeni klasörün yolu — `name` tek parça (`/ \ :` yok, `.`/`..` değil), `parent` kök içinde |

## PTY kanalları

- `pty_spawn` = Tauri komutunun argümanları (`id, cmd, args?, cwd?, memoryDir?, provider?, cols, rows`) + **`channel`** (u32, `0` ayrılmış). Kanal numarasını **istemci seçer** ve istekten önce kaydeder: çıktı cevaptan önce gelebilir.
- Kanal bağlantıya özeldir; aynı bağlantıda kullanımdaki bir kanal yeniden verilemez (`bad_request`).
- **İkili çerçeve**, iki yön de: `[kanal: u32 big-endian][ham baytlar]`. Sunucu → istemci: PTY çıktısı. İstemci → sunucu: klavye girdisi (sırası korunur). Bilinmeyen kanala yazılan çerçeve yok sayılır.
- PTY'ler **sunucunundur**, bağlantının değil: bağlantı koparsa kanalları susar, süreçler çalışır; yeni bağlantı `pty.list` + `pty.attach` ile geri bağlanır (istemci taşıma katmanı bunu otomatik yapar).
- `snapshot`: `{scrollback, ansi, cursor, rows, cols, alternateScreen}`. `scrollback` ve `ansi` bayt dizileridir. İstemci önce scrollback'i, sonra ANSI ekran durumunu, son olarak attach sırasında tamponladığı canlı baytları yazar. Snapshot üretimi ve abone ekleme aynı kilit altında olduğundan sınırdaki çıktı kaybolmaz veya iki kez görünmez.

## İstemci tarafı (`src/api/transport.ts`, `src/api/hosts.ts`)

- `TauriTransport`: süreç içi çekirdek — `invoke` / `listen` / `Channel`. `WsTransport`: bu protokol. Kopunca üstel geri çekilmeyle (en çok 10 sn) yeniden bağlanır, `auth.hello` yapar, açık kanalları `pty.attach` ile geri takar; 4001/4003'te yeniden denemez. Durum: `connecting | connected | reconnecting | unauthorized`.
- #150 `LocalTransport`: yerel çekirdek (`dexd --local`) için `WsTransport`; adresi sabit değil — her yeniden bağlanmadan önce kabuğa (`local_host`) yeniden sorar, cevap yoksa durum `down` (kendiliğinden başlatılmaz; `local_start` = "Çekirdeği başlat"). Kabukta kalan komutlar `src/api/clientOnly.ts` (`CLIENT_ONLY`); `inproc-core` derlemesinde `local_host` null → `TauriTransport`.
- **S4 — çoklu host:** pencere aynı anda yerel çekirdeği (`local`) ve her eşlenmiş `dexd`'yi tutar (`hosts.ts`). Takım tek bir host'a aittir; ona dair her çağrı o host'a gider. Bir host'tan gelen kimlikler istemcide `<host>:<id>` olur (takım, ajan, PTY, not kapsamı; satır kimlikleri — görev/devretme/soru/öneri — `host` alanıyla birlikte tutulur), sunucuya daima çıplak kimlik gider. Yerel kimlikler S4 öncesiyle aynı (önekiz).
- Olaylar her host'tan dinlenir (`onEvent(event, (e, host) => …)`); host (yeniden) bağlanınca ya da `dexd.lagged` gelince o host'un listeleri yeniden çekilir (`onHostUp`).
- Host listesi (ad, adres, cihaz id'si) `localStorage["dex.hosts"]`'ta; cihaz jetonu OS anahtarlığında (`host_token_*`, servis `dex-host`). S2'nin `dex.server` kaydı açılışta bir kez listeye + anahtarlığa taşınır.
- Yerleşim (`layout_load/save`) ve ortak not (`"shared"`) her zaman yereldir; takım notu, görevler, devretmeler, sorular, öneriler, sağlayıcılar, sınırlar ve ilk yanıt eşikleri takımın host'undandır.
