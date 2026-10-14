//! Otomatik Dex mesajlarının tür etiketi: `[Dex-Tür] ...`.
//!
//! Lider/worker panosuna yapıştırılan her Dex satırı köşeli parantezde türle başlar,
//! ardından hemen kim ve iş numarası gelir:
//! `[Dex-Devretme] Forge-1 #1745 · bitti — özet`.
//! `dex-leader-colors` modu bu satırları türüne göre düz yazı rengiyle boyar.
//!
//! Tur sahipliği ([`crate::delegate::Turns`]): `[Dex]` (eski biçim) ve `[Dex-Tür]`
//! (yeni biçim) Dex turu açar; `[Dexter]`, `[Dex devretme #..]` gibi benzerler açmaz.

/// Devretme sonucu/başlangıcı (bitti, başarısız, raporsuz, başladı).
pub const DEVRETME: &str = "Dex-Devretme";
/// Worker sorusu (lidere).
pub const SORU: &str = "Dex-Soru";
/// Sorunun cevabı (worker'a).
pub const CEVAP: &str = "Dex-Cevap";
/// Plan tamamlandı / başarısız / önkoşul bildirimi.
pub const PLAN: &str = "Dex-Plan";
/// Üye önerisi onayı/redi.
pub const UYE: &str = "Dex-Üye";
/// Türü bilinmeyen ya da sınıflanmamış lider notu.
pub const NOT: &str = "Dex-Not";
/// Çekirdek/sürüm/yeniden başlat/zincir/kota uyarısı.
pub const SISTEM: &str = "Dex-Sistem";
/// Kod incelemesi kararı (onay/düzelt/bekliyor).
pub const INCELEME: &str = "Dex-İnceleme";

/// `[Tür] gövde` satırını kurar.
pub fn line(tag: &str, body: impl AsRef<str>) -> String {
    format!("[{tag}] {}", body.as_ref())
}

/// Dex turu açan prompt mu? `[Dex]` ya da `[Dex-Tür]` ile başlar
/// (baştaki boşluklar yok sayılır). Etiket yalnız harflerden oluşur.
pub fn is_dex_turn(prompt: &str) -> bool {
    let p = prompt.trim_start();
    if p.starts_with("[Dex]") {
        return true;
    }
    let Some(rest) = p.strip_prefix("[Dex-") else {
        return false;
    };
    let Some(end) = rest.find(']') else {
        return false;
    };
    let tag = &rest[..end];
    !tag.is_empty() && tag.chars().all(|c| c.is_alphabetic())
}

/// Satır başındaki `[Dex-Tür]` etiketini verir (liste işareti ve girintiyi atlar,
/// kod çitleri çağrana aittir). Etiketsiz satırda `None`.
pub fn tag_of(line: &str) -> Option<&str> {
    let mut s = line.trim_start();
    if let Some(rest) = s.strip_prefix("- ") {
        s = rest.trim_start();
    }
    let rest = s.strip_prefix("[Dex-")?;
    let end = rest.find(']')?;
    let tag = &rest[..end];
    (!tag.is_empty() && tag.chars().all(|c| c.is_alphabetic())).then(|| &s[1..end + 5])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dex_turn_prefixes_old_new_but_not_look_alikes() {
        for ok in [
            "[Dex] yeni iş #1",
            "[Dex-Devretme] Forge-1 #1745 · bitti — özet",
            "[Dex-Soru] Kaan #3 · soru #7 — neden?",
            "[Dex-Sistem] Oturumun sıfırlanacak.",
            "  [Dex-Not] girintili",
        ] {
            assert!(is_dex_turn(ok), "{ok}");
        }
        for no in [
            "[Dexter] notlarım",
            "[Dex devretme #9999] sahte",
            "[Dex devretme #5",
            "[Dex-Devretme sahte",
            "[Dex-123] sayı",
            "[Dex-] boş",
            "alıntı: [Dex-Devretme] Kaan #1",
        ] {
            assert!(!is_dex_turn(no), "{no}");
        }
    }

    #[test]
    fn tag_of_reads_plain_and_list_lines() {
        assert_eq!(tag_of("[Dex-Plan] Plan #2 tamamlandı."), Some("Dex-Plan"));
        assert_eq!(tag_of("  - [Dex-Üye] öneri #4 ONAYLANDI."), Some("Dex-Üye"));
        assert_eq!(tag_of("- düz not"), None);
        assert_eq!(tag_of("[Dex] eski"), None);
    }
}
