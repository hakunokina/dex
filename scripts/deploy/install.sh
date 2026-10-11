#!/bin/bash
# dexd-deploy kurulumu — kendi VM'inde sudo yetkili SSH kullanıcısı olarak.
#   pnpm deploy:install (yerel deploy.conf üzerinden)
# Tekrar koşmak güvenli (idempotent). Geri dönüş: docs/deploy.md.
set -euo pipefail
D=${1:?kaynak dizin}
DEPLOY_CONF=${DEPLOY_CONF:-$D/deploy.conf}
. "$D/config.sh"
load_deploy_config
if [ "${2:-}" = --dry-run ]; then
  echo 'deploy install dry-run: local config validated; sudo/install/systemctl skipped'
  exit 0
fi
H=$DEPLOY_HOME
R=$(mktemp -d "$D/.render-XXXXXX")
trap 'rm -rf "$R"' EXIT
write_deploy_config >"$R/deploy.conf"
for f in dexd-deploy.service dexd-deploy.path post-receive sudoers-dexd-deploy; do
  sed -e "s|@DEPLOY_USER@|$DEPLOY_USER|g" -e "s|@DEPLOY_GROUP@|$DEPLOY_GROUP|g" \
    -e "s|@USER_HOME@|$USER_HOME|g" -e "s|@DEPLOY_HOME@|$DEPLOY_HOME|g" -e "s|@SERVICE@|$SERVICE|g" "$D/$f" >"$R/$f"
done
sudo visudo -cf "$R/sudoers-dexd-deploy"
sudo install -d -o "$DEPLOY_USER" -g "$DEPLOY_GROUP" -m 2775 "$H" "$H/bin" "$H/logs"
sudo install -o "$DEPLOY_USER" -g "$DEPLOY_GROUP" -m 0755 "$D/dexd-deploy.sh" "$H/bin/dexd-deploy"
sudo install -o "$DEPLOY_USER" -g "$DEPLOY_GROUP" -m 0644 "$D/config.sh" "$H/bin/config.sh"
sudo install -o "$DEPLOY_USER" -g "$DEPLOY_GROUP" -m 0640 "$R/deploy.conf" "$H/deploy.conf"
[ -e "$H/trigger" ] || sudo install -o "$DEPLOY_USER" -g "$DEPLOY_GROUP" -m 0664 /dev/null "$H/trigger"
sudo install -o "$DEPLOY_USER" -g "$DEPLOY_GROUP" -m 0775 "$R/post-receive" "$REPO/hooks/post-receive"
sudo install -m 0644 "$R/dexd-deploy.service" "$R/dexd-deploy.path" /etc/systemd/system/
sudo install -m 0440 "$R/sudoers-dexd-deploy" /etc/sudoers.d/dexd-deploy
sudo visudo -c >/dev/null
sudo systemctl daemon-reload
sudo systemctl enable --now dexd-deploy.path
systemctl is-active dexd-deploy.path
echo "dexd-deploy kuruldu"
