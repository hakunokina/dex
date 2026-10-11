#!/bin/bash
# Yayın VM kurulumu — kendi VM'inde sudo yetkili SSH kullanıcısı olarak:
#   tar -C scripts/yayin -cf - . | ssh admin@vm.example.com \
#     'd=$(mktemp -d) && tar -x -C "$d" && sed -i "s/\r$//" "$d"/* && bash "$d/install.sh" "$d" [adım...]'
# (sed: Windows checkout'unda autocrlf dosyaları CRLF yapabilir)
# Adımlar: ssh fail2ban ufw docker caddy tools (varsayılan: hepsi) | confirm
# Tekrar koşmak güvenli (idempotent). ssh/ufw değişince 10 dk'lık geri alma zamanlayıcısı kurulur;
# YENİ bir bağlantıyla giriş doğrulanınca `confirm` ile iptal edilir, edilmezse kendiliğinden geri alır.
# Geri dönüş: docs/yayin.md.
set -euo pipefail
D=${1:?kaynak dizin}
shift
. "${YAYIN_CONF:-$D/yayin.conf}"
: "${YAYIN_USER:?yayin.conf: YAYIN_USER gerekli}" "${DOMAIN:?yayin.conf: DOMAIN gerekli}"
[[ "$YAYIN_USER" =~ ^[a-z_][a-z0-9_-]*$ ]] || exit 2
[[ "$DOMAIN" =~ ^[a-zA-Z0-9.-]+$ && "${YAYIN_HOST:-}" =~ ^[a-zA-Z0-9_.:-]+$ ]] || exit 2
[[ "${CPUS:-}" =~ ^[0-9]+(\.[0-9]+)?$ && "$CPUS" =~ [1-9] ]] || exit 2
[[ "${MEM:-}" =~ ^[0-9]+(\.[0-9]+)?[bBkKmMgG]?$ && "$MEM" =~ [1-9] ]] || exit 2
for key in KEEP_RELEASES HEALTH_TIMEOUT; do
  [[ "${!key:-}" =~ ^[0-9]+$ ]] && [ "${!key}" -gt 0 ] || exit 2
done
if [ "${1:-}" = --dry-run ]; then
  echo 'yayin install dry-run: local config loaded; sudo/apt/systemctl skipped'
  exit 0
fi
STEPS=${*:-ssh fail2ban ufw docker caddy tools}
GUARD_SEC=${GUARD_SEC:-600}
R=$(mktemp -d "$D/.render-XXXXXX")
trap 'rm -rf "$R"' EXIT
for key in YAYIN_USER YAYIN_HOST DOMAIN CPUS MEM KEEP_RELEASES HEALTH_TIMEOUT; do
  printf '%s=%q\n' "$key" "${!key}"
done >"$R/yayin.conf"

say() { echo "[yayin-install] $*"; }
apt_q() { sudo DEBIAN_FRONTEND=noninteractive apt-get -qq -o Dpkg::Options::=--force-confold "$@"; }

# $1 ad, $2 geri alma komutu: GUARD_SEC sonra root olarak koşar (confirm iptal eder).
guard() {
  sudo systemctl stop "yayin-guard-$1.timer" 2>/dev/null || true
  sudo systemctl reset-failed "yayin-guard-$1.service" "yayin-guard-$1.timer" 2>/dev/null || true
  sudo systemd-run -q --unit="yayin-guard-$1" --on-active="$GUARD_SEC" /bin/sh -c "$2"
  say "geri alma zamanlayıcısı: yayin-guard-$1 (${GUARD_SEC}s) — yeni bağlantıyla doğrula, sonra: install.sh <d> confirm"
}

step_ssh() {
  local f=/etc/ssh/sshd_config.d/00-hardening.conf
  if sudo cmp -s "$D/00-hardening.conf" "$f"; then say "ssh: değişiklik yok"; else
    sudo install -m 0644 "$D/00-hardening.conf" "$f"
    sudo sshd -t || { sudo rm -f "$f"; say "ssh: sshd -t başarısız, geri alındı"; return 1; }
    guard ssh "rm -f $f; systemctl reload ssh"
    sudo systemctl reload ssh
  fi
  sudo sshd -T | grep -E '^(passwordauthentication|kbdinteractiveauthentication|permitrootlogin|pubkeyauthentication) '
}

step_fail2ban() {
  dpkg -s fail2ban >/dev/null 2>&1 || apt_q install -y fail2ban
  sudo install -m 0644 "$D/fail2ban-sshd.local" /etc/fail2ban/jail.d/yayin-sshd.local
  sudo systemctl enable -q fail2ban
  sudo systemctl restart fail2ban
  sleep 2
  sudo fail2ban-client status sshd
}

