Tauri 2 masaüstü uygulaması: Rust arka uç + React/TypeScript (Vite) arayüz.

### Proje yapısı
- `crates/dex-core/` — çekirdek (Rust): DB, hook'lar, MCP, kurallar; birim testleri burada.
- `src-tauri/` — Tauri kabuğu. `src/` — React/TS arayüz (saf mantık `*.test.ts`, node:test). `e2e/` — Playwright.

### Test komutları
- Rust: `cargo check`, `cargo test -p dex-core --lib`.
- TS: `npx tsc --noEmit`, `npm test`.
- Tarayıcı testi (yalnız UI akışını değiştiren işte, yalnız ilgili spec): `npx playwright test <dosya> --workers=1` ya da `--grep "<ad>"`; video/trace kapalı.
- Tüm e2e paketi yalnız QA'nın işi (plan sonunda ya da patron isteyince, `--workers=2`).

### Derleme/çalıştırma
- `npm run build` (tsc + vite); masaüstü paketi `.dex/release/desktop/`.
