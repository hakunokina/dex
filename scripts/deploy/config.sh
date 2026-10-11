# Shared strict KEY=value reader. Environment overrides keep the isolated test path working.
load_deploy_config() {
  local line key value
  DEPLOY_CONF=${DEPLOY_CONF:-$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/deploy.conf}
  [ -r "$DEPLOY_CONF" ] || { echo 'deploy.conf gerekli (deploy.conf.example dosyasını kopyala)' >&2; return 2; }
  while IFS= read -r line || [ -n "$line" ]; do
    line=${line%$'\r'}
    [[ -z "$line" || "$line" == \#* ]] && continue
    [[ "$line" =~ ^([A-Z_]+)=([a-zA-Z0-9_./:@+-]+)$ ]] || { echo 'deploy.conf: KEY=value bekleniyor' >&2; return 2; }
    key=${BASH_REMATCH[1]} value=${BASH_REMATCH[2]}
    [[ "$key" =~ ^(DEX_DEPLOY_HOST|DEPLOY_USER|DEPLOY_GROUP|USER_HOME|DEPLOY_HOME|REPO|SRC|BIN|DATA|SERVICE|PORT|HEALTH_TIMEOUT|KEEP_LOGS)$ ]] || { echo 'deploy.conf: bilinmeyen alan' >&2; return 2; }
    printf -v "$key" '%s' "${!key:-$value}"
  done <"$DEPLOY_CONF"
  if [ -n "${DEX_DEPLOY_HOST:-}" ]; then
    [[ "$DEX_DEPLOY_HOST" != -* && "$DEX_DEPLOY_HOST" =~ ^[a-zA-Z0-9_.@:-]+$ ]] || return 2
  fi
  for key in DEPLOY_USER DEPLOY_GROUP; do
    [[ "${!key:-}" =~ ^[a-z_][a-z0-9_-]*$ ]] || { echo 'geçersiz kullanıcı/grup' >&2; return 2; }
  done
  for key in USER_HOME DEPLOY_HOME REPO SRC BIN DATA; do
    [[ "${!key:-}" =~ ^/[a-zA-Z0-9_./-]+$ && "${!key}" != *..* ]] || { echo 'geçersiz deploy yolu' >&2; return 2; }
  done
  [[ "${SERVICE:-}" =~ ^[a-zA-Z0-9_-]+\.service$ ]] || return 2
  for key in PORT HEALTH_TIMEOUT KEEP_LOGS; do [[ "${!key:-}" =~ ^[0-9]+$ ]] || return 2; done
  [ "$PORT" -ge 1 ] && [ "$PORT" -le 65535 ] && [ "$HEALTH_TIMEOUT" -gt 0 ] && [ "$KEEP_LOGS" -gt 0 ]
}

# Snapshot the values already resolved and validated by load_deploy_config.
write_deploy_config() {
  local key
  for key in DEX_DEPLOY_HOST DEPLOY_USER DEPLOY_GROUP USER_HOME DEPLOY_HOME REPO SRC BIN DATA SERVICE PORT HEALTH_TIMEOUT KEEP_LOGS; do
    [ -z "${!key:-}" ] || printf '%s=%s\n' "$key" "${!key}"
  done
}
