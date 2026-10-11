# Yayın VM'i (#221)

Kendi Linux VM'inde Compose projelerini Caddy ile yayınlamak için isteğe bağlı rehber.
Örnek hedef `admin@vm.example.com`, alan adı `example.com`; IP dokümantasyon örneğidir.

## Kurulum özeti

Betikler `scripts/yayin/`. Önce yerel config oluştur:

```sh
cp scripts/yayin/yayin.conf.example scripts/yayin/yayin.conf
# YAYIN_USER, YAYIN_HOST ve DOMAIN değerlerini kendi VM'ine göre düzenle.
bash scripts/yayin/install.sh scripts/yayin --dry-run
YAYIN_CONF=scripts/yayin/yayin.conf bash scripts/yayin/yayin-deploy --dry-run
```

`yayin.conf` gitignored'dır; gerçek host/domain/kullanıcı burada tutulur.
SSH anahtarı/parola yazma; SSH agent kullan. `YAYIN_CONF` alternatif config yoludur.
VM araçları varsayılan `/etc/yayin.conf` okur. `tools` adımı kurulumda kullanılan,
doğrulanmış değerleri bu dosyaya her seferinde yazar; önce mevcut config'i yedekle.
Sunucuya özgü `.env` yalnız `/srv/env/` altında
kalır; `/srv/` yolları genel servis düzenidir.

