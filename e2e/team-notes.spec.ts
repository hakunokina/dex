// #2040: the team note page — the leader's two appends (saved to the core), the amber band while the
// running leader hasn't picked the system append up, the "Dex istemleri" overlay (read, edit, back to
// default, read-only parts) and the notes tabs' colour (red with content, white when empty).
import type { Page } from "@playwright/test";
import { isolatedTest as test, expect, newTeam, openTeam, paneOf, teamAgents } from "./fixtures";

interface LeaderPrompt { systemAppend: string; messageAppend: string; pendingRestart: boolean }
interface Part { id: string; title: string; editable: boolean; overridden: boolean }

const tabName = (page: Page, title: string) =>
  page.locator(".team-stage:not(.hidden) .dv-tab .notes-tab-name", { hasText: title });

test.afterEach(async ({ dexd }) => {
  for (const p of await dexd.call<{ id: string }[]>("pty.list").catch(() => [])) await dexd.call("pty_kill", { id: p.id });
});

test("Takım notu: ekleme alanları kaydeder, sarı kuşak, Dex istemleri overlay'i, dolu sekme kırmızı", async ({ app, dexd }) => {
  const team = await newTeam(dexd, `Notlar ${test.info().workerIndex}`);
  const leader = (await teamAgents(dexd, team.id)).find((a) => a.kind === "leader")!;
  await app.reload();
  await openTeam(app, team.name);
  const pane = await paneOf(app, leader.name);
  // The leader is up (autostart): its applied fingerprint is recorded, so a system append is pending.
  await expect(pane.locator(".xterm-rows")).toContainText("fake-claude hazir");

  await expect(tabName(app, "Ortak not")).not.toHaveClass(/filled/);
  await expect(tabName(app, "Takım notu")).not.toHaveClass(/filled/);
  await app.locator(".team-stage:not(.hidden) .dv-tab", { hasText: "Takım notu" }).click();

  const stage = app.locator(".team-stage:not(.hidden)");
  const sys = stage.getByLabel("Lider sistem istemine ekle");
  const msg = stage.getByLabel("Her mesaja ekle");
  await expect(sys).toBeVisible();
  await expect(stage.getByText("Lider oturumu yeniden başlamadan değişiklikler uygulanmaz.")).toHaveCount(0);

  // Keyboard: type into the system append, Tab to the next field, type there.
  await sys.click();
  await app.keyboard.type("Kısa yaz.");
  await app.keyboard.press("Tab");
  await expect(msg).toBeFocused();
  await app.keyboard.type("Türkçe cevap ver.");
  await app.keyboard.press("Tab"); // blur saves at once
  await expect.poll(() => dexd.call<LeaderPrompt>("leader_prompt_get", { teamId: team.id }))
    .toMatchObject({ systemAppend: "Kısa yaz.", messageAppend: "Türkçe cevap ver.", pendingRestart: true });
  await expect(stage.getByText("Lider oturumu yeniden başlamadan değişiklikler uygulanmaz.")).toBeVisible();
  await expect(stage.getByRole("button", { name: "Lider paneline git" })).toBeVisible();
  await expect(tabName(app, "Takım notu")).toHaveClass(/filled/);
  await expect(tabName(app, "Ortak not")).not.toHaveClass(/filled/);

  // Back to what the leader runs: the band goes, the tab is white again.
  await sys.fill("");
  await msg.fill("");
  await msg.blur();
  await expect.poll(() => dexd.call<LeaderPrompt>("leader_prompt_get", { teamId: team.id }))
    .toMatchObject({ systemAppend: "", messageAppend: "", pendingRestart: false });
  await expect(stage.getByText("Lider oturumu yeniden başlamadan değişiklikler uygulanmaz.")).toHaveCount(0);
  await expect(tabName(app, "Takım notu")).not.toHaveClass(/filled/);

  // A remote write (another client / the core) reaches the field and the tab.
  await dexd.call("leader_prompt_set", { teamId: team.id, messageAppend: "uzaktan" });
  await expect(msg).toHaveValue("uzaktan");
  await expect(tabName(app, "Takım notu")).toHaveClass(/filled/);
  await dexd.call("leader_prompt_set", { teamId: team.id, messageAppend: "" });
  await expect(tabName(app, "Takım notu")).not.toHaveClass(/filled/);

  // The shared note: content → red.
  await dexd.call("note_set", { scope: "shared", body: "ortak" });
  await expect(tabName(app, "Ortak not")).toHaveClass(/filled/);

  // Overlay: open by keyboard, focus inside, Esc closes and returns focus.
  const open = stage.getByRole("button", { name: "Dex istemleri" });
  await open.focus();
  await app.keyboard.press("Enter");
  const dialog = app.getByRole("dialog", { name: "Dex istemleri" });
  await expect(dialog).toBeVisible();
  const { parts, policyNote } = await dexd.call<{ parts: Part[]; policyNote: string }>("prompt_parts");
  await expect(dialog.getByText(policyNote)).toBeVisible();
  const rules = parts.find((p) => p.id === "dex-rules")!;
  const fixed = parts.find((p) => !p.editable)!;
  // Titles repeat as prefixes ("Proje yığını", "Proje yığını notu"): the list keeps the core's order.
  const item = (p: Part) => dialog.locator(".pp-item").nth(parts.indexOf(p));

  // A read-only part: no edit, no save.
  await item(fixed).click();
  await expect(dialog.getByRole("textbox")).toHaveAttribute("readonly", "");
  await expect(dialog.getByRole("button", { name: "Kaydet" })).toHaveCount(0);

  // Edit dex-rules, save → overridden; "Varsayılana dön" (confirmed) → default again.
  await item(rules).click();
  const text = dialog.getByRole("textbox");
  await text.press("Control+End");
  await text.pressSequentially("\n- e2e ek kuralı");
  await dialog.getByRole("button", { name: "Kaydet" }).click();
  await expect.poll(async () => (await dexd.call<{ parts: Part[] }>("prompt_parts")).parts.find((p) => p.id === "dex-rules")!.overridden).toBe(true);
  await expect(item(rules)).toContainText("değiştirildi");
  await dialog.getByRole("button", { name: "Varsayılana dön" }).click();
  await dialog.getByRole("button", { name: "Evet, varsayılana dön" }).click();
  await expect.poll(async () => (await dexd.call<{ parts: Part[] }>("prompt_parts")).parts.find((p) => p.id === "dex-rules")!.overridden).toBe(false);
  await expect(item(rules)).not.toContainText("değiştirildi");

  await app.keyboard.press("Escape");
  await expect(dialog).toHaveCount(0);
  await expect(open).toBeFocused();

  // Narrow window: the overlay stacks and fits without horizontal scroll.
  await app.setViewportSize({ width: 480, height: 800 });
  await open.click();
  await expect(dialog).toBeVisible();
  const overflow = await dialog.evaluate((el) => el.scrollWidth - el.clientWidth);
  expect(overflow).toBeLessThanOrEqual(0);
  const box = (await dialog.boundingBox())!;
  expect(box.x).toBeGreaterThanOrEqual(0);
  expect(box.x + box.width).toBeLessThanOrEqual(480);
  await dialog.getByRole("button", { name: "Kapat" }).click();
  await expect(dialog).toHaveCount(0);
});
