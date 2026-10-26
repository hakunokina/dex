# Upstream

- Kaynak: https://github.com/gamedev-skills/awesome-gamedev-agent-skills
- Sabit commit: `184b72be02884f782a0058f2eeae2ce4277453e4` (2026-10-24)
- Lisans: Apache-2.0 (`LICENSE`, `NOTICE` bu klasörde, değiştirilmeden)
- Dex işi: #2442 (güvenlik incelemesi #2441)

## Alınanlar (bayt bayt aynı, LF)

- `router/`
- `skills/` (aşağıdaki iki klasör hariç)
- `LICENSE`, `NOTICE`

## Alınmayanlar

- `tests/`, `docs/`, `scripts/` (site üreteci), `.github/`, `.claude-plugin/`, `templates/`,
  `README.md` ve görselleri, `CONTRIBUTING.md`, `.gitignore` — çalışma anında gerekmez.
- `skills/workflows/steam-publish/`, `skills/workflows/itch-publish/` — ajanı canlıya yüklemeye
  yönlendiriyor (`steamcmd run_app_build`, `butler push`); #2441 kararıyla ilk sürüme alınmadı.
  Router ve `references/` metni bunlara hâlâ atıf yapar; upstream metne dokunulmadı, Dex'in
  router'a eklediği not "yayın adımı için lidere sor" der.

## Dex'te nasıl yüklenir

Ağacın tamamı `build.rs` ile ikiliye gömülür (`crates/dex-core/src/skills.rs`, `TREE_SKILLS`).
Oyun rollerinin plugin dizininde ağaç `<plugin>/gamedev/` altına, `router/SKILL.md` ise
`skills/gamedev-router/SKILL.md` olarak yazılır: frontmatter adı `gamedev-router` yapılır ve
frontmatter'dan sonra paket kökünün mutlak yolunu veren Dex notu eklenir. Repodaki dosyalar
upstream ile aynı kalır; güncelleme = yeni sha'dan aynı klasörleri yeniden kopyalamak.
