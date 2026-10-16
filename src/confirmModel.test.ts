import { strict as assert } from "node:assert";
import { describe, it } from "node:test";
import { confirmParts, serial } from "./confirmModel.ts";

describe("confirmParts", () => {
  it("ilk paragraf başlık, kalanlar gövde; tek \n paragrafı bölmez", () => {
    assert.deepEqual(confirmParts("Kapatılsın mı?\n\nA satırı\nB satırı\n\nSon."), {
      title: "Kapatılsın mı?",
      body: ["A satırı\nB satırı", "Son."],
    });
  });
  it("tek cümle: yalnız başlık", () => {
    assert.deepEqual(confirmParts("Arşivlensin mi?"), { title: "Arşivlensin mi?", body: [] });
  });
  it("verilen başlıkta metnin tamamı gövde", () => {
    assert.deepEqual(confirmParts("Açıklama.\n\nDevam?", "Başlık"), { title: "Başlık", body: ["Açıklama.", "Devam?"] });
  });
});

describe("serial", () => {
  it("ikinci soru birincinin cevabını bekler; ret kuyruğu kırmaz", async () => {
    const run = serial();
    const log: string[] = [];
    let answer!: (v: boolean) => void;
    const first = run(() => { log.push("1 açıldı"); return new Promise<boolean>((r) => { answer = r; }); });
    const second = run(async () => { log.push("2 açıldı"); return true; });
    const third = run(async () => { throw new Error("x"); });
    const fourth = run(async () => { log.push("4 açıldı"); return false; });
    await new Promise((r) => setTimeout(r, 10));
    assert.deepEqual(log, ["1 açıldı"]);
    answer(false);
    assert.equal(await first, false);
    assert.equal(await second, true);
    await assert.rejects(third);
    assert.equal(await fourth, false);
    assert.deepEqual(log, ["1 açıldı", "2 açıldı", "4 açıldı"]);
  });
});
