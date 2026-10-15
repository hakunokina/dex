#!/bin/bash
# build-apk — Dex Android APK'sını derler, release anahtarıyla imzalar, ~/apk/dex-<sürüm>.apk'ya koyar.
# S5-A. Tek komutluk tekrar derleme. Gereksinimler (bir kez kurulur, bu dosya kurmaz):
#   JDK 17, Android SDK (platform-34, build-tools 34, NDK 26), Rust + aarch64-linux-android hedefi,
#   node 22 + pnpm, ~/.config/dex/release.keystore (0600) + keystore.pass (0600).
#   İsteğe bağlı (#1719 bildirimler): Firebase'in google-services.json'u repo DIŞINDA,
#   ~/.config/dex/google-services.json (0600; DEX_FCM_JSON ile değiştirilebilir). Yoksa APK FCM'siz derlenir.
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
# #1759 şeffaf paylaşım: WebView'sız tramplen (ShareActivity.kt, paket com.hakunokina.dex) + saydam
# MainActivity teması (dex-theme.xml → gen/ res/values/). Tramplen manifest blokla birlikte gelir.
SAT=src-tauri/android/ShareActivity.kt
SAD=src-tauri/gen/android/app/src/main/java/com/hakunokina/dex/ShareActivity.kt
[ -f "$SAT" ] && cp "$SAT" "$SAD"
THEME=src-tauri/android/dex-theme.xml
THD=src-tauri/gen/android/app/src/main/res/values/dex-theme.xml
[ -f "$THEME" ] && cp "$THEME" "$THD"
[ -f "$THD" ] || { echo "saydam tema kopyalanamadı (dex-theme.xml)"; exit 1; }
# Dex simgesi: `tauri android init` gen/'i varsayılan simgelerle üretir; repodaki Dex
# simgeleri (src-tauri/icons/android, `tauri icon` çıktısı) her derlemede gen/ res/
# üzerine kopyalanır (idempotent, varsayılanları ezer).
ICON_SRC=src-tauri/icons/android
ICON_DST=src-tauri/gen/android/app/src/main/res
[ -d "$ICON_SRC" ] || { echo 'Dex android simgeleri yok (src-tauri/icons/android)'; exit 1; }
mkdir -p "$ICON_DST"
cp -r "$ICON_SRC/." "$ICON_DST/"
cmp -s "$ICON_SRC/mipmap-xxxhdpi/ic_launcher.png" "$ICON_DST/mipmap-xxxhdpi/ic_launcher.png" \
  || { echo 'Dex simgeleri gen/ res/ altına kopyalanamadı'; exit 1; }
# #1719 Echo cevapları: bildirim köprüsü (jeton / bildirimden açılış / tarayıcı) + FCM servisi.
for k in PushPlugin DexMessagingService; do cp "src-tauri/android/$k.kt" "$(dirname "$DST")/$k.kt"; done
# #1859 Günlük motivasyon: WorkManager işi (host'tan çek → JezLauncher ContentProvider) + saf plan
# mantığı ve JVM birim testi (gen/ app/src/test; `./gradlew :app:testDebugUnitTest`).
for k in MotivationPlan MotivationWorker; do cp "src-tauri/android/$k.kt" "$(dirname "$DST")/$k.kt"; done
TDST=src-tauri/gen/android/app/src/test/java/com/hakunokina/dex/hosttokens
mkdir -p "$TDST" && cp src-tauri/android/MotivationPlanTest.kt "$TDST/"
M=src-tauri/gen/android/app/src/main/AndroidManifest.xml
awk -v f=src-tauri/android/share-aliases.xml '
  /Paylaş hedefi \(ShareTargetPlugin.kt\): metin/ { skip = 2; next }
  skip > 0 { if (/<\/intent-filter>/) skip--; next }
  /dex-share-aliases:begin/, /dex-share-aliases:end/ { next }
  !done && /<\/activity>/ { print; while ((getline l < f) > 0) print l; done = 1; next }
  { print }' "$M" > "$M.tmp" && mv "$M.tmp" "$M"
[ "$(grep -c '<activity-alias' "$M")" = 3 ] && [ "$(grep -c 'android.intent.action.SEND' "$M")" = 6 ] \
  || { echo "manifest'e Paylaş girişleri eklenemedi"; exit 1; }
[ "$(grep -c 'android:name=".ShareActivity"' "$M")" = 1 ] \
  || { echo "manifest'e ShareActivity eklenemedi"; exit 1; }
[ "$(grep -c 'targetActivity=".ShareActivity"' "$M")" = 3 ] \
  || { echo "Paylaş girişleri ShareActivity'ye bağlı değil"; exit 1; }
grep -q 'android:launchMode="singleTask"' "$M" || sed -i '0,/<activity/s//<activity android:launchMode="singleTask"/' "$M"
grep -q 'android:launchMode="singleTask"' "$M" || { echo 'MainActivity singleTask yapılamadı; Paylaş (açık uygulama) çalışmaz'; exit 1; }
# #1759: MainActivity saydam tema (soğuk paylaşımda arkada Instagram görünür). Açılış etiketi çok
# satıra yayılabileceği için awk ile MainActivity bloğunda değiştirilir/eklenir (idempotent).
awk '
  /<activity([[:space:]>]|$)/ { inact = 1; ismain = ($0 ~ /\.MainActivity/) }
  inact && /\.MainActivity/ { ismain = 1 }
  inact && ismain && /android:theme="/ { sub(/android:theme="[^"]*"/, "android:theme=\"@style/DexTranslucent\""); themed = 1 }
  inact && />/ { if (ismain && !themed) sub(/>/, " android:theme=\"@style/DexTranslucent\">"); inact = 0; ismain = 0 }
  { print }' "$M" > "$M.tmp" && mv "$M.tmp" "$M"
