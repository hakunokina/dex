#!/bin/bash
# dexd-deploy — VM: master'ı derle → yedekle → kur → sağlık kontrolü → başarısızsa geri al.
# dexd-deploy.service (User=dex) koşturur; dexd-deploy.path, post-receive hook'un yazdığı tetik
# dosyasıyla başlatır. Elle: `sudo systemctl start dexd-deploy.service` ya da dex olarak bu dosya.
# Kurulum: scripts/deploy/install.sh. Belgeler: docs/deploy.md.
#
# Sağlık = servis active + MainPID 7717'yi dinliyor + WS el sıkışması 101 + koşan imaj
# (`/proc/<pid>/exe`) = kurulan dosya + (yeni sürümde) imajın `--version` commit'i = hedef sha;
# iki kez, arayla (çökme döngüsü yakalansın). server_info jeton ister; `/proc/<pid>/exe` koşan
# sürecin kendi imajıdır, aynı damgayı verir. `--version`'u olmayan eski ikiliye geri alınca
# yalnız imaj karşılaştırılır.
#
# Ortam (test için ezilebilir; yerel deploy.conf zorunlu):
#   DEPLOY_HOME REPO SRC BIN DATA SERVICE PORT HEALTH_TIMEOUT KEEP_LOGS SUDO
#   PREBUILT=<ikili>   derlemeyi atla, bu ikiliyi kur (test)
#   TARGET_SHA=<sha>   master yerine bu sha (tek tur; test)
#   EXPECT_SHA=<sha>   sağlık kontrolü bu commit'i beklesin (sahte → geri alma yolu; test)
set -uo pipefail

SCRIPT_DIR=$(cd "$(dirname "$0")" && pwd)
if [ -f "$SCRIPT_DIR/deploy.conf" ]; then DEFAULT_CONF=$SCRIPT_DIR/deploy.conf; else DEFAULT_CONF=$SCRIPT_DIR/../deploy.conf; fi
DEPLOY_CONF=${DEPLOY_CONF:-${DEX_DEPLOY_CONF:-$DEFAULT_CONF}}
. "$SCRIPT_DIR/config.sh"
load_deploy_config || exit 2
if [ "${1:-}" = --dry-run ]; then
  echo 'dexd-deploy dry-run: local config validated; build/service/health skipped'
  exit 0
fi
SUDO=${SUDO:-sudo -n}
PREBUILT=${PREBUILT:-}
TARGET_SHA=${TARGET_SHA:-}
EXPECT_SHA=${EXPECT_SHA:-}

LOGS="$DEPLOY_HOME/logs"
STATUS="$DEPLOY_HOME/status.json"
mkdir -p "$LOGS"

say() { echo "[$(date '+%F %T')] $*"; }
svc() { $SUDO /usr/bin/systemctl "$1" "$SERVICE"; }

# Koşan dexd'nin commit'i (10 hane) — dosyadaki değil, sürecin imajı.
running_commit() {
  local pid
  pid=$(systemctl show -p MainPID --value "$SERVICE" 2>/dev/null)
  [ -n "$pid" ] && [ "$pid" != 0 ] || return 1
  "/proc/$pid/exe" --version 2>/dev/null | sed -n 's/.*"commit":"\([^"]*\)".*/\1/p'
}
bin_commit() { "$1" --version 2>/dev/null | sed -n 's/.*"commit":"\([^"]*\)".*/\1/p'; }
# $1 tam ya da kısa sha, $2 damga (10 hane): biri ötekinin öneki mi.
same_sha() { [ -n "$1" ] && [ -n "$2" ] && [ "$2" != unknown ] && { [ "${1#"$2"}" != "$1" ] || [ "${2#"$1"}" != "$2" ]; }; }

status() { # state result sha prev log
  printf '{"state":"%s","result":"%s","sha":"%s","prev":"%s","time":"%s","log":"%s"}\n' \
    "$1" "$2" "$3" "$4" "$(date -Is)" "$5" >"$STATUS.tmp" && mv -f "$STATUS.tmp" "$STATUS"
}

health_once() { # expect(boş = commit bakma) → stdout pid
  local expect=$1 pid c addr code
  systemctl is-active -q "$SERVICE" || { say "sağlık: $SERVICE active değil" >&2; return 1; }
  pid=$(systemctl show -p MainPID --value "$SERVICE")
  [ -n "$pid" ] && [ "$pid" != 0 ] || { say "sağlık: MainPID yok" >&2; return 1; }
  addr=$(ss -ltnpH "sport = :$PORT" 2>/dev/null | grep "pid=$pid," | awk '{print $4}' | head -n1)
  [ -n "$addr" ] || { say "sağlık: pid $pid :$PORT dinlemiyor" >&2; return 1; }
  code=$(curl -s -o /dev/null -w '%{http_code}' -m 2 -H 'Connection: Upgrade' -H 'Upgrade: websocket' \
    -H 'Sec-WebSocket-Version: 13' -H 'Sec-WebSocket-Key: ZGV4ZGRlcGxveWhlYWx0aA==' "http://$addr/" 2>/dev/null)
  [ "$code" = 101 ] || { say "sağlık: $addr WS el sıkışması $code" >&2; return 1; }
  cmp -s "/proc/$pid/exe" "$BIN" || { say "sağlık: koşan imaj ≠ $BIN" >&2; return 1; }
  if [ -n "$expect" ]; then
    c=$(bin_commit "/proc/$pid/exe")
    same_sha "$expect" "$c" || { say "sağlık: koşan commit '$c' ≠ beklenen '$expect'" >&2; return 1; }
  fi
  echo "$pid"
}

