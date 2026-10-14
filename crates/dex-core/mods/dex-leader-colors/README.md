# Dex leader colors

Claude Code 2.1.292 ile doğrulandı. `ui.render` / `AssistantMessage` dışında
hiçbir olaya müdahale etmez. Başlıklar satır başında tek başına yazılır:

- `Senden beklenen:` → tema `warning`
- `Süren:` → tema `success`
- `Biten:` → tema `suggestion`

Başlık kalın ve renklidir; gövde renksiz Markdown ve renkli sol şerittir.
Başlıksız mesajlar ve `isSummary` blokları `next(e)` ile aynen devam eder.
Kod bloklarının içindeki başlıklar grup sayılmaz. Terminal, desktop, VS Code
ve mobile yüzeyleri test edilir; gerekli eleman yoksa varsayılan render sürer.

Dex modu ikiliye gömer ve lider başlangıcında `skills.root/dex-leader-colors`
altına kurar. Yalnız Claude liderin spawn env'ine `CLAUDE_CODE_PLUGIN_DIRS`
eklenir; mevcut değer korunur. Yol ayracı Windows'ta `;`, diğer platformlarda
`:`. Worker ve inceleyicilere bu mod Dex tarafından eklenmez.

```sh
claude plugin validate --strict crates/dex-core/mods/dex-leader-colors
claude plugin test crates/dex-core/mods/dex-leader-colors
```

Birleşen sürüm çalıştırıldıktan sonra lider oturumu yeniden açılır.
Canlı TUI boyaması bu testlerle doğrulanmaz; testler engine'in ürettiği ve
yüzeyin kabul ettiği ağaçları doğrular.

API kaynağı: [Claude Code mods reference](https://code.claude.com/docs/en/plugins/mods/reference).
Kurulu 2.1.292'nin `plugin-authoring/reference.md` ve `types/claude-code.d.ts`
dosyaları esas alındı; SDK tipleri kopyalanmaz.
