// #58: the phone terminal (src/mobile/MobileTerminal.tsx) in Chromium's mobile emulation. No Tauri
// bridge here: a plain browser is the WEB/mobile shell, paired to the worker's dexd through
// localStorage (src/api/hosts.ts webLocal). The agent is the fake `claude` (e2e/fake-claude.rs).
import type { Page } from "@playwright/test";
import { test, expect, newTeam, teamAgents, type Dexd } from "./fixtures";

test.use({ viewport: { width: 390, height: 780 }, hasTouch: true, isMobile: true });

const HOST = "he2e01";
const screen = (dexd: Dexd, agentId: string) => dexd.call<string>("chat_screen", { agentId });

test.afterEach(async ({ dexd }) => {
  for (const p of await dexd.call<{ id: string }[]>("pty.list").catch(() => [])) await dexd.call("pty_kill", { id: p.id });
});

async function openAgent(page: Page, dexd: Dexd, teamName: string) {
  const team = await newTeam(dexd, teamName);
  const worker = (await teamAgents(dexd, team.id)).find((a) => a.kind === "worker")!;
  await page.addInitScript(
    ({ host, url, token }) => {
      localStorage.setItem("dex.hosts", JSON.stringify([{ id: host, name: "e2e", url, device: "" }]));
      localStorage.setItem(`dex.webtoken.${host}`, token);
    },
    { host: HOST, url: dexd.url, token: dexd.token },
  );
  await page.goto("/");
  await page.locator(".m-team-btn").click();
  await page.getByRole("listitem").getByRole("button", { name: team.name, exact: true }).click();
  await page.getByRole("tab", { name: "Ajanlar" }).click();
  await page.getByRole("button", { name: new RegExp(worker.name) }).click();
  // Sohbet is the default view; the terminal (and its PTY attach) only opens on request.
  await page.getByRole("button", { name: "Terminal", exact: true }).tap();
  await page.getByRole("button", { name: "Başlat" }).tap();
  await expect(page.locator(".xterm-rows")).toContainText("fake-claude hazir");
  return worker;
}

test("dokunmatik sürükleme scrollback'te kaydırır, En alta geri getirir", async ({ page, dexd }) => {
  const worker = await openAgent(page, dexd, `Mobil kaydır ${test.info().workerIndex}`);
  for (let i = 0; i < 80; i++) await dexd.call("pty_write", { id: worker.id, data: `satir${i}\r` });
  await expect.poll(() => screen(dexd, worker.id)).toContain("RX:satir79");
  await expect(page.locator(".xterm-rows")).toContainText("RX:satir79");
  await expect(page.getByRole("button", { name: "En alta" })).toHaveCount(0);

  // A finger dragged down over the terminal: older lines come into view.
  const box = (await page.locator(".m-term-host").boundingBox())!;
  const x = box.x + box.width / 2;
  const cdp = await page.context().newCDPSession(page);
  const touch = (type: string, y: number) =>
    cdp.send("Input.dispatchTouchEvent", { type, touchPoints: type === "touchEnd" ? [] : [{ x, y }] });
  await touch("touchStart", box.y + 40);
  for (let y = box.y + 60; y <= box.y + box.height - 20; y += 20) await touch("touchMove", y);
  await touch("touchEnd", 0);
  await cdp.detach();

  await expect(page.locator(".xterm-rows")).not.toContainText("RX:satir79");
  const bottom = page.getByRole("button", { name: "En alta" });
  await expect(bottom).toBeVisible();
  await bottom.tap();
  await expect(page.locator(".xterm-rows")).toContainText("RX:satir79");
  await expect(bottom).toHaveCount(0);
});

test("giriş satırı: yazıp bir harf silip devam edince PTY'ye doğru metin gider", async ({ page, dexd }) => {
  const worker = await openAgent(page, dexd, `Mobil yaz ${test.info().workerIndex}`);
  const line = page.getByRole("textbox", { name: "Terminale yaz" });
  await line.tap();
  await page.keyboard.type("şu an mobildeyik");
  await page.keyboard.press("Backspace");
  // The rest as an IME would send it: a composed word, reshaped once, then committed.
  const cdp = await page.context().newCDPSession(page);
  await cdp.send("Input.imeSetComposition", { text: "m", selectionStart: 1, selectionEnd: 1 });
  await cdp.send("Input.imeSetComposition", { text: "", selectionStart: 0, selectionEnd: 0 });
  await cdp.send("Input.imeSetComposition", { text: "m", selectionStart: 1, selectionEnd: 1 });
  await cdp.send("Input.insertText", { text: "m, tamam" });
  await cdp.detach();
  await expect(line).toHaveValue("şu an mobildeyim, tamam");
  // Nothing reached the agent while typing in the line.
  expect(await screen(dexd, worker.id)).not.toContain("RX:ş");

  await page.getByRole("button", { name: "Gönder" }).tap();
  await expect.poll(() => screen(dexd, worker.id)).toContain("RX:şu an mobildeyim, tamam");
  await expect(line).toHaveValue("");

  // The soft keyboard never opens on xterm's own textarea.
  await expect(page.locator(".xterm-helper-textarea")).toHaveAttribute("readonly", "");
});
