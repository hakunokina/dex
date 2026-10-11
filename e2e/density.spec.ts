// Görev #33: Settings → Görünüm switches the UI density (default compact) at once, keeps it over a
// reload, and compact really shows more terminal rows. The leader is the fake `claude`.
import type { Locator, Page } from "@playwright/test";
import { test, expect, calls, newTeam, openTeam, paneOf, teamAgents } from "./fixtures";

/** The leader pane's terminal rows and the vertical px around them that aren't terminal. */
const measure = (pane: Locator) =>
  pane.evaluate((p) => {
    const px = (el: Element | null | undefined) => Math.round(el?.getBoundingClientRect().height ?? 0);
    // Dockview draws the body in an overlay: the pane's group is the one around it.
    const group = (el: Element) => {
      const r = el.getBoundingClientRect();
      return [...document.querySelectorAll(".team-stage:not(.hidden) .dv-groupview")].find((g) => {
        const b = g.getBoundingClientRect();
        return b.top <= r.top && b.bottom >= r.bottom - 1 && b.left <= r.left + 1 && b.right >= r.right - 1;
      });
    };
    const screen = p.querySelector(".xterm-screen")!.getBoundingClientRect();
    return {
      rows: p.querySelector(".xterm-rows")!.children.length,
      termFont: getComputedStyle(p.querySelector(".xterm-rows")!).fontSize,
      rowH: px(p.querySelector(".xterm-rows")!.firstElementChild),
      termPadTop: Math.round(screen.top - p.querySelector(".pane-body")!.getBoundingClientRect().top),
      topbar: px(document.querySelector(".topbar")),
      toggle: px(p.querySelector(".pane-view-toggle")),
      tabHeader: px(group(p)?.querySelector(".dv-tabs-and-actions-container")),
      chromeAbove: Math.round(screen.top),
      group: px(group(p)),
      groupChrome: px(group(p)) - Math.round(screen.height),
    };
  });

async function leaderPane(page: Page, teamName: string, leaderName: string) {
  await openTeam(page, teamName);
  const pane = await paneOf(page, leaderName);
  await expect(pane.locator(".xterm-rows")).toContainText("fake-claude hazir");
  return pane;
}

test("Ayarlar → Görünüm: varsayılan kompakt, anında Normal'e geçer, reload sonrası korunur; kompakt daha çok satır gösterir", async ({ app, dexd }) => {
  const team = await newTeam(dexd, `Yoğunluk ${test.info().workerIndex}`);
  const leader = (await teamAgents(dexd, team.id)).find((a) => a.kind === "leader")!;
  await app.evaluate(() => localStorage.removeItem("dex.density"));
  await app.reload();
  await expect(app.locator("html")).toHaveAttribute("data-density", "compact");
  let pane = await leaderPane(app, team.name, leader.name);
  const compact = await measure(pane);

  await app.getByRole("button", { name: "Ayarlar", exact: true }).click();
  const group = app.getByRole("radiogroup", { name: "Yoğunluk" });
  await expect(group.getByRole("radio", { name: "Kompakt" })).toBeChecked();
  // Keyboard: arrows move within the native radio group.
  await group.getByRole("radio", { name: "Kompakt" }).focus();
  await app.keyboard.press("ArrowDown");
  await expect(group.getByRole("radio", { name: "Normal" })).toBeChecked();
  await expect(app.locator("html")).toHaveAttribute("data-density", "normal");
  // Applied at once: the open terminal took the normal size without a reload.
  await expect.poll(() => pane.locator(".xterm-rows").evaluate((el) => getComputedStyle(el).fontSize)).toBe("13px");

  // Close the floating Ayarlar before the reload and wait for the debounced layout_save without it:
  // a saved-open panel comes back over the leader pane and blinds paneOf's hit-test.
  const savedSettings = async () => {
    const save = (await calls(app)).filter((c) => c.cmd === "layout_save").at(-1);
    const teams = (save?.args.layout as { teams?: Record<string, { dock: unknown }> } | undefined)?.teams;
    const dock = teams?.[team.id]?.dock as { panels?: Record<string, unknown> } | null | undefined;
    return dock?.panels ? "settings" in dock.panels : "no save";
  };
  // First the save with Ayarlar open, so the next false can only be the save after the close.
  await expect.poll(savedSettings).toBe(true);
  await app.getByRole("button", { name: "Ayarlar panelini kapat" }).click();
  await expect(group).toHaveCount(0);
  await expect.poll(savedSettings).toBe(false);

  await app.reload();
  await expect(app.locator("html")).toHaveAttribute("data-density", "normal");
  pane = await leaderPane(app, team.name, leader.name);
  const normal = await measure(pane);
  console.log("DENSITY", JSON.stringify({ normal, compact }));

  expect(normal.termFont).toBe("13px");
  expect(compact.termFont).toBe("12px");
  expect(compact.rows).toBeGreaterThan(normal.rows);
  expect(compact.chromeAbove).toBeLessThan(normal.chromeAbove);
});
