# Oturum kullanımı ve API eşdeğer maliyeti

`dex-core::cost`, yerel çekirdek ve uzak dexd üzerinde aynı SQLite kayıtlarını ve
ayrıştırıcıyı kullanır. Uzak dosyalar sunucuda okunur; istemciye transcript içeriği
veya dosya yolu gönderilmez. Metotlar mevcut host kimlik doğrulamasını kullanır;
ajanların MCP araç listesine eklenmez. Host içindeki takım kimliği filtreleme
anahtarıdır, ayrı bir kullanıcı yetkisi değildir.

| RPC | Parametre | Sonuç |
|---|---|---|
| `usage_summary` | `{teamId, since?}` (`since`: UTC epoch ms, dahil) | `UsageSummary` |
| `usage_delegation` | `{id}` (host-local devretme kimliği) | `UsageSummary` |
| `agent_context` | `{agentId}` | `{tokens, window, model, sessionId, updatedAt}` |
| `team_context` | `{teamId}` | Ajan kimliğine göre context kayıtları (lider dahil) |
| `settings_usage_prices` | `{}` | `UsagePrice[]` |
| `settings_set_usage_prices` | `{prices: UsagePrice[]}` | Kaydedilmiş tabloyla birleştirilmiş katalog |

Tipler: `src/usage/costTypes.ts`. Özet, `total`, ajan kimliğine göre `agents`,
devretme kimliğine göre `delegations`, `costLabel` ve `warnings` içerir.
Her toplam `{tokens: {input, output, cacheRead, cacheWrite}, usd}` biçimindedir.
Cache read, input'a ikinci kez eklenmez; Codex reasoning output zaten output içindedir.
`costLabel` daima **API eşdeğer maliyeti**: abonelik faturası veya kalan kota değildir.

Fiyatlar **$/Mtok** biriminde, tam `provider` + transcript `model` anahtarıyladır.
`rates: {input, output, cacheRead, cacheWrite} | null`. Setter tüm özel fiyat
tablosunu değiştirir; atlanan modeller katalog varsayılanına döner. Fiyat değişince
geçmiş tokenların tahmini tutarı da yeni tabloyla hesaplanır. Negatif, sonlu olmayan
fiyatlar ve yinelenmiş anahtarlar reddedilir. Bir toplamın herhangi bir modelinin
fiyatı bilinmiyorsa toplam `usd: null`; bilinen parçalar sıfır sayılmaz.

Varsayılan katalog `providers::PROVIDERS` ve ölçülmüş transcript modellerinden
oluşur; **bütün fiyatlar bilinmiyor (`null`)**. Codex fiyat sağlayıcısı `openai`,
Claude yönlendirmelerinde `anthropic`, `glm`, `muse` veya `local` kullanılır.
Takma ada girilen fiyat gerçek model adına aktarılmaz. Doğrulanmış liste
fiyatlarını lider/patron girmelidir.

## Kayıt ve sınırlar

- `cost_sources`: motor/oturum kimliği, transcript yolu, byte ofseti ve Codex sayaç/model durumu.
- `cost_sessions`: aynı dosyada bile sağlayıcı/model/devretme değişiminde yeni
  dönem; ajan, takım, devretme, sağlayıcı, yapılandırılmış model ve başlangıç/bitiş.
- `cost_messages`: mesajın gerçek modeli, zaman damgası ve dört token sayacı.
  Konuşma metni saklanmaz/loglanmaz. Ajan veya devretme silinince maliyet geçmişi korunur.
- Hook'ta atıf kaydı oluşturulur. `SessionEnd` ve PTY çıkışı açık dönemi kapatır.
  Raporun ardından yazılan son cevap, yeni prompt başlayana kadar biten işe aittir.
- Özet isteği kayıtlı dosyaları kaldığı ofsetten okur; sohbetin açık olması gerekmez.
  Tamamlanmamış son satır sonraki okumaya kalır. Ofset, ayrıştırıcı durumu ve mesajlar
  aynı transaction ile kaydedilir. Claude mesaj kimliği tekrarlarını tekilleştirir;
  Codex kümülatif sayaç farkını alır, aynı sayaç tekrarını atlar.
- Kayıt başlangıcından önceki mesajlar yeni işe yüklenmez. Kurulum öncesinde kaybolmuş
  ajan/devretme ilişkileri tahmin edilmez. Bu özellik ileriye dönük ölçümdür.
