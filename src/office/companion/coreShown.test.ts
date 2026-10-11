// #247: Core şeridi açık ve görünürken köşe listesi o liderin kartlarını gizler.
import { strict as assert } from "node:assert";
import { describe, it } from "node:test";
import {
  coveredLeaders, deferredMap, isDeferred, officeLeaders, setCoreShown, setDeferred, setOfficeShown, stripBalloonOpen, stripCovers,
  subscribeCoreShown,
} from "./coreShown.ts";

describe("stripCovers", () => {
  it("açık + görünür → köşe gizli; küçük/gizli → köşe görünür", () => {
    assert.equal(stripCovers("open", true), true);
    assert.equal(stripCovers("compact", true), false);
    assert.equal(stripCovers("hidden", true), false);
  });
  it("lider pane görünmüyor (başka sekme/takım) → köşe görünür", () => {
    assert.equal(stripCovers("open", false), false);
  });
});

describe("coreShown store", () => {
  it("yalnız kaydolan liderin kartları kapsanır; aynı lider iki pane'de, biri kapanınca sürer", () => {
    let calls = 0;
    const off = subscribeCoreShown(() => calls++);
    const a = {}, b = {}, c = {};
    setCoreShown(a, "local:lead1");
    setCoreShown(b, "local:lead1");
    setCoreShown(c, "h2:lead2");
    assert.deepEqual([...coveredLeaders()].sort(), ["h2:lead2", "local:lead1"]);
    setCoreShown(a, null);
    assert.equal(coveredLeaders().has("local:lead1"), true);
    setCoreShown(b, null);
    assert.equal(coveredLeaders().has("local:lead1"), false);
    assert.equal(coveredLeaders().has("h2:lead2"), true);
    const before = calls;
    setCoreShown(b, null); // değişiklik yok → yayın yok, anlık görüntü aynı
    assert.equal(calls, before);
    setCoreShown(c, null);
    assert.equal(coveredLeaders().size, 0);
    off();
  });
});

describe("#252 ofis Core'u balonu taşır", () => {
  it("ofis görünür → strip balonu kapalı; ofis gizli → strip balonu açık", () => {
    const office = {};
    setOfficeShown(office, ["local:lead1"]);
    assert.equal(stripBalloonOpen("open", 1, false, officeLeaders().has("local:lead1")), false);
    assert.equal(coveredLeaders().has("local:lead1"), true); // köşe listesi de gizler
    setOfficeShown(office, null);
    assert.equal(stripBalloonOpen("open", 1, false, officeLeaders().has("local:lead1")), true);
    assert.equal(stripBalloonOpen("compact", 1, false, false), true);
    assert.equal(stripBalloonOpen("hidden", 1, false, false), false);
    assert.equal(stripBalloonOpen("open", 0, false, false), false);
    assert.equal(stripBalloonOpen("open", 1, true, false), false);
    assert.equal(coveredLeaders().size, 0);
  });
  it("aynı liste tekrar gelince yayın yok; strip + ofis birleşir", () => {
    let calls = 0;
    const off = subscribeCoreShown(() => calls++);
    const office = {}, strip = {};
    setOfficeShown(office, ["a", "b"]);
    setOfficeShown(office, ["a", "b"]);
    assert.equal(calls, 1);
    setCoreShown(strip, "c");
    assert.deepEqual([...coveredLeaders()].sort(), ["a", "b", "c"]);
    setOfficeShown(office, null);
    setCoreShown(strip, null);
    assert.equal(coveredLeaders().size, 0);
    off();
  });
  it("Esc ertelemesi lider başına ortak: strip ve ofis aynı durumu görür; yeni kart açar", () => {
    setDeferred("L", ["p1", "q2"]);
    assert.equal(isDeferred("L", ["p1"]), true); // karar verildi → kalan kart hâlâ ertelenmiş
    assert.equal(isDeferred("L", ["p1", "p3"]), false); // yeni kart
    assert.equal(isDeferred("M", ["p1"]), false);
    setDeferred("L", null);
    assert.equal(isDeferred("L", ["p1"]), false);
    assert.equal(deferredMap().size, 0);
  });
});
