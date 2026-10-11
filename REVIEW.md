# Kod incelemesi ölçütleri

Dex, commit'li her worker işini birleştirmeden önce takımın code-review üyesine inceletir.
İnceleyici kodu yazmaz, yalnız okur; karar dal ucunun commit'ine bağlıdır.
Dex bunu teknik olarak da uygular: inceleyici dosya yazamaz, repo değiştiremez, test ya da derleme koşmaz;
test sonuçları için worker'ın raporuna bakar.

## Rapor biçimi

```
karar: onay | düzelt

## önemli
- dosya:satır — kısa gerekçe (hangi girdi hangi yanlış sonucu verir)

## küçük
- dosya:satır — kısa gerekçe
```

Önemli bulgu varsa karar `düzelt`. Bulgu yoksa başlığın altına "yok" yaz.

## Ölçütler

- **Doğruluk / hata** — yanlış sonuç, kenar durum, yarış, veri kaybı, regresyon. Önemli.
- **Güvenlik** — sır ya da kişisel veri koda/log'a yazılmış; dış girdi doğrulanmadan kullanılmış; yetki kontrolü eksik. Önemli.
- **Görev kapsamı** — istenmeyen refactor, kapsam dışı değişiklik, ilgisiz dosyaya dokunma. Davranışı değiştiriyorsa önemli, değilse küçük.
- **Kırmızı testi gevşetme** — testi silmek, beklentiyi zayıflatmak, atlamak ya da kırmızıyı yeşile çevirmek için doğrulamayı kaldırmak. Önemli.
- **Çevredeki koda uymama** — adlandırma, deyim, yorum yoğunluğu, mevcut yardımcıyı yeniden kullanmama. Genelde küçük.
- **Patronun sözüyle uyuşmazlık** — iş metnindeki "Patronun sözü" ile yapılan iş çelişiyor ya da onun istediği eksik. Her zaman önemli.

Zevki kusur gibi sunma; emin olmadığın bulguda varsayımını yaz.