- Eksik, okunamayan, küçülmüş veya bozuk dosya `warnings` üretir; önceki kalıcı
  sayımlar korunur. Uyarılı özet eksik olabilir. Dosya ilk okuma öncesinde silinirse
  tokenlar kurtarılamaz. İlk okuma geçmiş dosyayı tarar; sonraki okumalar artımsaldır.
- Abonelik indirimleri, vergi, uzun bağlam/servis katmanı farkları bu dört oranlı
  tahmine dahil değildir. Bilinmeyen modeli başka sağlayıcının fiyatıyla saymaz.

## Güncel context (migration 21)

Context maliyet toplamı değildir. Claude için son assistant mesajının
`input_tokens + cache_read_input_tokens + cache_creation_input_tokens` toplamıdır.
Output, ölçülen isteğin giriş penceresinde henüz bulunmadığından eklenmez; bu,
`/context` için transcript'ten elde edilebilen son giriş ölçümüdür (canlı tokenizer
hesabı değildir). Codex kendi `last_token_usage.total_tokens` ölçümünü kullanır;
yoksa input + output alınır. Cached input zaten input içinde, ikinci kez eklenmez.
Codex pencere boyutu aynı `token_count.info.model_context_window` alanından gelir.

Opus 5.5 / Sonnet 5.5 / Fable 5.1: 1M, Haiku 4.5: 200K; Muse 1.x için mevcut
provider tanımındaki 1.048.576 kullanılır. Bilinmeyen modeller (GLM dahil) `window:null`
döndürür; tahmini pencere üzerinden yüzde hesaplanmamalıdır. Ölçüm yoksa tokens=0,
updatedAt=null; ölçüm varsa updatedAt transcript zaman damgasıdır (epoch ms).

Yeni poll yoktur: context RPC ve hook, maliyetin aynı artımsal okuyucusunu çalıştırır.
Değişince `agent-context` olayı `{agentId, teamId, context}` yayınlanır. Takım RPC'si
host-local ajan kimliklerine göre nesne döndürür; bilinmeyen takım boş nesnedir,
bilinmeyen ajan hatadır. Yetki sınırı diğer host RPC'leriyle aynıdır.
`/clear`, yeni oturum ve temiz başlangıç eski ölçümü kaldırır; compaction sınırında
ilk yeni usage gelene kadar ölçüm bilinmiyor durumuna döner. Stop hook'u dosyanın son
flush'ından önce gelirse sonraki RPC/hook kalan satırı okur.

`cost_context_heads` yalnız güncel kaynak işaretçisi ve son yayınlanan ölçümü tutar.
Migration 21 additive ve idempotenttir. Eski parser durumuna sahip transcript bir
kez yeniden okunur; mesaj anahtarları maliyetin iki kez yazılmasını önler.
Normal geri alma önceki ikiliye dönüp tabloyu bırakmaktır. Fiziksel kaldırma yalnız
yedek + açık onayla, daha yeni migration yokken: `DROP TABLE IF EXISTS cost_context_heads;`
ve `PRAGMA user_version = 20;` aynı transaction içinde uygulanır. Maliyet tabloları
korunur. Aşağıdaki migration 20 kaldırma örneğinden önce migration 21 geri alınmalıdır.

## Migration 20 ve geri alma (maliyet tabloları)

Migration yalnız `CREATE TABLE/INDEX IF NOT EXISTS` kullanır. Mevcut uygulama
tablolarını veya satırlarını değiştirmez. Temiz DB'de iki uygulama ve veri varken
iki uygulama test edilir. Normal geri alma: önceki ikiliye dön, additive tabloları
bırak; eski kod onları kullanmaz, ölçülen geçmiş korunur.

Tabloları fiziksel kaldırma gerekirse önce Dex'i kapat ve SQLite backup al.
Aşağıdaki **veri kaybettiren** alternatif sadece `PRAGMA user_version = 20` ve
daha yeni migration yokken, açık onayla çalıştırılır; uygulama bunu otomatik yapmaz:

```sql
BEGIN IMMEDIATE;
DROP TABLE IF EXISTS cost_messages;
DROP TABLE IF EXISTS cost_sessions;
DROP TABLE IF EXISTS cost_sources;
DELETE FROM settings WHERE key = 'usage_prices';
PRAGMA user_version = 19;
COMMIT;
```

Kaldırmanın geri dönüşü alınan SQLite backup'ını geri yüklemektir.
