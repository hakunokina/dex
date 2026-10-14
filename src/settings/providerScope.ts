// Sağlayıcı kartının sunucu kapsamlı anahtarı: aynı sağlayıcı kimliği iki
// sunucuda aynı kart değildir. Sunucu değişince kart yeniden kurulur;
// yazılmamış anahtar taslağı ve satır mesajı öbür sunucuya taşınmaz
// (güven sınırı). Saf modül: birim test node:test ile çalışır.
export function providerScopeKey(host: string, providerId: string) {
  return `${host}:${providerId}`;
}
