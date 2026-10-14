# Masaüstü uygulama kimliği ve veri kökü — #1311

Paket kimliği `com.hakunokina.dex`, publisher `hakunokina` ve MSI UpgradeCode `db5b1b64-92a1-56cd-bb39-9d6086178561` sabittir. Veri kökü seçimi paket kimliğinden ayrıdır; eski kurulum yerinde kullanılır.

`crates/dex-core/src/desktop_storage.rs` tek seçim noktasıdır. Eski veri dizini ve okunabilir bir `dex.db` dosyası varsa **bütün** masaüstü yolları eski kimliği kullanır. Eski DB yoksa yeni kimlik kullanılır. Yeni kök dolu olsa bile kullanılabilir eski kök önceliklidir. QA/test kimlikleri eski patron verisini seçmez.

Hiçbir dosya kopyalanmaz, taşınmaz veya yeniden eşlenmez. SQLite kayıtları, Git worktree bağlantıları ve layout shell `cwd` değerleri değişmez. Kök seçimi dosya/dizin oluşturmaz, DB açmaz veya kilit almaz. Çalışan eski core'un kilidi seçim kararını değiştirmez; mevcut discovery ve normal core kilit protokolü korunur.

| Bileşen | Seçilen kök |
|---|---|
| Windows veri/config | `%APPDATA%/<seçilen kimlik>` |
| Windows WebView2 / localStorage / core binary cache | `%LOCALAPPDATA%/<seçilen kimlik>` |
| Linux veri/config/local | Platformun data/config/local tabanlarında aynı seçilen kimlik |
| macOS veri/config/local | `Library/Application Support/<seçilen kimlik>` |
| Core ve shell logları | Seçilen veri kökünün `logs` dizini |
| Shell/layout | Seçilen config kökünün `layout.json` dosyası; kayıtlı yollar aynen kalır |
| `dexd --local` | Shell'in seçtiği `--data-dir` ve `--config-dir` |
| `scripts/dexd-dev.mjs` | `dexd --desktop-data-dir <paket kimliği>` ile aynı Rust seçicisi |
| Standalone `dexd` server | Mevcut bağımsız `dexd` veri dizini |

Tauri context hazırlanırken seçim yapılır; `AppPaths` state'i core_proc, inproc host, log açma ve snip tarafından kullanılır. Windows/Linux ana pencereleri aynı `data_directory` ile açılır. Tray'den yeniden açılan pencere hazırlanmış config'i, snip aynı state'i kullanır. Windows eski Local profilindeki sunucu/eşleme listesi korunur; profil kopyası yapılmaz.

macOS WKWebView'de Windows/Linux `data_directory` ayarı mevcut değildir. Bu iş Windows WebView2 profil seçimini doğrular; macOS native WebView profil davranışı cihazda doğrulanmadı.

Eski kök adı yalnız `desktop_storage::LEGACY_IDENTIFIER` sabitinde, kabul edilmiş uyumluluk istisnasını açıklayan yorumla tutulur. Kimlikten bağımsız keyring servisleri (`dex-provider`, `dex-host`, `dex-tm-hub`, `dex-gpu`) sabittir.

NSIS'in eski kurulum konumunu yeni publisher kaydına aktaran hook'u değişmedi; uygulama verisi göçü değildir. Tek `Dex` uninstall kaydı ve eski kurulum yolu korunur. `scripts/test-identity-installer.ps1` gerçek uygulamayı kurmadan izole registry anahtarlarıyla beş kontrol yapar. Tauri CLI yükseltilirken bu kontrol tekrar çalıştırılmalıdır. NSIS'in eski publisher'lı MSI'ı tanımaması önceki lider kararında kabul edildi.

Android #481 kararı değişmedi: yeni applicationId ayrı pakettir, eski APK/veri kalır; yeni pakette bir kez yeniden eşleme gerekir. Kotlin/Rust köprü paketi, R8 ve build kimlik kontrolleri yeni kimlikte kalır.

Şema migration'ı veya toplu SQL güncellemesi yoktur. Geri dönüş eski sürümü açmaktır; aynı eski veri kökünü kullanır. Testler çalışma ağacındaki `.dex/tmp` altında izole köklerle çalışır. Windows Known Folder API'leri nedeniyle yalnız APPDATA değiştirerek gerçek Tauri shell'i açmak güvenli izolasyon değildir; patron kurulumu/verisi kullanılmaz.
