// Phone agent screen: Sohbet is the default view, the terminal (PTY attach) opens only on request,
// and the system back gesture returns to the agent list.
import { test, expect, newTeam, teamAgents } from "./fixtures";

test.use({ viewport: { width: 390, height: 780 }, hasTouch: true, isMobile: true });

test("ajan ekranı sohbetle açılır, terminal istenince bağlanır, geri listeye döner", async ({ page, dexd }) => {
  const team = await newTeam(dexd, `Mobil sohbet ${test.info().workerIndex}`);
  const worker = (await teamAgents(dexd, team.id)).find((a) => a.kind === "worker")!;
  await page.addInitScript(
    ({ url, token }) => {
      localStorage.setItem("dex.hosts", JSON.stringify([{ id: "he2e01", name: "e2e", url, device: "" }]));
      localStorage.setItem("dex.webtoken.he2e01", token);
    },
    { url: dexd.url, token: dexd.token },
  );
  await page.goto("/");
  await page.locator(".m-team-btn").click();
  await page.getByRole("listitem").getByRole("button", { name: team.name, exact: true }).click();
  await page.getByRole("tab", { name: "Ajanlar" }).click();
  await page.getByRole("button", { name: new RegExp(worker.name) }).click();

  const screen = page.locator(".m-agent-screen");
  await expect(screen.locator(".m-chat-slot")).toBeVisible();
  await expect(screen.locator(".m-agent-state")).toContainText("kapalı");
  await expect(page.locator(".m-term-host")).toHaveCount(0);

  for (const name of ["Geri", "Terminal"]) {
    const box = (await screen.getByRole("button", { name, exact: true }).boundingBox())!;
    expect(box.width).toBeGreaterThanOrEqual(44);
    expect(box.height).toBeGreaterThanOrEqual(44);
  }

  await screen.getByRole("button", { name: "Terminal", exact: true }).tap();
  await expect(page.getByRole("button", { name: "Başlat" })).toBeVisible();
  await screen.getByRole("button", { name: "Sohbet", exact: true }).tap();
  await expect(page.locator(".m-term-host")).toHaveCount(0);

  await page.goBack();
  await expect(screen).toHaveCount(0);
  await expect(page.locator(".m-agents")).toBeVisible();
});
