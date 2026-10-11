// Renk atama, komşuluk, filtre — `pnpm test` ile koşar (node --test, bağımlılık yok).
import { describe, it } from "node:test";
import assert from "node:assert/strict";
import {
  agentColor, buildAdjacency, degree, filterNodes, MISSING_COLOR, neighbors, nodeRadius, resolveLink,
  SHARED_COLOR, type MemNode,
} from "./graph.ts";

const n = (id: string, over: Partial<MemNode> = {}): MemNode => ({
  id, name: id, scope: "agent", agentId: "a1", description: "", type: null,
  links: [], updatedAt: 0, size: 0, ...over,
});

describe("agentColor", () => {
  it("yeni team/project kapsamları ortak renk, private ajan rengi kullanır", () => {
    assert.equal(agentColor({ scope: "team", agentId: null }), SHARED_COLOR);
    assert.equal(agentColor({ scope: "project", agentId: null }), SHARED_COLOR);
    assert.equal(agentColor({ scope: "private", agentId: "a1" }), agentColor({ scope: "agent", agentId: "a1" }));
  });
  it("shared tek nötr renk, missing soluk", () => {
    assert.equal(agentColor({ scope: "shared", agentId: null }), SHARED_COLOR);
    assert.equal(agentColor({ scope: "missing", agentId: "a1" }), MISSING_COLOR);
    assert.equal(agentColor({ scope: "agent", agentId: null }), MISSING_COLOR);
  });
  it("aynı ajan hep aynı renk, farklı ajanlar palet içinde", () => {
    const c1 = agentColor({ scope: "agent", agentId: "x" });
    assert.equal(c1, agentColor({ scope: "agent", agentId: "x" }));
    assert.notEqual(c1, SHARED_COLOR);
    const others = new Set(["y", "z", "w", "v", "u", "t"].map((a) => agentColor({ scope: "agent", agentId: a })));
    assert.ok(others.size >= 2, "tek renge yığılmamalı");
  });
});

describe("komşuluk", () => {
  const nodes = [n("a"), n("b"), n("c")];
  const adj = buildAdjacency(nodes, [{ from: "a", to: "b" }, { from: "b", to: "a" }, { from: "x", to: "a" }, { from: "a", to: "a" }]);
  it("kenar iki yönlü, yok/öz-döngü elenir", () => {
    assert.deepEqual([...neighbors(adj, "a")], ["b"]);
    assert.deepEqual([...neighbors(adj, "b")], ["a"]);
    assert.deepEqual([...neighbors(adj, "c")], []);
    assert.equal(degree(adj, "a"), 1);
    assert.equal(degree(adj, "yok"), 0);
  });
});

describe("filterNodes", () => {
  const nodes = [
    n("bir", { name: "Dağıtım notu", description: "Sunucu kurulumu", agentId: "a1" }),
    n("iki", { name: "Alışveriş", description: "dağıtım listesi", scope: "shared", agentId: null }),
    n("üç", { name: "Başka", agentId: "a2" }),
  ];
  it("arama ad+description'da, Türkçe küçük harf", () => {
    assert.deepEqual([...filterNodes(nodes, { q: "DAĞITIM", agentId: null, scope: null })], ["bir", "iki"]);
  });
  it("ajan ve scope süzgeci", () => {
    assert.deepEqual([...filterNodes(nodes, { q: "", agentId: "a2", scope: null })], ["üç"]);
    assert.deepEqual([...filterNodes(nodes, { q: "", agentId: null, scope: "shared" })], ["iki"]);
    assert.deepEqual([...filterNodes(nodes, { q: "başka", agentId: "a1", scope: null })], []);
  });
});

describe("nodeRadius", () => {
  it("bağlantıyla büyür, tavanı var", () => {
    assert.ok(nodeRadius(4) > nodeRadius(0));
    assert.equal(nodeRadius(1000), 14);
  });
});

describe("resolveLink", () => {
  const edges = [
    { from: "agents/w/acik", to: "shared/dagitim" },
    { from: "agents/w/acik", to: "missing/yok-boyle" },
    { from: "shared/dagitim", to: "shared/kurulum" },
  ];
  it("slug'ı kaynağın giden kenarından çözer (id/ad eşleşmesi değil)", () => {
    assert.equal(resolveLink(edges, "agents/w/acik", "dagitim"), "shared/dagitim");
    assert.equal(resolveLink(edges, "agents/w/acik", " yok-boyle "), "missing/yok-boyle");
    assert.equal(resolveLink(edges, "shared/dagitim", "kurulum|etiket"), "shared/kurulum");
  });
  it("başka düğümün kenarı ya da önek eşleşmesi sayılmaz", () => {
    assert.equal(resolveLink(edges, "agents/w/acik", "kurulum"), null);
    assert.equal(resolveLink(edges, "agents/w/acik", "boyle"), null);
  });
  it("kapsamlar arası nitelikli bağ hedefin tam id'sidir", () => {
    const scoped = [{ from: "private/t/agents/w/not", to: "team/t/shared/kurulum" }];
    assert.equal(resolveLink(scoped, "private/t/agents/w/not", "team/t/shared/kurulum"), "team/t/shared/kurulum");
  });
});
