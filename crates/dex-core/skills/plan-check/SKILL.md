---
name: plan-check
description: "Plan ya da delegate öncesi tek kök itiraz + en ucuz sınama (ilk çivi) ve iş başına model/efor kademe tavsiyesi. YALNIZ büyük işte (yeni mekanizma, veri modeli, izin politikası, çok adımlı plan) ya da patron açıkça isteyince kullan; küçük/orta işte çağırma — sürekli yıkım refleksi liderde yıkım önyargısı yaratır. Çıktı liste değil: {root, first_nail} + iş başına kademe satırı. Tavsiyedir; plan/delegate kararını lider verir."
---

# Plan denetimi: tek kök itiraz + kademe rubriği

## Goal

Büyük bir planı `plan` ile patrona sunmadan ya da `delegate` ile işe başlamadan önce iki şeyi netleştir:

1. **Tek kök itiraz (`root`)** — planı tek başına öldürebilecek taşıyıcı itiraz ve onu sınayacak **en ucuz deney (`first_nail`)**. Liste değil; çok eksen taranır ama sentez tek köke iner.
2. **İş başına kademe** — her iş kalemi için en ucuz yeterli model kademesi ve `effort` niyeti. Riski boyuttan önce değerlendir; efor yetenek satın almaz; doğrulama kademeden bağımsızdır; çıktı yalnız tavsiyedir.

## Workflow

### A. Kök itiraz ve ilk çivi

1. Planın **taşıyıcı varsayımını** yaz: bu yanlışsa planın geri kalanı anlamsızlaşır. Başlamadan `memory_search` ile aynı konuda geçmiş kök neden, tuzak ve kararları ara; hafızadaki sınıf kuralı varsayımın kendisi olabilir.
2. Varsayıma uygunsa şu eksenlerden saldır: yanlış olabilecek taşıyıcı olgu; sonradan uydurulmuş hikâyenin olgu sayılması; analojinin izomorfizm sanılması (başka alandaki yapı buraya taşınmaz); henüz var olmayan bir sonuca dayanma; anılan ilkenin tersinin uygulanması. Plan bir ölçüm/eval içeriyorsa ayrıca: dış gerçek ölçümü bağımsız giriyor mu (bkz. sızıntı denetimi) ve ölçümün yanlış çıkma gücü var mı.
3. Bulguları **tek köke** indir: hangisi çökerse diğerleri önemsizleşir? O köktür.
4. **İlk çiviyi** bul: taşıyıcı varsayımı yanlışlayabilecek en ucuz kontrol (zaman/maliyet/örneklem). Çivi, planın kendisinden belirgin ucuz olmalı; gerekiyorsa asıl işten önce küçük, salt-okuma bir `delegate` ile sınat.
5. `{root, first_nail}` döndür. Çivi varsayımı öldürürse planı sunma; patrona kökü ve çivi sonucunu tek mesajla söyle.

### B. İş başına kademe rubriği

Her iş kalemi için risk ve karmaşıklığı **bir kez** oku; aynı okuma iki kadranı besler. Puanlarken bak: dosya/modül/sahiplik sınırı aşımı, geri-alınabilirlik ve patlama yarıçapı, güvenlik/gizlilik/veri kaybı riski, yenilik ve belirsizlik, yanlış cevabın maliyeti.

**Model kademesi** — en ucuz yeterli sınıf:

- `hızlı`: yerel, mekanik, geri alınabilir; doğrulaması ucuz ve tam.
- `standart`: sıradan repo-içi akıl yürütme, çok adımlı taslak, normal kodlama.
- `sınır`: mimari, yüksek belirsizlik, güvenlik/gizlilik/veri-kaybı riski, sürüm-kritik inceleme; tek yanlış varsayımın koca koşuyu çöpe attığı iş.

**Efor niyeti** — o zihnin ne kadar düşüneceği; `delegate`'teki `effort` alanına lider çevirir:

- `bakış`: en az düşünme, doğrudan yol (modelin tabanı).
- `ölçülü`: gündelik olağan düşünme (modelin varsayılanı/ortası).
- `derin`: bilinçli fazladan — alternatifleri işle, varsayımları yokla (varsayılanın üstü, tavanın altı).
- `tüketici`: en yüksek düşünme, aramayı tüket ve işi yeniden kontrol et (modelin tavanı).

Varsayılan eşleşme: `hızlı`→`bakış`, `standart`→`ölçülü`, `sınır`→`derin`; `tüketici` yalnız en zor/en riskli işe. Sapma bilinçli olsun: ucuz ama incelikli iş `hızlı`+`derin`, güçlü modelde hızlı uzman kararı `sınır`+`bakış` olabilir.

Her kalem için tek satır ver: kademe, efor, tek cümle gerekçe, `yükselt: …` / `düşür: …` tetikleri ve **kanıt yüzeyi** (kademe/efor ne olursa olsun gereken doğrulama: test, inceleme, komut çıktısı).

## Rules

- **Saldır, iyileştirme.** Planı güzelleştirmek başka refleks; burada amaç öldürücü itirazı ucuzca bulmak.
- **Liste döndürme.** Çok eksen ateşlenebilir; sentezi sen yap, tek kök ver.
- **Risk boyutu yener.** Tek dosyalık yüksek riskli değişiklik `sınır` isteyebilir; geniş ama mekanik yeniden adlandırma, doğrulaması tamsa `hızlı`+`derin` kalır.
- **Efor yetenek satın almaz.** Efor düşünme bütçesidir, cevap uzunluğu da değildir; yetersiz kademeyi eforla kurtarma — kademe işidir.
- **Doğrulama kademeden bağımsız.** Güçlü model ya da yüksek efor testin, incelemenin, komut çıktısının yerini tutmaz; kanıt yüzeyini her kalemde ayrıca yaz.
- **Yalnız tavsiye.** Bu skill model/efor seçmez, ayar değiştirmez, işi çalıştırmaz; `plan` ve `delegate` kararını lider (ve gerekiyorsa patron) verir. Patronun açık kademe/efor seçimini ezme; uyumsuzluk görürsen söyle.
- Somut model ürünü/satıcı/sürüm adı yazma; kademe ve efor niyetiyle konuş.
- Küçük/orta işte bu skill'i çağırma; ölçülü kullan ki liderde yıkım önyargısı oluşmasın.

## Verification

Bitirmeden önce doğrula:

1. `root` gerçekten taşıyıcı: çökerse plan da çöküyor mu?
2. `first_nail`, önleyeceği plandan gerçekten ucuz mu ve varsayımı gerçekten yanlışlayabilir mi?
3. Her iş kaleminde tam bir kademe (`hızlı|standart|sınır`) ve bir efor (`bakış|ölçülü|derin|tüketici`) var mı; ikisi de en güçlüsü değil, **en ucuz yeterlisi** mi?
4. Yükselt/düşür tetikleri iki kadranı da kapsıyor mu; kanıt yüzeyi her kalemde yazılı mı?
5. Çıktı yönlendirme iddiası taşımıyor mu (model adı, ayar değişikliği, çalıştırma yok)?
