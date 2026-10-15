# Symbiote kart — A · Doku: frontend devri

Patron #1937'de A'yı seçti; varlıklar #1957'de geldi. Bu klasörde yalnız veri var, hiçbir modül bunları import etmiyor. Renderer frontend'in işi. Haritaları yeniden üretmek için: `tools/symbiote-cards/README.md`.

Tasarım dili: zemin canlı ve ıslak bir zar, ışık içeriden geliyor. Core'un kırmızı gözü zemini boyar, nabız Core'dan damarlarla dışa yayılır. Worker'lar yavru keseleri; ortadaki çekirdekleri durum renginde yanar. Echo soğuk camgöbeği bir medüz; zemine değmeyen tek varlık o.

## 1. Dosyalar ve boyut

| Varlık | Dosya | Boyut | Çözünürlük | Wrap |
|---|---|---|---|---|
| Zemin karosu | `A-bg-n/c/x.webp` | 1323 + 642 + 753 KB | 1024² | REPEAT (dikişsiz) |
| Worker kesesi | `A-worker-n/c/x.webp` | 96 + 55 + 37 KB | 512² | CLAMP_TO_EDGE |
| Echo medüzü | `A-echo-n/c/x.webp` | 87 + 59 + 40 KB | 512² | CLAMP_TO_EDGE |
| | `meta.json` | 1.1 KB | | |
| **Toplam** | | **3 092 192 B ≈ 3.09 MB** | | |

**Bağ damarı için harita yok, olmamalı da.** Bağ rotaları yerleşimle değiştiği için hazır bir bitmap gerilir. Damar her yerleşim değişiminde 2D canvas'a yükseklik profili olarak çizilir ve shader normalini bundan türetir (§6). Core çevresindeki 9 kök damar da aynı katmanda çizilir.

Boyut seçeneği: zemin 512²'ye inerse toplam ≈ %70 küçülür. Karo 170 dünya birimi olduğu için büyük panelde ayrıntı azalır. İstenirse `tools/` ile dakikalar içinde yeniden üretilir.

## 2. Kanal düzeni (üç harita setinde de aynı)

| Dosya | R | G | B | A |
|---|---|---|---|---|
| `-n` | normal X (+sağ) | normal Y (+yukarı) | normal Z (+izleyici) | kapsama (gövde silueti; zeminde 1) |
| `-c` | albedo (sRGB) | | | AO (doğrusal, 1 = açık) |
| `-x` | emit: durum/ışık bölgesi | thick: yarı saydamlık | roughness | `1 + 254·yükseklik01` |

- `N = 2·RGB/255 − 1`. Canvas'ta y aşağı baktığı için `N.y` **bir kez** ters çevrilir (`n.y = -n.y`).
- Yükseklik: `h01 = (A·255 − 1) / 254`. Gerçek aralık `meta.json` → `height: [min, max]` (model birimi). Shader'da yalnız göreli kullanılıyor (§5).
- **`-c` sRGB doku olarak yüklenmeli** (`SRGB8_ALPHA8`). Bu dokuda AO alfa kanalındadır ve alfa her zaman doğrusaldır. `-n` ve `-x` veridir: `RGBA8`, gamma yok.
- **Ön-çarpım yasak.** `createImageBitmap(blob, { premultiplyAlpha: "none", colorSpaceConversion: "none" })` ve `UNPACK_PREMULTIPLY_ALPHA_WEBGL = false`, `UNPACK_COLORSPACE_CONVERSION_WEBGL = NONE` kullanılmalı. Aksi hâlde normal ve veri kanalları alfayla çarpılır, kenarlarda ışık bozulur. `<img>` ya da 2D canvas üzerinden yüklemeyin; tarayıcı ön-çarpar.
- `A-bg-n.webp` 3 kanallı kaydedildi (alfa her yerde 255 olduğu için WebP alfayı attı). Decode edildiğinde A = 1 gelir, ek işlem gerekmez.
- Gövdelerde kenar RGB'si 6 px dilate edildi; mipmap/bilinear filtre siyah sızdırmaz. Ayarlar: mipmap + `LINEAR_MIPMAP_LINEAR`, varsa anizotropi 8.
- Gövdelerde emit = çekirdek (A-worker) ya da kenar, halka ve dokunaç ucu (A-echo). Zeminde emit = damarlar; nabız dalgası yalnız bunları yakar.

