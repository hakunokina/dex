# Dex bildirim sesleri

Bu klasöre `<tür>.<uzantı>` adıyla bir dosya koyarsan Dex o olayda onu çalar; dosya yoksa yerleşik sentez sesi çalar.
Uzantı sırası: `.opus`, `.mp3`, `.wav`, `.ogg` (ilk bulunan kullanılır). Ses düzeyi Ayarlar → Sesler'deki ayarla çarpılır.

| Dosya adı | Olay |
|---|---|
| `complete` | Tur tamamlandı |
| `attention` | Patrona soru / üye önerisi |
| `warning` | Takıldı / izin bekliyor / kota tükendi |
| `delegation` | İş devredildi |
| `worker` | Worker işi bitti |

Dosyalar uygulama açılışında (ses ilk açıldığında) yüklenir; değiştirdiysen Dex'i yeniden aç.
