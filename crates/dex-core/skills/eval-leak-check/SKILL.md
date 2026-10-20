---
name: eval-leak-check
description: "Değerlendirme/benchmark/ölçüm sonucunu yorumlamadan önce 8 kalıplı sızıntı taksonomisiyle tara: dış gerçek bağımsız giriyor mu, yoksa tasarlayan-model-puanlayıcı birbirini mi onaylıyor? YALNIZ eval/benchmark/deney/holdout işlerinde ve bir sonuç fazla temiz/kendi-kendini-onaylar göründüğünde kullan. Salt okuma: yalnız ateşlenen kalıpları, adıyla ve bağımsızlık düzeltmesiyle ver; deneyi yeniden yazma."
---

# Eval sızıntı denetimi

## Goal

Bir ölçümün (eval, benchmark, deney, holdout, "işe yaradığını nasıl bileceğiz") sonucuna güvenmeden önce tek soruyu cevapla: **dış gerçek bu ölçüme bağımsız giriyor mu, yoksa tasarlayan, ölçülen ve puanlayan birbirini mi onaylıyor?** Sızıntı varsa skor, ölçülen şeye değil sızıntının kendisine aittir.

Dex emsalleri (ayrıntı için `memory_search`):

- **Sentetik araç testi Laguna'yı yanılttı:** talimatsız kod-dökümü istemiyle koşan sentetik test modeli 2/5 gösterdi; aynı model gerçek Claude Code isteminde araçları sorunsuz kullandı. Ölçüm koşulu gerçek koşul değilse skor modele değil koşula aittir → model seçimi gerçek oturumla yapılır.
- **Embedding eval'i:** sorgunun kendi gelecek raporu endekste olursa ve sorgu sonucunda yazılmış kayıt gold sayılırsa skorlar şişer, yöntemler arası fark silinir. Düzeltme: N. sorgu yalnız N'den eski belgeleri görür; sonuçtan doğan gold en fazla `uncertain` sayılır ve duyarlılık analiziyle raporlanır.

## Workflow

1. Ölçümü tanımla ve **bileşenlerini** adlandır: ne ölçülen (model/sistem), ne puanlayıcı, kim tasarladı, veri/gold nereden geldi.
2. Çekirdek soruyu sor: dış gerçek bağımsız giriyor mu?
3. Aşağıdaki **8 kalıpla** tara (kimisi yalnız belli bileşene uygulanır — insan denek, puanlayıcı). Yalnız **ateşlenenleri**, adıyla raporla.
4. Her ateşlenen kalıp için bağımsız-gerçek düzeltmesini tek satır ver.

## 8 sızıntı kalıbı

1. **Recall, not reason (ezber, akıl yürütme değil)** — hesaplaması beklenen cevap zaten ezberde; sistem türetmesi gerekeni tekrarlıyor.
2. **Wrong null hypothesis (yanlış boş hipotez)** — ablasyon yüzeydeki etiketi kaldırıyor ama sistemin gerçekten sömürdüğü sinyali kaldırmıyor; "kontrol" hâlâ sızdırıyor.
3. **Shared hallucination (paylaşılan halüsinasyon)** — iki bileşen birbirini doğruluyor; döngüsellik sayı diye raporlanıyor.
4. **Tautology (totoloji)** — puanlayıcı, kendisinin çizdiği kovaları puanlıyor.
5. **Verifier = designer (doğrulayan = tasarlayan)** — holdout kılığında özel, yeniden üretilemez tarif; ölçümü tasarlayan ile sonucu doğrulayan aynı kişi/bağlam.
6. **Shared-pool bias (ortak havuz yanlılığı)** — eğitim ve holdout aynı etiketleyici havuzundan; tek yanlılık iki tarafa da giriyor.
7. **Frame injection (çerçeve enjeksiyonu)** — soru, hipotezi deneğe elden veriyor.
8. **Demand characteristics (talep özellikleri)** — ölçülen denekler ölçüldüğünü biliyor ve ona göre davranıyor.

## Rules

- **Bağımsızlık kuralı:** ölçümü tasarlayan ile sonucu doğrulayan aynı olmasın. Dex'te: eval'i yazan worker'ın sonucunu başka bağlam yorumlasın; yüksek riskli ölçümde lider, yalnız ölçüm tasarımını (bu oturumun akıl yürütmesini değil) alan ayrı bir işe kör denetim verebilir.
- İki kez ısıran incelik: **çıktı değerini** körleyip **toplama tarifini** sızdırabilirsin; ikisini de kontrol et.
- Salt okuma: sızıntıyı ve bağımsızlık düzeltmesini adlandır; deneyi yeniden yazma, sonucu "düzeltip" yeni skor üretme.
- Ateşlenmeyen kalıbı listeleme; rapor kökü söyler, çamaşır listesi değil.
- Emsal ararken `memory_search` kullan (örn. geçmiş eval kararları, iptal edilmiş ölçüm yaklaşımları); hafızadaki iptal kararını yeniden önerme.

## Verification

Denetimi kendine çevir:

1. 3–5. kalıpları kendinde koş: kendi çizdiğin kovaları mı puanlıyorsun (#4)? Doğrulayan, tasarlayanla aynı mı (#5 — bu denetimi ölçümü tasarlayan bağlam mı yapıyor)? Kararın, tasarımın kendi iddialarıyla paylaşılan bir halüsinasyon mu (#3)?
2. Denetimi koşmamış bir okur, yalnız gösterdiğin kanıttan aynı karara varabilir mi — bağımsız gerçek var mı?
3. Rapor ateşlenen her kalıbı adıyla ve düzeltmesiyle veriyor, ateşlenmeyeni şişirmiyor mu?