### `meta.json`

| Alan | Anlam |
|---|---|
| `extent` | Haritanın kapladığı model birimi (gövde 2.3, zemin 1.0 = bir karo) |
| `res` | Piksel |
| `tile` | `true` = periyodik zemin |
| `height` | Yükseklik aralığı (model birimi) |
| `bounds` | `[x0, y0, x1, y1]`: kapsama > 0.5 kutusu (piksel, üstten). Sprite boyutu ve merkezi buradan türetilir (§7). |
| `stats` | Kalite ölçüleri (kapsama, AO ortalama/p05, nz, emit, albedo parlaklığı). Yeniden render sonrası karşılaştırma için. |

## 3. Katman düzeni

1. **WebGL2 ışık katmanı** (alt canvas, `aria-hidden`). Çözünürlük `min(DPR, 1.5)`.
   - **Zemin geçişi:** Tam ekran üçgen. Zemin karosu, bağ yükseklik katmanı ve gövde gölgeleri bu geçişte.
   - **Gövde geçişi:** Her worker ve Echo için bir döndürülebilir quad, kendi haritalarıyla, alfa karışımlı. Karışım: `blendFuncSeparate(SRC_ALPHA, ONE_MINUS_SRC_ALPHA, ONE, ONE_MINUS_SRC_ALPHA)`.
   - İki geçiş **aynı ışık modelini** kullanır (§4), böylece gövdeler zeminle aynı ışıkta durur.
2. **2D canvas** (üst, DPR). Core (`coreEngine`/`coreXL`/`symbiote` değişmeden), temas dokunacı, etiketler, `✓ ! ?` işaretleri, odak halkası ve takım imzası burada.

PCB'ye ait her şey kalkar: `pcb.webp`, ızgara, montaj delikleri, "DEX / AGENT FABRIC", "REV.39", `worker.webp`, pinler, via'lar, düz iz çizgisi, `pcbTheme.ts`.

## 4. Işık ve shader parametreleri (A)