grep -q 'android:theme="@style/DexTranslucent"' "$M" || { echo 'MainActivity saydam tema yapılamadı'; exit 1; }
# #1719 FCM servisi: işaretli blok her derlemede silinip </application> önüne yeniden eklenir.
awk -v f=src-tauri/android/push-service.xml '
  /dex-push:begin/, /dex-push:end/ { next }
  !done && /<\/application>/ { while ((getline l < f) > 0) print l; done = 1 }
  { print }' "$M" > "$M.tmp" && mv "$M.tmp" "$M"
[ "$(grep -c 'android:name="com.hakunokina.dex.hosttokens.DexMessagingService"' "$M")" = 1 ] || { echo "manifest'e FCM servisi eklenemedi"; exit 1; }
# #1859 JezLauncher izni + queries: işaretli blok her derlemede silinip <application önüne yeniden
# eklenir. Sabitler MotivationWorker.kt'deki JezLauncher nesnesiyle aynı olmalı (aşağıda denetlenir).
MOT=src-tauri/android/motivation.xml
awk -v f="$MOT" '
  /dex-motivation:begin/, /dex-motivation:end/ { next }
  !done && /^[[:space:]]*<application([[:space:]>]|$)/ { while ((getline l < f) > 0) print l; done = 1 }
  { print }' "$M" > "$M.tmp" && mv "$M.tmp" "$M"
for k in AUTHORITY PERMISSION; do
  v="$(grep -oE "const val $k = \"[^\"]+\"" src-tauri/android/MotivationWorker.kt | cut -d'"' -f2)"
  [ -n "$v" ] && grep -q "\"$v\"" "$MOT" || { echo "motivation.xml ile MotivationWorker.kt JezLauncher.$k uyuşmuyor"; exit 1; }
  [ "$(grep -c "\"$v\"" "$M")" = 1 ] || { echo "manifest'e JezLauncher $k eklenemedi"; exit 1; }
done
grep -q security-crypto "$G" || sed -i 's|implementation("androidx.webkit:webkit:1.14.0")|implementation("androidx.webkit:webkit:1.14.0")\n    implementation("androidx.security:security-crypto:1.1.0-alpha06")|' "$G"
# #1719 FCM: kütüphane her zaman (Kotlin servisi derlensin); google-services eklentisi yalnız
# google-services.json varken. Dosya yoksa FirebaseApp kurulmaz: uygulama çalışır, jeton/bildirim yok.
# Dosya repoya girmez; gen/ içine her derlemede kopyalanır (yoksa oradan da silinir).
grep -q firebase-messaging "$G" || sed -i 's|implementation("androidx.webkit:webkit:1.14.0")|implementation("androidx.webkit:webkit:1.14.0")\n    implementation("com.google.firebase:firebase-messaging:24.1.0")|' "$G"
grep -q firebase-messaging "$G" || { echo "firebase-messaging bağımlılığı eklenemedi"; exit 1; }
# #1859: WorkManager (sabah işi) + OkHttp (native WebSocket) + JUnit (yalnız birim testi). Kotlin 1.9
# şablonuyla uyumlu sürümler.
grep -q androidx.work:work-runtime "$G" || sed -i 's|implementation("androidx.webkit:webkit:1.14.0")|implementation("androidx.webkit:webkit:1.14.0")\n    implementation("androidx.work:work-runtime:2.9.1")|' "$G"
grep -q com.squareup.okhttp3:okhttp "$G" || sed -i 's|implementation("androidx.webkit:webkit:1.14.0")|implementation("androidx.webkit:webkit:1.14.0")\n    implementation("com.squareup.okhttp3:okhttp:4.12.0")|' "$G"
grep -q 'testImplementation("junit:junit' "$G" || sed -i 's|implementation("androidx.webkit:webkit:1.14.0")|implementation("androidx.webkit:webkit:1.14.0")\n    testImplementation("junit:junit:4.13.2")|' "$G"
grep -q androidx.work:work-runtime "$G" && grep -q com.squareup.okhttp3:okhttp "$G" && grep -q 'testImplementation("junit:junit' "$G" \
  || { echo "WorkManager/OkHttp/JUnit bağımlılığı eklenemedi"; exit 1; }
FCM_JSON="${DEX_FCM_JSON:-$HOME/.config/dex/google-services.json}"
R=src-tauri/gen/android/build.gradle.kts
if [ -f "$FCM_JSON" ]; then
  cp "$FCM_JSON" src-tauri/gen/android/app/google-services.json
  grep -q 'com.google.gms:google-services' "$R" \
    || sed -i 's|classpath("com.android.tools.build:gradle:[^"]*")|&\n        classpath("com.google.gms:google-services:4.4.2")|' "$R"
  grep -q 'id("com.google.gms.google-services")' "$G" \
    || sed -i 's|id("org.jetbrains.kotlin.android")|&\n    id("com.google.gms.google-services")|' "$G"
  grep -q 'com.google.gms:google-services' "$R" && grep -q 'id("com.google.gms.google-services")' "$G" \
    || { echo "google-services eklentisi eklenemedi"; exit 1; }
  echo "FCM: açık"
else
  rm -f src-tauri/gen/android/app/google-services.json
  sed -i '/id("com.google.gms.google-services")/d' "$G"
  echo "FCM: kapalı (google-services.json yok) — bildirimsiz derleniyor"
fi

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
