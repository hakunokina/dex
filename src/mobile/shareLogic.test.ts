import { test } from "node:test";
import assert from "node:assert/strict";
import {
  IMAGE_MAX_BYTES,
  assistantTeams,
  base64Bytes,
  buildSubmit,
  fitUnderLimit,
  hasShare,
  imageFitsAsIs,
  initialText,
  resolveShareHost,
  resultText,
  scaleToFit,
  shouldReloadTargets,
  validateSubmit,
} from "./shareLogic.ts";

const png = (bytes: number) => ({ mime: "image/png" as const, base64: "A".repeat(Math.ceil((bytes * 4) / 3)) });

test("hasShare / initialText: metin, yoksa konu; boş paylaşım yok sayılır", () => {
  assert.ok(!hasShare(null) && !hasShare({}));
  assert.ok(hasShare({ subject: "x" }) && hasShare({ error: "Görsel çok büyük" }));
  assert.equal(initialText({ text: "  https://instagram.com/reel/abc  ", subject: "s" }), "https://instagram.com/reel/abc");
  assert.equal(initialText({ subject: " konu " }), "konu");
  assert.equal(initialText({}), "");
});

test("base64Bytes: dolgulu ve dolgusuz", () => {
  assert.equal(base64Bytes(""), 0);
  assert.equal(base64Bytes("TQ=="), 1);
  assert.equal(base64Bytes("TWE="), 2);
  assert.equal(base64Bytes("TWFu"), 3);
});

test("scaleToFit: uzun kenar 1600, oran korunur, küçük görsele dokunulmaz", () => {
  assert.deepEqual(scaleToFit(800, 600), { width: 800, height: 600 });
  assert.deepEqual(scaleToFit(3200, 1600), { width: 1600, height: 800 });
  assert.deepEqual(scaleToFit(1000, 4000), { width: 400, height: 1600 });
  assert.deepEqual(scaleToFit(10000, 1), { width: 1600, height: 1 });
});

test("imageFitsAsIs: MIME + bayt + kenar", () => {
  assert.ok(imageFitsAsIs("image/jpeg", IMAGE_MAX_BYTES, 1600, 900));
  assert.ok(!imageFitsAsIs("image/jpeg", IMAGE_MAX_BYTES + 1, 100, 100));
  assert.ok(!imageFitsAsIs("image/jpeg", 10, 1601, 100));
  assert.ok(!imageFitsAsIs("image/gif", 10, 10, 10));
  assert.ok(!imageFitsAsIs("image/heic", 10, 10, 10));
});

test("fitUnderLimit: ilk sığan kalite; hiçbiri sığmazsa null", async () => {
  const tried: number[] = [];
  const sizes: Record<number, number> = { 0.8: 900_000, 0.6: 600_000, 0.4: 300_000 };
  const enc = async (q: number) => (tried.push(q), { value: `q${q}`, bytes: sizes[q] });
  assert.equal(await fitUnderLimit(enc), "q0.6");
  assert.deepEqual(tried, [0.8, 0.6], "sığınca durur");
  assert.equal(await fitUnderLimit(async (q) => ({ value: q, bytes: IMAGE_MAX_BYTES + 1 })), null);
  assert.equal(await fitUnderLimit(async (q) => ({ value: q, bytes: IMAGE_MAX_BYTES })), 0.8, "sınırın kendisi sığar");
});

test("validateSubmit: asistan yok, boş, uzun metin, büyük/yanlış görsel", () => {
  assert.equal(validateSubmit("merhaba", null, null), "Bu sunucuda asistan yok");
  assert.equal(validateSubmit("   ", null, "t1"), "Gönderilecek bir şey yok");
  assert.equal(validateSubmit("a".repeat(8 * 1024), null, "t1"), null);
  assert.equal(validateSubmit("a".repeat(8 * 1024 + 1), null, "t1"), "Metin çok uzun (en fazla 8 KB)");
  assert.equal(validateSubmit("é".repeat(4 * 1024 + 1), null, "t1"), "Metin çok uzun (en fazla 8 KB)", "bayt sayılır, karakter değil");
  assert.equal(validateSubmit("", png(1000), "t1"), null, "yalnız görsel yeter");
  assert.equal(validateSubmit("", png(IMAGE_MAX_BYTES + 10), "t1"), "Görsel çok büyük");
  assert.equal(validateSubmit("", { mime: "image/gif" as never, base64: "AAAA" }, "t1"), "Görsel biçimi desteklenmiyor");
});

test("buildSubmit: teamId + clientId her zaman, boş alan yok", () => {
  assert.deepEqual(buildSubmit("t1", "  merhaba ", null, "c1"), { teamId: "t1", clientId: "c1", text: "merhaba" });
  const img = png(10);
  assert.deepEqual(buildSubmit("t1", " ", img, "c1"), { teamId: "t1", clientId: "c1", image: img });
});

test("assistantTeams: yalnız asistan üyesi olan takımlar, takım sırasıyla", () => {
  const teams = [{ id: "a" }, { id: "b" }, { id: "c" }];
  const agents = [
    { teamId: "c", roleSlug: "assistant" },
    { teamId: "b", roleSlug: "backend" },
    { teamId: "a", roleSlug: null },
    { teamId: "c", roleSlug: "assistant" },
  ];
  assert.deepEqual(assistantTeams(teams, agents), [{ id: "c" }]);
  assert.deepEqual(assistantTeams(teams, []), []);
});

test("resultText", () => {
  assert.equal(resultText({ queued: true }), "Sıraya alındı");
  assert.equal(resultText({ queued: false }), "Asistana gönderildi");
  assert.equal(resultText(null), "Asistana gönderildi");
});

test("resolveShareHost: varsayılan sonradan gelirse seçim ona döner; açık seçim ezilmez", () => {
  const ids = ["A", "B"];
  // soğuk açılış: varsayılan henüz yok → ilk kayıtlı; sonra B gelince B
  assert.equal(resolveShareHost(ids, null, null), "A");
  assert.equal(resolveShareHost(ids, null, "B"), "B");
  // kullanıcı A'yı açıkça seçti → varsayılan değişse de A
  assert.equal(resolveShareHost(ids, "A", "B"), "A");
  // seçilen host kaldırıldı / varsayılan kayıtlı değil → geri düşüş
  assert.equal(resolveShareHost(["B"], "A", "B"), "B");
  assert.equal(resolveShareHost(ids, null, "Z"), "A");
  assert.equal(resolveShareHost([], "A", "B"), null);
});

test("shouldReloadTargets: yalnız başarısız aramada ve seçili host bağlanınca", () => {
  assert.equal(shouldReloadTargets("A", "A", true), true);
  assert.equal(shouldReloadTargets("B", "A", true), false);
  assert.equal(shouldReloadTargets("A", "A", false), false); // ilk yükleme sürüyor/başarılı: çift istek yok
  assert.equal(shouldReloadTargets("A", null, true), false);
});