Birim: **dünya birimi** (`fitUnit`'in verdiği `unit` = CSS px / birim). Renkler sRGB hex olarak verildi, shader'a doğrusala çevrilerek girer (`linear()`).

| Parametre | Değer | Not |
|---|---|---|
| Zemin karosu | 170 birim / karo | `uv = (css / unit) / 170` |
| Normal kazancı | 1.0 | |
| Anahtar ışık | `#ffd8d2` × **0.32** | yükseklik 0.95 rad, azimut −2.3 rad |
| Işık süpürmesi | +0.05 rad/sn | yalnız "ışık hareketi" açıkken; azaltılmış harekette dur |
| Dolgu ışığı | `#4a6a8a` × 0.06 | azimut = anahtar + π, yükseklik 0.7 rad |
| Ortam | `#3a2a33` × 0.16 | AO ile çarpılır |
| Specular | 0.55 | Blinn-Phong, `shininess = clamp(2/r² − 2, 4, 1200)`, normalize `(sh+8)/8π` |
| Ortam yansıması (env) | 0 | A'da kapalı |
| Pozlama | 1.25 | ardından ACES (Narkowicz) + sRGB kodlama |
| Vinyet | 0.55 | `col *= clamp(1 − 0.55·|q|²·1.6)`, `q = css/view − 0.5` |
| Zemin emit rengi | `#ff4a3a` | damar nabzı |
| Yarı saydamlık (zemin) | wrap = 0.45·thick | sarmalayan diffuse + `alb·trans·thick·0.6` ışık sızması |
| Nokta ışığı sayısı | ≤ 16 | Core gözü, worker durum ışıkları, Echo, geri dönen damlalar |
| Nokta ışığı zayıflaması | `smoothstep(7r, 2r, d) / (1 + d²/r²)` | `r` = ışık yarıçapı |

**Nokta ışıkları** (konum CSS px; z = yükseklik, `u = unit`):

| Kaynak | x, y | z | r | Renk × şiddet | spec |
|---|---|---|---|---|---|
| Core gözü | Core merkezi | `0.55·s` | `120·(s/120)` | `--red-hi` × 2.6 × stateK × (1 − 0.85·lid) | 0.6 |
| Worker durumu (k > 0.3, kapalı değil) | gövde merkezi | `0.45·D` | `0.9·D` | durum rengi × 0.9·k·(0.35 + 0.65·pulse) | 0.3 |
| Echo | yuva merkezi | `0.7·D` | `70·(D/40)` | `#8fe6ea` × 1.3·(intensity/0.8)·(0.6 + 0.4·pulse) | 0.4 |
| Geri dönen damla | rota üstünde | 8 | 26 | damla rengi × 1.1 | 0.5 |

`s` = Core gövde boyu (`coreBody`). stateK: sleep 0.15, idle 0.6, diğerleri `0.8 + coreState.LOOKS[state].glow / 24`. `lid` = `core.lidNow`.

### Zemin fragment (A yolu, mock'tan sadeleştirildi)

```glsl
vec2 css = vec2(gl_FragCoord.x, uFB.y - gl_FragCoord.y) / uScale;   // CSS px
vec2 uv  = css / uUnit / 170.0;
vec3 n = texture(uGN, uv).xyz * 2.0 - 1.0;  n.y = -n.y;
vec4 c = texture(uGC, uv);  vec4 x = texture(uGX, uv);
vec3 alb = c.rgb; float ao = c.a;
float emit = x.r, thick = x.g, rough = x.b, hgt = (x.a * 255.0 - 1.0) / 254.0;

// bağ yükseklik katmanı (§6): R = yükseklik, B = sakin bölge
vec4 s = texture(uSH, css / uView);  float s0 = s.r;
vec2 sg = vec2(dR/dx, dR/dy) * uSHk;               // merkezi fark, 1 texel
float on = smoothstep(0.02, 0.4, s0);
n = normalize(vec3(n.xy * (1.0 - 0.8 * on) - sg, n.z));
alb = mix(alb, lin(#5a1626), on * 0.85);  rough = mix(rough, 0.2, on);  thick = mix(thick, 0.5, on);
ao *= 1.0 - 0.35 * smoothstep(0.0, 0.1, s0) * (1.0 - smoothstep(0.1, 0.45, s0));  // damar dibinde temas gölgesi

shadow = Π gövde gölgeleri (§7)

// Core'dan nabız (§8)
float dc = length(css - uCore.xy) / uUnit, wave = 0.0;
for (k = 0..1) { float ph = fract(uBeat + k * 0.5); wave += exp(-pow((dc - ph * 560.0) / 34.0, 2.0)) * (1.0 - ph) * (1.0 - ph); }
vec3 E = lin(#ff4a3a) * emit * (0.035 + 0.7 * wave) * (1.0 - 0.6 * on);
E += akış bandı + damla (§6) ;  E += lin(#ff3b30) * 0.16 * on * smoothstep(0.15, 0.8, s0);

vec3 P = vec3(css, (hgt * 4.0 + s0 * 3.0) * uUnit);
lightAll(P, n, rough, wrap = 0.45 * thick);          // gDiff, gSpec, gTrans
float calm = s.b, F = 0.04 + 0.96 * pow(1.0 - n.z, 5.0);
col  = alb * (amb * ao + gDiff * mix(1.0, ao, 0.6)) * shadow;
col += gSpec * 0.55 * (0.25 + F) * shadow * mix(1.0, 0.15, calm);
col += alb * gTrans * thick * 0.6 * shadow;
col += E * mix(1.0, 0.2, calm);
col *= mix(1.0, 0.5, calm);
col *= vinyet;  out = finish(col);                   // pozlama 1.25 → ACES → sRGB
```

### Gövde fragment (worker ve Echo, A)

```glsl
vec4 nt = texture(uBN, uv);  if (nt.a < 0.004) discard;
vec3 n = nt.xyz * 2.0 - 1.0;  n.y = -n.y;  n.xy = rotate(n.xy, rot);
vec4 c = texture(uBC, uv), x = texture(uBX, uv);
vec3 P = vec3(css, h01 * height);                    // height: worker 0.5·D, Echo 0.6·D (CSS px)
lightAll(P, n, x.b, wrap = translucency * x.g * 0.8); // translucency: worker 1, Echo 1
vec3 alb = mix(c.rgb, luma(c.rgb), desat);            // desat: kapalı 0.75
col  = alb * (amb * c.a + gDiff) + gSpec * 0.55 * (0.35 + F);
col += alb * gTrans * translucency * x.g * 0.5;
float pulse = 0.55 + 0.45 * statePulse;
col += state * intensity * x.r * pulse * 1.2;                   // çekirdek / kenar parlar
col += state * intensity * translucency * x.g * 0.22 * pulse;   // ışık zardan sızar
float a = nt.a * alpha * (1.0 - 0.42 * translucency * x.g * (1.0 - x.r));
out = vec4(finish(col), a);
```

## 5. Durum → ışık

Öncelik ve renkler bugünkü `chipLook` ile aynı. `soft(hz) = 0.5 + 0.5·sin(2π·hz·t)`. Azaltılmış harekette tüm `pulse` değerleri 1 (kapalıda 0).

| Durum (öncelik sırasıyla) | Renk token | k | intensity = k × 1.6 | pulse | İşaret |
|---|---|---|---|---|---|
| kapalı | `--muted` `#6c717a` | 0 | 0; alfa 0.72, desat 0.75, boy × 0.9 | 0 | — (etiket `--muted`, %62 opak) |
| teslim etti (sonuç done) | `--green` `#4f9e76` | 1 | 1.6 | `0.6 + 0.4·soft(0.35)` | `✓` |
| hata (sonuç failed) | `--red-hi` `#ff5c5f` | 1.1 | 1.76 | `0.55 + 0.45·|sin(0.7π·t)|` | `!` |
| soru / bekliyor | `--amber` `#f5c35b` | 1 | 1.6 | `soft(1.1)` | `?` (yanıp söner) |
| raporsuz bitti (diğer sonuç) | `--amber` | 0.9 | 1.44 | 1 | `!` |
| çalışıyor | `--red` `#e5383b` | 1 | 1.6 | `soft(1.15)^2.5` | — |
| boşta | `--muted` | 0.22 | 0.35 | 1 | — |

Not: spec "teslim etti: sabit" diyordu. Mock'ta ise 0.35 Hz'lik çok yavaş bir nefes var (pulse 0.6–1.0). Hangisini istediğinize ürün tarafında karar verin.

Renkler token'dan okunur; takım rengi `--team` (varsayılan `--red`, `[data-accent]` ile değişir). Hex'ler koyu tema içindir.

**İşaret halkası** (2D canvas, Core rozetiyle aynı biçim). Merkez: x = `bx ± 0.38·D` (dış tarafa), y = `by − 0.46·fh`. Yarıçap 6.5, dolgu `--surface-1`, çizgi 1.4 px işaret rengi, yazı `700 9px` mono. `?` yanıp söner: alfa `0.55 + 0.45·sin(6.9t)` ≈ 1.1 Hz.

**Odak / hover:** gövde × 1.05, kesikli (3/3) `--team-hi` elips, yarıçap `0.62·D × (0.62·fh + 2)`, çizgi 1.2.

## 6. Bağ damarı (delegasyon bağı)

Yerleşim (`fitUnit` / `layout` / `routes`) aynen kullanılır. Rota uç noktası gövdenin Core'a bakan kenarına çekilir: `x = bx − facing·0.42·D`. Stack modunda `y = by ± 0.42·fh`. Ardından:

- Chaikin × 3 → 2 birim adımla yeniden örnekleme → organik sapma: genlik 4 birim, `sin(s·f + φ)`, `f = 0.045..0.075`, tohum `index·97 + 13`, iki uçta sıfıra iner. Sonra rotanın başına Core merkezi eklenir.
- **Yükseklik katmanı** (R kanalı). Çözünürlük `k = clamp(1600/cssW, 0.4, 1)` × CSS. Tüp profili: yarı genişlik `5.5·√(1−h²)`, 14 basamak, `lighten` ile üst üste çizilir. Ardından `blur(max(0.5, 1·unit·k))`.
- **Core çapası:** 9 kök damar. Açı `i/9·2π + rnd·0.5`, uzunluk `s·(0.75 + rnd·0.55)`, başlangıç `0.3·s`, büküm `sin(d·0.05 + i)·0.25`. Uca doğru incelir, yarı genişlik × 0.75. Tohum 1937.
- **Sakin bölge** (B kanalı): her etiket kutusu (pad 7×6) ve Core ad bloğu dolu çizilir, `blur(max(2, 5·unit·k))` uygulanır. Shader bu bölgede spec'i %15'e, emisyonu %20'ye, toplam rengi %50'ye indirir. Etiket kontrastı AA'yı bununla geçer; kaldırmayın.
- **Akış katmanı** (ayrı doku, NEAREST). G = rota kimliği + 1. R + B/255 = rota boyunca konum (0..1). Çizgi kalınlığı `2.4 × 11` birim.
- `uSHk = 1.8·3.2·unit·k / 2`. Bağ yüksekliği P.z'ye `s0·3·unit` olarak eklenir.
- İki katman **yalnız yerleşim değişince** yeniden çizilir ve yüklenir.

**Akış (aktif iş, queued/running):** `band = pow(0.5 + 0.5·sin((along·L/46 − 1.5t)·2π), 5)`. Dalga boyu 46 CSS px, 1.5 dalga/sn, Core → worker yönünde. Renk `--team` × 1.1, katsayı 0.9.

**Geri dönen damla (sonuç/soru, worker → Core):** `packet = 1 − fract(t·v + id·c)`. Sonuçta v = 0.28, c = 0.07; soruda v = 0.24, c = 0.11. Gauss genişliği 10 px, şiddet 3. Damla aynı noktada bir nokta ışığı da taşır (§4). Renk: soru `--amber`, done `--green`, failed `--red-hi`, diğer `--amber`. Soru damlasının 8 px üstünde `?` yazılır (`700 11px` mono, amber). Azaltılmış harekette akış ve damla çizilmez; durum işareti ve gövde ışığı kalır.

## 7. Gövdeler

**Boyut:** `D = min(slot.w·0.42, slot.h·0.94 / aspect)`, aspect 0.82, `fh = 0.82·D`. Echo'da `0.40·slot.w` ve aspect 1. Gövde yuvanın Core'a bakan ucunda durur, etiket dış tarafta kalır. Sol sütunda etiket sağa hizalanır.

**Sprite:** `foot = bounds/res` → `size = D / foot.w`. Quad merkezi `(bx, by) − foot.c·size`; `foot.c` kapsama kutusu merkezinin harita merkezine göre konumudur. A'da worker dönmez (`rot = 0`).

**Gölge** (zemin geçişinde, gövde başına elips). Merkez `(bx, by)`, yarıçap `(0.5·D, 0.5·fh)`. Anahtar ışık yönünde `lift·(L.xy / max(L.z, 0.3))` kadar kayar.
- Worker: lift = `0.5·D·0.35`, k = 0.85.
- Echo: lift = `0.55·D`, k = 0.55. Gölge ayrık düşer, Echo havada okunur.
- Formül: `shadow *= 1 − k·0.62·smoothstep(1.3, 0.35, d)`. Temas halkası: `1 − k·0.3·smoothstep(1.22, 0.96, cd)`; yalnız lift < 0.4·rx olduğunda (Echo'da yok).

## 8. Core'dan damara nabız

- `beat = fract(t·0.42)`: 2.38 sn'lik döngü, aynı anda **iki halka** (yarım faz arayla).
- Halka yarıçapı `beat·560` dünya birimi, Gauss kalınlığı 34 birim, şiddet `(1 − beat)²` (dışa doğru söner).
- Yalnız zeminin emit kanalını (damarlar) yakar: `emit·(0.035 + 0.7·wave)`, renk `#ff4a3a`. Bağ damarının üstünde × 0.4 (`1 − 0.6·on`), çünkü bağ kendi `#ff3b30 × 0.16` parıltısını taşır.
- Azaltılmış harekette `beat = 0.3` sabit (halka durağan).
- Nabız Core durumundan bağımsız. Core'un durumu göz ışığının şiddetinde okunur (stateK, §4).

## 9. Echo medüzü

| Öğe | Değer |
|---|---|
| Kimlik rengi | `#8fe6ea` (gövde ışığı + glif). Durumdan bağımsız sabit. |
| Süzülme | `y += sin(1.3t)·2.2 − 3` birim (≈ 0.21 Hz salınım, 3 birim yukarıda) |
| Sallanma | `rot = sin(0.9t)·0.07` rad |
| Şiddet (× glow 0.8) | konuşuyor 1.25 · bekliyor 0.9 · boşta 0.55 · kapalı 0 |
| pulse | konuşuyor `0.5 + 0.5·sin(9.5t)·sin(2.1t)` (≈ 1.5 Hz ritim, küçük alan) · bekliyor `0.5 + 0.5·sin(6.9t)` · boşta `0.75 + 0.25·sin(1.4t)` |
| Alfa | 0.92; ayrıca zar `1 − 0.42·thick·(1 − emit)` ile incelir |
| Yarı saydamlık | 1 |
| Bağ | yok (`isAssignable` false); kendi yerleşim yuvasında |
| Bekliyor | amber `?` işareti (§5); gövde rengi değişmez |
| Azaltılmış hareket | süzülme, sallanma ve pulse durur (pulse 1) |

Hiçbir yanıp sönme 3/sn'yi geçmez (WCAG 2.3.1). En hızlısı Echo'nun konuşma ritmi.

## 10. Durumlar ve erişilebilirlik

- **Yükleniyor:** 9 harita decode edilene kadar bugünkü prosedürel 2D çizim kalır (`assets/index.ts` deseni). Haritalar yüklenince bir kerede geçilir; tek tek geçilmez.
- **WebGL2 yok / bağlam kaybı:** düz zemin + radyal gövde fallback'i. `webglcontextrestored` gelince dokular yeniden yüklenip ışık katmanına dönülür.
- **Boş takım (yalnız Core):** zemin, Core ve Core çapası çizilir.
- **Kontrast (mock'ta ölçüldü):** 5 panel boyutunda, t = 3/14/26 sn'de etiket altı zemin p95'e göre ad ≥ 8.3:1, rol (`--text-2`) ≥ 4.9:1. Bunu sağlayan sakin bölge (§6) ve etiket gölgesi (`rgba(0,0,0,.85)`, blur 3). Uygulayan aynı ölçümü tekrarlamalı.
- **Renk dışı ipuçları:** `✓ ! ?`, hareket, kapalıda gri + çökme, Echo'nun ayrı formu.
- **Azaltılmış hareket:** iki katman da durağan olmalı (iki kare farkı 0). Işık süpürmesi, nabız, akış, damla ve Echo süzülmesi durur.
- **Klavye:** odak sırası değişmez; odak halkası §5'te.
- **Dar alan:** 1280×720, 920×680, 620×380 (DPR 1.5) ve 520×760 dikeyde (stack modu) çakışma ve taşma yok. GL çözünürlüğü ≤ 1.5×.
- **Performans:** gizliyken çizme. Hareketsizken (azaltılmış hareket ya da ışık durdu, senaryo sabit) yalnız değişince çiz. Ölçüm için ayrı görünür canvas, warm-up ve gerçek GPU'da rAF A/B gerekir (hafıza: `symbiote-shadow-gpu-sync`).

## 11. Maket kaynakları (`.dex/design/symbiote-cards/`, repo dışı, gitignore'da)

| Dosya | İlgili yer |
|---|---|
| `A-doku.html` | Tek yön, tam ekran mock. Panel boyutu, ışık, senaryo ve azaltılmış hareket düğmeleri var. |
| `spec.md` | Tasarım kararları, ölçümler, maliyet tahmini (A: ortak ~5 + 1.5 gün, ek +0.5 gün) |
| `src/mock/gl.ts` | `HEADER` (ışık modeli, `lightAll`, `finish`) 31–101 · `GROUND_FS` 106–218 (A = `uMode == 0`) · `BODY_FS` 232–269 · doku yükleme 301–336 · `setStrands` 339 |
| `src/mock/looks.ts` | `LOOKS.A` 29–40: bu belgedeki bütün sayılar |
| `src/mock/main.ts` | harita yükleme 56–71 · `relayout` (gövde/etiket/rota) 170–211 · `buildStrands` (yükseklik, kök damar, sakin bölge, akış) 215–316 · etiket kutuları 322–343 · `workerState` 350–362 · `drawLit` (ışıklar, Echo, gölge, damla) 407–510 · `drawWorkerInk` (etiket, işaret, odak) 603–678 · `resize` (GL ≤ 1.5×) 692–700 |
| `src/mock/scenario.ts` | 36 sn'lik durum döngüsü (test senaryosu için) |
| `src/mock/check.mjs` | görsel bakmadan ölçüm: konsol hatası, ışık katmanı parlaklığı, etiket kontrastı, etiket/gövde çakışması (yön × panel boyutu) |