Önce hazırlık VM'inde uygula; canlı kurulum açık onay gerektirir.
Kurulum/güncelleme (idempotent, kendi SSH hedefini kullan; PC'den):

```
tar -C scripts/yayin -cf - . | ssh admin@vm.example.com \
  'd=$(mktemp -d) && tar -x -C "$d" && sed -i "s/\r$//" "$d"/* && bash "$d/install.sh" "$d" [adım...]'
```

Adımlar: `ssh fail2ban ufw docker caddy tools` (varsayılan hepsi), `confirm`.
Varsayılan kurulumun kullandığı kişisel değer içermeyen `scripts/yayin/fail2ban-sshd.local` şablonu depoda bulunur.

| Adım | Ne yapar |
|---|---|
| ssh | `/etc/ssh/sshd_config.d/00-hardening.conf`: şifre/kbd-interactive kapalı, root girişi yok. `50-cloud-init.conf`'tan önce okunur (`PasswordAuthentication yes`'i ezer; `sshd -T` ile doğrulanır). |
| fail2ban | `/etc/fail2ban/jail.d/yayin-sshd.local`: 10 dk'da 5 hata → 1 sa ban. journalmatch yalnız `ssh.service` (OpenSSH 10 `sshd-session` olarak loglar, paket filtresi `_COMM=sshd` kaçırır). |
| ufw | gelen: yalnız 22/tcp, 80/tcp, 443/tcp+udp. 22 kuralı yoksa enable edilmez. |
| docker | resmi apt reposu; Engine + compose plugin; `admin` docker grubunda. `daemon.json`: `"ip": "127.0.0.1"` (host IP'siz port da dışarı açılmaz — Docker UFW'yi atlar), log 3×10 MB. |
| caddy | resmi apt reposu; `/etc/caddy/Caddyfile` yalnız `import /etc/caddy/sites/*.caddy`. Sertifika otomatik (Let's Encrypt). |
| tools | `/usr/local/bin/yayin-{new,deploy,rm,status}`, hook `/usr/local/lib/yayin/post-receive`, `/etc/yayin.conf`, `/srv/{git,apps,env,archive}`. |

ssh ya da ufw değişince sunucuda 10 dk'lık `yayin-guard-<adım>` zamanlayıcısı kurulur; süre dolarsa
değişikliği kendiliğinden geri alır. **Yeni** bir bağlantıyla giriş doğrulanınca `install.sh <d> confirm`.

## Ajanlar için kullanım

### Yeni proje

```
ssh admin@vm.example.com yayin-new <proje> <port> [sağlık_yolu]   # örn. yayin-new blog 8101 /healthz
```

Oluşturur: `/srv/git/<proje>.git` (bare, hook bağlı), `/srv/apps/<proje>/`, `/srv/env/<proje>.env`
(0600), `/etc/caddy/sites/<proje>.caddy` (`<proje>.example.com` → `127.0.0.1:<port>`). Port
benzersiz olmalı (`yayin-status` listeler). Tekrar koşmak güvenli.

Repoda kökte `compose.yaml` (ya da `compose.yml` / `docker-compose.y[a]ml`) olmalı. Kurallar —
ihlal canlıya dokunmadan reddedilir (`invalid`):

```yaml
services:
  web:
    build: .
    ports: ["127.0.0.1:${PORT}:3000"]   # zorunlu: 127.0.0.1 + PORT (yayin-new'deki port)
```

- yayınlanan her port `127.0.0.1:` ile başlar; bind kaynakları symlink çözülmüş haliyle `/srv/apps/<proje>/` altında olmalı; daemon socket, host namespace, privileged, ek capability/device ve gevşek security_opt reddedilir; standart named volume serbest, `driver_opts` reddedilir. Araç sandbox değildir: [güven modeli](../SECURITY.md#trust-model).
- her servise otomatik eklenir: CPU/bellek limiti (`/etc/yayin.conf` `CPUS=2 MEM=1g`; proje
  başına `/srv/apps/<proje>/conf`'ta ezilir), `restart: unless-stopped`, `env_file: /srv/env/<proje>.env`;
- gizli ortam yalnız sunucuda: `/srv/env/<proje>.env` (repoya `.env` koyma). Compose
  değişken çözümü de bu dosyayı ve `PORT`'u kullanır.

### Push → deploy

```
git remote add yayin admin@vm.example.com:/srv/git/<proje>.git
git push yayin main          # main ya da master
```

post-receive → `yayin-deploy` (oturumdan bağımsız; bağlantı koparsa sürer, log push çıktısına akar):
checkout (`/srv/apps/<proje>/releases/<sha12>`) → `docker compose build` (başarısızsa canlıya
dokunulmaz) → `up -d` → sağlık: `http://127.0.0.1:<port><sağlık_yolu>` 2xx/3xx, 3 sn arayla iki
kez, 60 sn içinde, yeniden başlayan konteyner yok → `current` symlink'i. Sağlık düşerse önceki
sürüm (`current`) yeniden kaldırılır. Proje başına tek deploy (`flock`); arada yeni push gelirse
eskisi atlanır. Son 5 sürüm dizini tutulur.

Sonuç `/srv/apps/<proje>/status.json` → `result`:
`ok | invalid | build_failed | rolled_back | failed_no_prev | rollback_failed`. Atlanan push status yazmaz, yalnız log'a.

### Durum, log

```
yayin-status [proje]                       # tüm projeler: port, canlı HTTP kodu, current, son sonuç
docker compose -p <proje> logs -f --tail 100
ls -t /srv/apps/<proje>/logs/              # deploy log'ları (<ts>-<sha>.log)
sudo journalctl -u caddy -f                # TLS / proxy hataları
```

### Geri alma

```
yayin-deploy <proje> <eski-sha>            # o commit'i yeniden kurar (sağlık + otomatik dönüş aynı)
yayin-deploy <proje>                       # main/master ucunu yeniden kurar
```

`rollback_failed`: `ls /srv/apps/<proje>/releases`, sağlam olanı `yayin-deploy <proje> <sha>`.

### Kaldırma

`yayin-rm <proje>` — konteynerler ve Caddy sitesi kaldırılır; repo, sürümler ve `.env`
`/srv/archive/<proje>-<ts>/`'e taşınır (silinmez). İmajlar kalır (`docker image prune` elle).

## Altyapıyı geri alma

| | |
|---|---|
| SSH | `sudo rm /etc/ssh/sshd_config.d/00-hardening.conf && sudo systemctl reload ssh` |
| UFW | `sudo ufw disable` |
| fail2ban | `sudo rm /etc/fail2ban/jail.d/yayin-sshd.local && sudo systemctl restart fail2ban` (ya da `disable --now`) |
| Caddy | `sudo cp /etc/caddy/Caddyfile.orig /etc/caddy/Caddyfile && sudo systemctl reload caddy` |
| Docker | `docker compose -p <proje> down` her proje; paket `apt remove docker-ce ...` |

## DNS (Cloudflare, example.com)

İzleme sistemi `status.json` sonucunda `invalid|build_failed|rolled_back|failed_no_prev|rollback_failed`,
başarısız HTTP sağlık kontrolü ve Caddy/TLS hatasında uyarı üretmeli. Betikler uyarı servisi kurmaz.

| Kayıt | Değer | Not |
|---|---|---|
| `A @` | 203.0.113.10 | |
| `A *` | 203.0.113.10 | her `<proje>.example.com` |
| `A www` (ya da `CNAME www → @`) | 203.0.113.10 | `www`'e site gerekiyorsa ayrıca `yayin-new` / Caddy sitesi |

Cloudflare proxy (turuncu bulut) açıksa SSL modu **Full (strict)** olmalı; Caddy sertifikasını
HTTP-01 ile proxy arkasından alır. Proxy açıkken Caddy log'larında istemci IP'si Cloudflare'inkidir
(gerçek IP `Cf-Connecting-Ip`).
