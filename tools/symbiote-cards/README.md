# Symbiote kart haritaları — A · Doku

`src/office/assets/symbiote-cards/` altındaki 9 WebP ve `meta.json` buradan üretilir. Tasarım #1937'de, ürüne geçiş #1957'de.

| Dosya | İş |
|---|---|
| `shapes.py` | Prosedürel heightfield'lar (yalnız numpy): `A-bg` zemin karosu, `A-worker` kese, `A-echo` medüz. Kayıt `ASSETS` sözlüğünde: çözünürlük, kapsam, AO mesafesi, örnek sayısı. |
| `render_maps.py` | Blender 5.2, Cycles CPU. Üstten ortografik kamera, varlık başına bir multilayer EXR (Normal, AO, AOV albedo/emit/thick/rough/fade/height + alfa). |
| `pack_maps.py` | EXR'yi kayıpsız WebP'ye çevirir (`-n` / `-c` / `-x`) ve `meta.json` yazar. Kanal düzeni: `src/office/assets/symbiote-cards/HANDOFF.md`. |

## Yeniden render

Gerekenler: Blender **5.2** (5.2.2 LTS ile doğrulandı) ve Python 3 + `numpy` + `Pillow`. Komutlar repo kökünden çalışır. Ara EXR'ler ~115 MB tutar, bu yüzden repo dışında bir klasöre yazılır.

```bash
export PYTHONDONTWRITEBYTECODE=1        # tools/ altında __pycache__ kalmasın
EXR=/tmp/symbiote-exr                    # geçici, repo dışı
blender --background --factory-startup --python tools/symbiote-cards/render_maps.py -- "$EXR"            # üçü; ~25 sn
blender --background --factory-startup --python tools/symbiote-cards/render_maps.py -- "$EXR" A-worker   # tek varlık
python -I tools/symbiote-cards/pack_maps.py "$EXR" src/office/assets/symbiote-cards [A-bg A-worker A-echo]
```

`pack_maps.py` mevcut `meta.json`'u okur ve yalnız işlenen varlıkların girdisini günceller.

**Belirlenimci:** Gürültü tohumları sabit, Cycles örneklemesi de sabit tohumla çalışıyor. 2026-10-15'te bu üç betikle yapılan yeniden render, repodaki 9 WebP ile piksel piksel aynı çıktı (max fark 0). `meta.json` da aynı çıktı. Bir değişikliğin etkisini görmek için önce render alıp sonucu repodakiyle karşılaştırın.

## Değişiklik yaparken

- Biçim değiştirmek için `shapes.py`'deki ilgili fonksiyonu düzenleyin (`a_bg`, `a_worker`, `a_echo`). Kanal anlamlarını değiştirmeyin; shader'lar bu anlamlara göre okuyor (HANDOFF.md).
- Zemin karosu periyodiktir: gürültü `periodic=True`, periyot 1 model birimi. Periyodik olmayan bir terim eklenirse karo dikişi görünür.
- Gövdeler yıldız biçimli olmalı (merkezden her açıda tek kenar). `body_geometry` dış konturu kutupsal ızgarayla bulur.
- `res` değişirse boyut değişir. Zemin 1024²'den 512²'ye inerse paket ≈ %70 küçülür, ancak karo 170 birim olduğu için yakın ölçekte ayrıntı azalır.
- Blender 5.x tuzağı: `image_settings.media_type = "MULTI_LAYER_IMAGE"`, `file_format`'tan **önce** atanmalı. EXR multipart çıkar ve kanal adları iki kez öneklidir. `pack_maps.py` yalnız sıkıştırmasız EXR okur (`exr_codec = "NONE"`).

Lisans: Geometri özgün ve prosedürel. Dış asset, HDRI ya da model kullanılmadı. Blender'ın GPL'i çıktıya geçmez.