step_ufw() {
  dpkg -s ufw >/dev/null 2>&1 || apt_q install -y ufw
  sudo ufw default deny incoming >/dev/null
  sudo ufw default allow outgoing >/dev/null
  sudo ufw allow 22/tcp comment ssh >/dev/null
  sudo ufw allow 80/tcp comment http >/dev/null
  sudo ufw allow 443/tcp comment https >/dev/null
  sudo ufw allow 443/udp comment http3 >/dev/null
  sudo ufw show added | grep -qx "ufw allow 22/tcp comment 'ssh'" || { say "ufw: 22 açık değil, enable edilmedi"; return 1; }
  if ! sudo ufw status | grep -q 'Status: active'; then
    guard ufw "ufw --force disable"
    sudo ufw --force enable
  fi
  sudo ufw status verbose
}

step_docker() {
  local codename
  codename=$(. /etc/os-release && echo "$VERSION_CODENAME")
  if [ ! -e /etc/apt/sources.list.d/docker.sources ]; then
    sudo install -m 0755 -d /etc/apt/keyrings
    sudo curl -fsSL https://download.docker.com/linux/ubuntu/gpg -o /etc/apt/keyrings/docker.asc
    sudo chmod a+r /etc/apt/keyrings/docker.asc
    printf 'Types: deb\nURIs: https://download.docker.com/linux/ubuntu\nSuites: %s\nComponents: stable\nSigned-By: /etc/apt/keyrings/docker.asc\n' \
      "$codename" | sudo tee /etc/apt/sources.list.d/docker.sources >/dev/null
    apt_q update
  fi
  dpkg -s docker-compose-plugin >/dev/null 2>&1 ||
    apt_q install -y docker-ce docker-ce-cli containerd.io docker-buildx-plugin docker-compose-plugin
  # ip=127.0.0.1: host IP'siz yayınlanan port da dışarı açılmaz (Docker UFW'yi atlar).
  if ! sudo cmp -s "$D/daemon.json" /etc/docker/daemon.json; then
    sudo install -m 0644 "$D/daemon.json" /etc/docker/daemon.json
    sudo systemctl restart docker
  fi
  sudo systemctl enable -q docker containerd
  id -nG "$YAYIN_USER" | grep -qw docker || sudo usermod -aG docker "$YAYIN_USER"
  docker --version; docker compose version
}

step_caddy() {
  if [ ! -e /etc/apt/sources.list.d/caddy-stable.list ]; then
    curl -1sLf https://dl.cloudsmith.io/public/caddy/stable/gpg.key |
      sudo gpg --batch --yes --dearmor -o /usr/share/keyrings/caddy-stable-archive-keyring.gpg
    sudo curl -1sLf https://dl.cloudsmith.io/public/caddy/stable/debian.deb.txt -o /etc/apt/sources.list.d/caddy-stable.list
    sudo chmod a+r /usr/share/keyrings/caddy-stable-archive-keyring.gpg /etc/apt/sources.list.d/caddy-stable.list
    apt_q update
  fi
  dpkg -s caddy >/dev/null 2>&1 || apt_q install -y caddy
  sudo install -d -m 0755 /etc/caddy/sites
  if ! sudo cmp -s "$D/Caddyfile" /etc/caddy/Caddyfile; then
    [ -e /etc/caddy/Caddyfile.orig ] || sudo cp -p /etc/caddy/Caddyfile /etc/caddy/Caddyfile.orig
    sudo install -m 0644 "$D/Caddyfile" /etc/caddy/Caddyfile
    sudo caddy validate --config /etc/caddy/Caddyfile --adapter caddyfile >/dev/null
    sudo systemctl reload-or-restart caddy
  fi
  sudo systemctl enable -q caddy
  caddy version
}

step_tools() {
  dpkg -s jq >/dev/null 2>&1 || apt_q install -y jq
  dpkg -s python3 >/dev/null 2>&1 || apt_q install -y python3
  sudo install -d -m 0755 /usr/local/lib/yayin
  sudo install -m 0755 "$D/post-receive" /usr/local/lib/yayin/post-receive
  sudo install -m 0644 "$D/compose-validate.py" /usr/local/lib/yayin/compose-validate.py
  local t
  for t in yayin-new yayin-deploy yayin-rm yayin-status; do sudo install -m 0755 "$D/$t" "/usr/local/bin/$t"; done
  sudo install -m 0644 "$R/yayin.conf" /etc/yayin.conf
  sudo install -d -o "$YAYIN_USER" -g "$YAYIN_USER" -m 0755 /srv/git /srv/apps /srv/archive
  sudo install -d -o "$YAYIN_USER" -g "$YAYIN_USER" -m 0700 /srv/env
  say "araçlar: /usr/local/bin/yayin-{new,deploy,rm,status}"
}

step_confirm() {
  local g
  for g in ssh ufw; do
    if sudo systemctl stop "yayin-guard-$g.timer" 2>/dev/null; then say "zamanlayıcı iptal: yayin-guard-$g"; fi
  done
}

for s in $STEPS; do say "== $s"; "step_$s"; done
