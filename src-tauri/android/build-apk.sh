#!/bin/bash
# build-apk — Dex Android APK'sını derler, release anahtarıyla imzalar, ~/apk/dex-<sürüm>.apk'ya koyar.
# S5-A. Tek komutluk tekrar derleme. Gereksinimler (bir kez kurulur, bu dosya kurmaz):
#   JDK 17, Android SDK (platform-34, build-tools 34, NDK 26), Rust + aarch64-linux-android hedefi,
#   node 22 + pnpm, ~/.config/dex/release.keystore (0600) + keystore.pass (0600).
# Ayrı derleme kullanıcısı: servis kullanıcısı ajan ortamını çalıştırır (dexd.service); derleme araçları
# (SDK ~10GB, Rust toolchain, Gradle önbelleği) o ortamı kirletmesin diye ayrı kullanıcıda.
set -euo pipefail

SRC="${DEX_SRC:-$HOME/dex-build}"
OUT="$HOME/apk"
VER="$(node -p "require('$SRC/package.json').version" 2>/dev/null || echo 0.1.0)"
APK="$OUT/dex-$VER.apk"

export PATH="$HOME/.cargo/bin:$PATH"
export ANDROID_HOME="$HOME/android-sdk" ANDROID_SDK_ROOT="$HOME/android-sdk"
export JAVA_HOME=/usr/lib/jvm/java-17-openjdk-amd64
export NDK_HOME="$ANDROID_HOME/ndk/26.1.10909125"
export GRADLE_OPTS="-Xmx8g"

KS="$HOME/.config/dex/release.keystore"
[ -f "$KS" ] || { echo "YOK: $KS (keytool ile üret, 0600)"; exit 1; }

cd "$SRC"
if [ -d .git ] && git rev-parse --abbrev-ref --symbolic-full-name @{u} >/dev/null 2>&1; then
  git pull --ff-only
fi
[ -d src-tauri/gen/android ] || pnpm tauri android init

# A cached generated project still has the old applicationId. Never publish that APK silently;
# initialize the new package in a clean checkout (release-apk already builds from git archive).
G=src-tauri/gen/android/app/build.gradle.kts
grep -Eq 'namespace[[:space:]]*=[[:space:]]*"com.hakunokina.dex"' "$G" \
  && grep -Eq 'applicationId[[:space:]]*=[[:space:]]*"com.hakunokina.dex"' "$G" \
  || { echo 'Android gen/ eski paket kimliğinde; temiz checkout ile tauri android init çalıştırın'; exit 1; }

# S5-A Kotlin köprüsünün sürümlü kopyası src-tauri/android/'da durur; init/gen üzerine kopyalanır
# (gen/ commit'lenene kadar sunucuda üretilir).
KT=src-tauri/android/HostTokensPlugin.kt
DST=src-tauri/gen/android/app/src/main/java/com/hakunokina/dex/hosttokens/HostTokensPlugin.kt
if [ -f "$KT" ]; then
  mkdir -p "$(dirname "$DST")"
  cp "$KT" "$DST"
fi
# Paylaş hedefi (Instagram vb. → Paylaş → Dex): ikinci Kotlin köprüsü + manifest'te üç Paylaş girişi
# (activity-alias, share-aliases.xml; alias adı görevi seçer). gen/ commit'li değil; her derlemede
# işaretli blok silinip güncel hali yeniden eklenir (idempotent). Önbellekli gen/'deki eski (#1698)
# yama — MainActivity içine doğrudan konmuş iki SEND filtresi — de silinir; yoksa dördüncü giriş kalır.
# MainActivity singleTask olmalı (açık uygulamaya yeni paylaşım onNewIntent ile gelir).
SKT=src-tauri/android/ShareTargetPlugin.kt
[ -f "$SKT" ] && cp "$SKT" "$(dirname "$DST")/ShareTargetPlugin.kt"
M=src-tauri/gen/android/app/src/main/AndroidManifest.xml
awk -v f=src-tauri/android/share-aliases.xml '
  /Paylaş hedefi \(ShareTargetPlugin.kt\): metin/ { skip = 2; next }
  skip > 0 { if (/<\/intent-filter>/) skip--; next }
  /dex-share-aliases:begin/, /dex-share-aliases:end/ { next }
  !done && /<\/activity>/ { print; while ((getline l < f) > 0) print l; done = 1; next }
  { print }' "$M" > "$M.tmp" && mv "$M.tmp" "$M"
[ "$(grep -c '<activity-alias' "$M")" = 3 ] && [ "$(grep -c 'android.intent.action.SEND' "$M")" = 6 ] \
  || { echo "manifest'e Paylaş girişleri eklenemedi"; exit 1; }
grep -q 'android:launchMode="singleTask"' "$M" || sed -i '0,/<activity/s//<activity android:launchMode="singleTask"/' "$M"
grep -q 'android:launchMode="singleTask"' "$M" || { echo 'MainActivity singleTask yapılamadı; Paylaş (açık uygulama) çalışmaz'; exit 1; }
grep -q security-crypto "$G" || sed -i 's|implementation("androidx.webkit:webkit:1.14.0")|implementation("androidx.webkit:webkit:1.14.0")\n    implementation("androidx.security:security-crypto:1.1.0-alpha06")|' "$G"

# Release R8 strips classes it can't see referenced; the bridge is loaded by name from Rust.
P=src-tauri/gen/android/app/proguard-rules.pro
grep -q "com.hakunokina.dex.hosttokens" "$P" || printf '\n%s\n' '-keep class com.hakunokina.dex.hosttokens.** { *; }' >> "$P"
# Tink (security-crypto) references compile-only annotations that are absent at runtime.
grep -q "dontwarn javax.annotation" "$P" || printf '\n%s\n%s\n' '-dontwarn javax.annotation.**' '-dontwarn com.google.errorprone.annotations.**' >> "$P"
# dexd speaks ws:// over the tailnet (WireGuard-encrypted); release builds block cleartext by default.
grep -q 'release") {\s*$' "$G" && ! grep -q 'usesCleartextTraffic"] = "true"' <(sed -n '/getByName("release")/,/}/p' "$G") \
  && sed -i '/getByName("release") {/a\            manifestPlaceholders["usesCleartextTraffic"] = "true"' "$G"
pnpm tauri android build --apk -t aarch64 --ci

UNSIGNED="$(find src-tauri/gen/android/app/build/outputs/apk -name '*.apk' | head -1)"
[ -n "${UNSIGNED:-}" ] || { echo "APK bulunamadı"; exit 1; }

mkdir -p "$OUT"
ALIGN="$ANDROID_HOME/build-tools/34.0.0/zipalign"
SIGN="$ANDROID_HOME/build-tools/34.0.0/apksigner"
"$ALIGN" -f 4 "$UNSIGNED" "$APK.tmp.apk"
"$SIGN" sign --ks "$KS" --ks-pass "file:$HOME/.config/dex/keystore.pass" --out "$APK" "$APK.tmp.apk"
rm "$APK.tmp.apk"
"$SIGN" verify --print-certs "$APK" | head -4
echo "OK: $APK"