health() { # expect → 0: HEALTH_TIMEOUT içinde art arda iki geçiş (5 sn ara, aynı pid)
  local expect=$1 deadline=$((SECONDS + HEALTH_TIMEOUT)) p1 p2
  while [ "$SECONDS" -lt "$deadline" ]; do
    if p1=$(health_once "$expect"); then
      sleep 5
      p2=$(health_once "$expect")
      [ "$p1" = "$p2" ] && { say "sağlık: tamam (pid $p1${expect:+, commit ~$expect})"; return 0; }
    fi
    sleep 2
  done
  health_once "$expect" >/dev/null # son neden log'a
  return 1
}

# Yalnız deploy log'ları döner (adlar tarihle başlar, en yeni $KEEP_LOGS kalır). İkili/veri
# yedekleri geri alma için korunur — gerektiğinde elle temizlenir.
rotate_logs() { ls -1 "$LOGS"/*.log 2>/dev/null | sort -r | tail -n +$((KEEP_LOGS + 1)) | xargs -r rm -f --; }

deploy_one() { # sha log
  local sha=$1 log=$2 short=${1:0:10} new prev ts bak_bin bak_data
  status running building "$short" "" "$log"
  if [ -n "$PREBUILT" ]; then
    new=$PREBUILT
  else
    say "kaynak: $SRC → $short"
    git -C "$SRC" fetch -q origin && git -C "$SRC" reset -q --hard "$sha" || { status done build_failed "$short" "" "$log"; return 1; }
    say "derleme: cargo build --release -p dexd"
    (cd "$SRC" && cargo build --release -p dexd) || { say "derleme başarısız; canlı dexd'ye dokunulmadı"; status done build_failed "$short" "" "$log"; return 1; }
    new="${CARGO_TARGET_DIR:-$SRC/target}/release/dexd"
  fi
  same_sha "$sha" "$(bin_commit "$new")" || { say "derlenen ikili damgası $(bin_commit "$new") ≠ $short"; status done build_failed "$short" "" "$log"; return 1; }

  prev=$(running_commit || bin_commit "$BIN" || true)
  prev=${prev:-unknown} # --version'u olmayan eski ikili
  ts=$(date +%Y%m%d-%H%M)
  [ -e "$DATA.bak-$ts" ] && ts=$(date +%Y%m%d-%H%M%S)
  bak_bin="$(dirname "$BIN")/dexd.$ts-pre${prev:0:7}"
  bak_data="$DATA.bak-$ts"
  status running installing "$short" "$prev" "$log"
  [ -e "$BIN" ] && cp -p "$BIN" "$bak_bin" && say "ikili yedeği: $bak_bin"
  say "durdur: $SERVICE"
  svc stop || { say "stop başarısız"; status done failed "$short" "$prev" "$log"; return 1; }
  [ -d "$DATA" ] && cp -a "$DATA" "$bak_data" && say "veri yedeği: $bak_data"
  install -m 0755 "$new" "$BIN.new" && mv -f "$BIN.new" "$BIN"
  say "başlat: $SERVICE ($short)"
  svc start
  if health "${EXPECT_SHA:-$sha}"; then
    status done ok "$short" "$prev" "$log"; say "deploy tamam: $short"; return 0
  fi

  say "GERİ ALMA: önceki ikili ($prev)"
  if [ ! -e "$bak_bin" ]; then status done failed_no_prev "$short" "" "$log"; return 1; fi
  svc stop
  install -m 0755 "$bak_bin" "$BIN.new" && mv -f "$BIN.new" "$BIN"
  svc start
  if health ""; then status done rolled_back "$short" "$prev" "$log"; say "geri alındı: $prev"; return 1; fi

  say "GERİ ALMA: önceki ikili + veri yedeği (mevcut veri → $DATA.failed-$ts)"
  svc stop
  [ -d "$bak_data" ] && mv "$DATA" "$DATA.failed-$ts" && cp -a "$bak_data" "$DATA"
  svc start
  if health ""; then status done rolled_back_data "$short" "$prev" "$log"; say "geri alındı (veriyle): $prev"; return 1; fi
  status done rollback_failed "$short" "$prev" "$log"; say "GERİ ALMA BAŞARISIZ — elle müdahale: docs/deploy.md"
  return 2
}

rotate_logs
exec 9>"$DEPLOY_HOME/lock"
flock -n 9 || { say "başka bir deploy sürüyor; o tur bitince master'ı yeniden okur"; exit 0; }

# Derleme sürerken gelen push'lar: tur bitince master yeniden okunur, yalnız sonuncusu kurulur.
tried=""
rc=0
while :; do
  target=${TARGET_SHA:-$(git -C "$REPO" rev-parse master)}
  cur=$(running_commit || true)
  if [ -z "$TARGET_SHA" ] && same_sha "$target" "$cur"; then say "güncel: $cur"; break; fi
  [ "$target" = "$tried" ] && break
  tried=$target
  log="$LOGS/$(date +%Y%m%d-%H%M%S)-${target:0:10}.log"
  say "deploy: ${target:0:10} (koşan: ${cur:-yok}) → $log"
  deploy_one "$target" "$log" >"$log" 2>&1
  rc=$?
  tail -n 3 "$log"
  [ -n "$TARGET_SHA" ] && break
done
exit $rc
