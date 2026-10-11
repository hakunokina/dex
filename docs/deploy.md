# Kendi VM'inde dexd barındırma

VM isteğe bağlıdır; masaüstü derlemesi için gerekli değildir. Linux VM'de Rust/Cargo,
Git, curl, systemd, ss, flock ve sudo yetkili bir SSH hesabı gerekir. Örnek servis
hesabı `dex`, SSH hedefi `admin@vm.example.com`. Servis hesabını, bare repoyu, kaynak
clone'unu ve çalışan `dexd.service` birimini önceden oluştur. Clone'un origin'i bare
repo olmalı; servis config'teki `BIN`, `DATA`, `PORT` değerlerini kullanmalı.

## Config ve kurulum

```sh
cp scripts/deploy/deploy.conf.example scripts/deploy/deploy.conf
# SSH hedefi, kullanıcı/grup, yollar ve portu kendi VM'ine göre düzenle.
pnpm deploy:install --dry-run
pnpm deploy:status --dry-run
```

Config gitignored'dır; yalnız basit `KEY=value` kabul edilir. Anahtar/parola yazma;
SSH agent kullan. PC betikleri alternatif dosya için `DEX_DEPLOY_CONF`, hedefi ezmek
için `DEX_DEPLOY_HOST` kabul eder; config yine gereklidir. Kuru çalıştırma config'i
okur, tar/SSH/servis çağrısı yapmaz.

Önce hazırlık VM'inde uygula ve doğrula; canlı kurulum açık onay gerektirir:

```sh
pnpm deploy:install
git remote add server admin@vm.example.com:/home/dex/dex.git
git push server master
pnpm deploy:status --log
```

Kurulum tekrarlanabilir. Config VM'e `DEPLOY_HOME/deploy.conf` olarak 0640 izinle
kurulur. Ortam ezmeleri doğrulandıktan sonra çözümlenmiş config aktarılır ve kurulur;
systemd/sudoers şablonları ile runtime hook aynı değerleri kullanır.
Önce mevcut config/betik/birim/sudoers dosyalarını yedekle. Sudoers servis hesabına
yalnız seçilen servisin start/stop/restart iznini verir. dexd biriminde ajanların
sudo kullanmasını engellemek için `NoNewPrivileges=yes` kullan. dexd ve deploy
servislerinin `[Service]` bölümünde `UMask=0077` kullan:

```ini
[Service]
UMask=0077
```

Unix'te dexd veri/config köklerini 0700; DB, WAL/SHM ve log dosyalarını 0600
yapar, mevcut modları da düzeltir. Veri yedeğini yalnız servis hesabının okuyabildiği
0700 dizinde tut; yeni yedekler bu modları korur. Güncelleme öncesi alınmış yedeklerin
izinlerini ayrıca doğrula. Eski stderr logları komut içerebilir; izinlerini daraltmak
içeriklerini temizlemez, paylaşım öncesi ayrıca incele.

## Akış ve sağlık

master push → post-receive → trigger → dexd-deploy.path → dexd-deploy.service →
fetch/reset → `cargo build --release -p dexd` → commit damgası → ikili/veri yedeği →
stop/install/start → sağlık. Derleme başarısızsa çalışan servise dokunulmaz.
Oneshot + flock eşzamanlı deploy'u engeller; yeni push sonraki turda kurulur.

Sağlık: servis active, MainPID seçilen portu dinliyor, WS el sıkışması 101,
koşan `/proc/<pid>/exe` kurulan ikiliyle aynı ve commit hedef SHA ile eşleşiyor.
Varsayılan 60 saniye içinde, 5 saniye arayla aynı PID'de iki geçiş gerekir.

## İzleme ve geri alma

`pnpm deploy:status --log`: status.json, üç birimin durumu ve log kuyruğu.
Loglar `DEPLOY_HOME/logs/` altında, en yeni 30 tutulur; ayrıca
`journalctl -u dexd-deploy.service`. İzleme sistemi `build_failed`, `rolled_back`,
`rolled_back_data`, `rollback_failed`, `failed_no_prev` veya inactive serviste uyarı
üretmeli. Betikler uyarı servisi kurmaz. İkili/veri yedekleri otomatik budanmaz.

Sağlık başarısızsa önceki ikili geri kurulur; bu yetmezse mevcut veri `.failed-<ts>`
altına taşınır ve `.bak-<ts>` yedeği geri konur. `rollback_failed`: servisi durdur,
sağlam `dexd.<ts>-pre<sha>` ikilisini ve gerekirse `DATA.bak-<ts>` yedeğini geri kur,
servisi başlat ve sağlığı doğrula.

```sh
ssh admin@vm.example.com sudo systemctl disable --now dexd-deploy.path
ssh admin@vm.example.com sudo systemctl enable --now dexd-deploy.path
ssh admin@vm.example.com sudo systemctl start dexd-deploy.service
```

Kurulum değişikliğini geri almak için path birimini kapat, yedek config/betik/birim/
sudoers dosyalarını geri koy, `sudo visudo -c`, `sudo systemctl daemon-reload` koş,
path birimini aç. Kendi SSH hedefini kullan.

Yerel kontrol: `DEPLOY_CONF=<config> bash scripts/deploy/dexd-deploy.sh --dry-run`.
İzole VM'de `DEPLOY_HOME REPO SRC BIN DATA SERVICE PORT` ortamı config'i ezer;
`PREBUILT=<ikili>` derlemeyi atlar, `TARGET_SHA` tek tur, `EXPECT_SHA=<sahte>` geri alma
yolunu sınar. Bu gerçek deploy testidir; canlıda koşulmaz.

## Android

`pnpm release:all` masaüstü release ardından `pnpm release:apk` çalıştırır. APK
uzak derleme hattıdır: SSH host, SDK/NDK, build-apk, release keystore ve
sertifika eşleşmesi gerekir; bağlı adb cihazına kurulum yapabilir. Masaüstü için
`pnpm release` kullan. APK betiği aynı yerel `scripts/deploy/deploy.conf` dosyasını okur.
Beklenen imza sertifikasının SHA-256 parmak izini `DEX_APK_CERT=<64 hex karakter>`
satırıyla ekle veya `DEX_APK_CERT` ortam değişkenini ayarla (önceliklidir). Örnek
config'teki boş değer deploy için geçerlidir; APK yayını öncesinde doldurulmalıdır.
Parmak izi eksik/geçersizse betik derleme ya da SSH çağrısı yapmadan durur.
