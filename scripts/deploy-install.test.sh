#!/bin/bash
# Real installers with privileged/system operations confined to a mock filesystem.
set -euo pipefail
ROOT=$(cd "$(dirname "$0")/.." && pwd)
mkdir -p "$ROOT/.dex/tmp"
T=$(mktemp -d "$ROOT/.dex/tmp/install-test-XXXXXX")
trap 'rm -rf "$T"' EXIT
mkdir -p "$T/mock-bin" "$T/system" "$T/deploy" "$T/yayin"
export MOCK_ROOT="$T/system"
export MOCK_WORK="$T"
cp "$ROOT"/scripts/deploy/* "$T/deploy/"
cp "$ROOT"/scripts/yayin/* "$T/yayin/"
# Normalise copies only (Windows autocrlf).
sed -i 's/\r$//' "$T/deploy/"* "$T/yayin/"*
# Redirect the installer's direct existence check as well as its sudo install target.
sed -i "s|/etc/yayin.conf|$MOCK_ROOT/etc/yayin.conf|g" "$T/yayin/install.sh"
cat >"$T/mock-bin/sudo" <<'EOF'
#!/bin/bash
case "$1" in
  install|visudo|systemctl) exec "$@" ;;
  *) echo "unexpected sudo command: $1" >&2; exit 1 ;;
esac
EOF
cat >"$T/mock-bin/install" <<'EOF'
#!/bin/bash
set -euo pipefail
args=()
directory=false
while [ "$#" -gt 0 ]; do
  case "$1" in
    -o|-g|-m) shift 2 ;; # Unix ownership/modes are outside this config regression.
    -d) directory=true; shift ;;
    /etc/*|/usr/local/*|/srv/*) args+=("$MOCK_ROOT$1"); shift ;;
    *) args+=("$1"); shift ;;
  esac
done
if "$directory"; then targets=("${args[@]}"); else targets=("${args[-1]}"); fi
for target in "${targets[@]}"; do
  [[ "$target" == "$MOCK_WORK/"* ]] || { echo 'install target outside sandbox' >&2; exit 1; }
done
if "$directory"; then mkdir -p "${args[@]}"; else cp "${args[@]}"; fi
EOF
for c in visudo systemctl dpkg; do
  printf '#!/bin/bash\nexit 0\n' >"$T/mock-bin/$c"
done
chmod +x "$T/mock-bin/"*
export PATH="$T/mock-bin:$PATH"
mkdir -p "$MOCK_ROOT/etc/systemd/system" "$MOCK_ROOT/etc/sudoers.d" "$MOCK_ROOT/usr/local/bin"
keys=(DEX_DEPLOY_HOST DEPLOY_USER DEPLOY_GROUP USER_HOME DEPLOY_HOME REPO SRC BIN DATA SERVICE PORT HEALTH_TIMEOUT KEEP_LOGS)
unset "${keys[@]}"
unset DEPLOY_CONF DEX_DEPLOY_CONF YAYIN_CONF YAYIN_USER YAYIN_HOST DOMAIN CPUS MEM KEEP_RELEASES
if [ "${1:-}" != yayin ]; then
for scenario in plain override; do
  home="$T/$scenario"
  mkdir -p "$home/repo/hooks"
  sed -e "s|/home/dex|$home|g" -e "s|^REPO=.*|REPO=$home/repo|" \
    "$T/deploy/deploy.conf.example" >"$T/deploy/deploy.conf"
  expected="$home/deploy"
  if [ "$scenario" = override ]; then
    export DEPLOY_HOME="$home/resolved" BIN="$home/resolved-bin/dexd" DATA="$home/resolved-data" SERVICE=resolved.service
    expected=$DEPLOY_HOME
  fi
  # Repeat to prove the installed state is stable.
  for run in 1 2; do
    bash "$T/deploy/install.sh" "$T/deploy"
    installed=$(env -u DEPLOY_HOME -u BIN -u DATA -u SERVICE DEPLOY_CONF="$expected/deploy.conf" \
      bash -c '. "$1"; load_deploy_config; printf "%s" "$DEPLOY_HOME"' _ "$expected/bin/config.sh")
    [ "$installed" = "$expected" ] || { echo "FAIL $scenario: installed DEPLOY_HOME=$installed, unit/hook=$expected" >&2; exit 1; }
    grep -Fx "ExecStart=$installed/bin/dexd-deploy" "$MOCK_ROOT/etc/systemd/system/dexd-deploy.service"
    grep -Fx "PathChanged=$installed/trigger" "$MOCK_ROOT/etc/systemd/system/dexd-deploy.path"
    grep -F "$installed/deploy.conf" "$home/repo/hooks/post-receive"
    if [ "$scenario" = override ]; then
      for k in BIN DATA SERVICE; do grep -Fx "$k=${!k}" "$expected/deploy.conf"; done
      grep -F "$SERVICE" "$MOCK_ROOT/etc/sudoers.d/dexd-deploy"
    fi
    printf '%s\n' 'old new refs/heads/master' | env -u DEPLOY_HOME -u BIN -u DATA -u SERVICE \
      bash "$home/repo/hooks/post-receive"
    [ "$(cat "$expected/trigger")" = new ]
  done
  unset DEPLOY_HOME BIN DATA SERVICE
  echo "PASS deploy $scenario (2 installs + runtime hook)"
done
if DEX_DEPLOY_HOST='invalid host' bash "$T/deploy/install.sh" "$T/deploy"; then
  echo 'FAIL deploy: invalid host override accepted' >&2
  exit 1
fi
echo 'PASS deploy invalid host rejected'
fi

# An existing VM config must not diverge from the config used by the installer.
cp "$T/yayin/yayin.conf.example" "$T/yayin/yayin.conf"
printf 'YAYIN_USER=stale\nDOMAIN=stale.example\n' >"$MOCK_ROOT/etc/yayin.conf"
for run in 1 2; do
  bash "$T/yayin/install.sh" "$T/yayin" tools
  values='. "$1"; printf "%s\n" "$YAYIN_USER" "$YAYIN_HOST" "$DOMAIN" "$CPUS" "$MEM" "$KEEP_RELEASES" "$HEALTH_TIMEOUT"'
  expected=$(bash -c "$values" _ "$T/yayin/yayin.conf")
  installed=$(bash -c "$values" _ "$MOCK_ROOT/etc/yayin.conf")
  [ "$installed" = "$expected" ] || { echo 'FAIL yayin: installed config differs from installer input' >&2; exit 1; }
done
echo 'PASS yayin (2 installs replace stale config)'

# A value inherited for a missing field must also survive without that environment.
sed -i '/^CPUS=/d' "$T/yayin/yayin.conf"
CPUS=3 bash "$T/yayin/install.sh" "$T/yayin" tools
installed=$(env -u CPUS bash -c '. "$1"; printf "%s" "$CPUS"' _ "$MOCK_ROOT/etc/yayin.conf")
[ "$installed" = 3 ]
echo 'PASS yayin inherited CPU snapshot'
