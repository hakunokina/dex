// Palette commands (DEX-6). Built fresh on every render from the team store + live run state.
import { getStatus } from "../agents";
import { STALL_CHOICES, setStallMinutes, stallMinutes } from "../stall";
import { createTask } from "../board/api";
import { openBoard } from "../board/store";
import { applyFavoriteLayout, applyFavoriteToAll, hasFavorite, openAgent, openNotes, openPanel, openSettings, openShell, resetLayout, revealAgent, saveFavoriteLayout } from "../layout/panels";
import { requestStart, startAgent } from "../launcher";
import { killPty } from "../pty";
import { getTeamsSnapshot, selectTeam, setWorkersModel } from "../team/store";
import { openTeamMenu } from "../team/TeamBar";
import type { AgentRec } from "../team/api";
import { leaveTerminal } from "../termKeys";
import type { Group, Rankable } from "./rank";

export interface Command extends Rankable {
  run: () => void | Promise<void>;
  /** Give the keyboard back to where it was before the palette (the command doesn't take it). */
  restoreFocus?: boolean;
  /** One-off (quick task): kept out of the recents. */
  transient?: boolean;
}

/** App-owned view state the palette drives (passed in at the mount). */
export interface Bridge {
  onFocusAgent: (agentId: string) => void;
}

export function buildCommands(bridge: Bridge): Command[] {
  const { teams, agents, activeId } = getTeamsSnapshot();
  const teamName = (id: string) => teams.find((t) => t.id === id)?.name ?? "";

  const focus = async (a: AgentRec) => {
    selectTeam(a.teamId);
    bridge.onFocusAgent(a.id);
    await revealAgent(a.teamId, a.id);
  };

  // Everything not a team jump or an agent row is an action.
  const cmds: Command[] = ([
    { id: "board", title: "Görev panosunu aç", hint: "Görevler", keywords: "board task pano", run: openBoard, restoreFocus: true },
    // #1076: the docked process panel (also the Kart toolbar's CPU button).
    { id: "procs", title: "Süreç panelini aç", hint: "Süreçler · CPU/RAM", keywords: "süreç process cpu ram bellek playwright tarayıcı chrome kill kapat yavaş", run: () => void openPanel(activeId, "procs") },
    // FIX-1: Tab belongs to the terminal; this is where the patron learns the way out.
    {
      id: "term.leave",
      title: "Terminalden çık (sonraki kontrole geç)",
      hint: "Ctrl+Tab · geri Ctrl+Shift+Tab",
      keywords: "odak focus tab klavye çıkış terminal",
      run: () => void leaveTerminal(),
    },
    // DEX-36: the same board as a dockable panel of the active team.
    {
      id: "board.panel",
      title: "Görev panosunu panelde aç",
      hint: `Düzen · ${teamName(activeId)}`,
      keywords: "board task pano panel yüzen",
      run: () => void openPanel(activeId, "board"),
      restoreFocus: true,
    },
    { id: "office.panel", title: "Ajan kartını aç", hint: `Düzen · ${teamName(activeId)}`, keywords: "kart pcb office panel", run: () => void openPanel(activeId, "office"), restoreFocus: true },
    // #46/#2086: the notes are one free panel; closed, it comes back here.
    { id: "notes.open", title: "Notları aç", hint: `Düzen · ${teamName(activeId)}`, keywords: "not notlar notes ortak takım", run: () => void openNotes(activeId) },
    // #47: the Kart's Ayarlar button, for when the Kart is closed.
    { id: "settings.open", title: "Ayarlar", hint: "Ayarlar", keywords: "settings tercihler görünüm genel", run: () => void openPanel(activeId, "settings") },
    // DEX-31: app settings as a floating panel; the limits live under Sunucu › Kaynak sınırları.
    {
      id: "settings.panel",
      title: "Ayarlar → Sunucu: kaynak sınırları",
      hint: "Ayarlar",
      keywords: "settings cpu bellek memory tavan limit sınır ajan kaynak",
      run: () => void openSettings(activeId, "resources"),
    },
    // Ayar sayfaları: paletin bulanık araması ayrı ayar aramasının işini görür.
    ...(
      [
        ["general", "Ayar: Bu cihaz · Genel", "görünüm yoğunluk pano düzeni ekran alıntısı kısayol"],
        ["notifications", "Ayar: Bu cihaz · Bildirimler", "ses takılma uyarı watchdog stall"],
        ["connections", "Ayar: Bu cihaz · Bağlantılar", "sunucu telefondan erişim"],
        ["providers", "Ayar: Sunucu · Sağlayıcılar", "sağlayıcı provider anahtar jeton token glm muse yerel llm"],
        ["chain", "Ayar: Sunucu · Kota zinciri", "kota zincir motor failover"],
        ["resources", "Ayar: Sunucu · Kaynak sınırları", "cpu bellek memory tavan limit sınır"],
        ["gpu", "Ayar: Sunucu · GPU", "gpu ekran kartı broker"],
      ] as const
    ).map(([tab, title, keywords]) => ({
      id: `settings.page:${tab}`,
      title,
      hint: "Ayarlar",
      keywords: `settings ayar ${keywords}`,
      run: () => void openSettings(activeId, tab),
      restoreFocus: true,
      group: "Eylemler" as Group,
    })),
    // Sık aranan satırlar: ilgili sayfayı açıp satırı odaklar.
    ...(
      [
        ["notifications", "stall", "Ayar: Takılma uyarısı", "takıldı sessiz süre watchdog stall"],
        ["general", "snip-shortcut", "Ayar: Ekran alıntısı kısayolu", "kısayol shortcut ekran alıntı snip"],
        ["providers", "provider-glm", "Ayar: Sağlayıcı anahtarı", "sağlayıcı anahtar jeton token glm"],
        ["chain", undefined, "Ayar: Kota zinciri", "kota zincir motor failover"],
      ] as const
    ).map(([tab, setting, title, keywords]) => ({
      id: `settings.row:${setting ?? tab}`,
      title,
      hint: "Ayarlar",
      keywords: `settings ayar ${keywords}`,
      run: () => void openSettings(activeId, tab, setting),
      restoreFocus: true,
      group: "Eylemler" as Group,
    })),
    {
      id: "layout.reset",
      title: "Düzeni sıfırla",
      hint: `Düzen · ${teamName(activeId)}`,
      keywords: "layout reset varsayılan pane panel yerleşim",
      run: () => resetLayout(activeId),
      restoreFocus: true,
    },
    {
      id: "layout.favorite.save",
      title: "Bu düzeni favori yap",
      hint: `Düzen · ${teamName(activeId)}${hasFavorite() ? " · mevcut favorinin yerine" : ""}`,
      keywords: "favori patron düzen kaydet varsayılan şablon",
      run: () => {
        if (!saveFavoriteLayout(activeId)) throw new Error("Panel henüz hazır değil — takım açılınca tekrar deneyin");
      },
      restoreFocus: true,
    },
    { id: "team.new", title: "Yeni takım", hint: "Takım", keywords: "ekip oluştur team", run: () => openTeamMenu("new") },
    ...(hasFavorite()
      ? [
          {
            id: "layout.favorite.apply",
            title: "Favori düzeni bu takıma uygula",
            hint: `Düzen · ${teamName(activeId)}`,
            keywords: "favori patron düzen uygula şablon",
            run: () => {
              if (!applyFavoriteLayout(activeId)) throw new Error("Favori uygulanamadı");
            },
            restoreFocus: true,
          },
          {
            id: "layout.favorite.applyAll",
            title: "Favori düzeni tüm takımlara uygula",
            hint: "Düzen · tüm takımlar",
            keywords: "favori patron düzen uygula hepsi toplu şablon",
            run: () => {
              const n = applyFavoriteToAll(teams.map((t) => t.id));
              if (!n) throw new Error("Favori uygulanamadı");
            },
            restoreFocus: true,
          },
        ]
      : []),
    {
      id: "team.member",
      title: "Üye ekle",
      hint: `Takım · ${teamName(activeId)}`,
      keywords: "ajan agent worker lider",
      run: async () => {
        await openTeamMenu("team");
        document.querySelector<HTMLInputElement>(".team-add input")?.focus();
      },
    },
    {
      // DEX-29: the patron's shell, as a new panel of the active team (workdir = the team's).
      id: "terminal.open",
      title: "Terminal aç",
      hint: `PowerShell · ${teamName(activeId)}`,
      keywords: "powershell pwsh shell kabuk konsol cmd",
      run: () => {
        void openShell(activeId).then(id => { if (id) bridge.onFocusAgent(id); });
      },
    },
    { id: "team.panel", title: "Takım paneli", hint: `Takım · ${teamName(activeId)}`, keywords: "takım ayarları üyeler settings", run: () => openTeamMenu("team") },
    // Hafıza haritası (Kademe 1, salt okuma): Kart araç çubuğundaki düğmeyle aynı panel.
    { id: "memory.open", title: "Hafıza haritasını aç", hint: `Düzen · ${teamName(activeId)}`, keywords: "hafıza memory harita graph obsidyen", run: () => void openPanel(activeId, "memory"), restoreFocus: true },
    // DEX-30: bulk model switch for the active team's workers (leaders stay); running ones wait.
    {
      id: "workers.claude",
      title: "Worker'ları Anthropic'e (Sonnet 5.5) al",
      hint: `Takım · ${teamName(activeId)} · Sonnet 5.5`,
      keywords: "claude anthropic sonnet model toplu hepsi",
      restoreFocus: true,
      run: async () => void (await setWorkersModel(activeId, { engine: "claude", provider: "anthropic", model: "claude-sonnet-5-5" })),
    },
    {
      id: "workers.model",
      title: "Worker modellerini değiştir…",
      hint: `Takım · ${teamName(activeId)}`,
      keywords: "motor sağlayıcı provider glm muse yerel model toplu hepsi",
      run: async () => {
        await openTeamMenu("team");
        document.querySelector<HTMLSelectElement>(".team-bulk select")?.focus();
      },
    },
  ] as Omit<Command, "group">[]).map((c) => ({ ...c, group: (c.id === "team.new" ? "Takımlar" : "Eylemler") as Group }));

  // DEX-28: stall watchdog threshold (per machine, localStorage).
  const stallLabel = (m: number) => (m ? `${m} dk` : "kapalı");
  for (const m of STALL_CHOICES.filter((m) => m !== stallMinutes())) {
    cmds.push({
      id: `stall:${m}`,
      title: `Takılma bekçisi: ${stallLabel(m)}`,
      hint: `Ayar · şu an ${stallLabel(stallMinutes())}`,
      keywords: "takıldı sessiz uyarı süre watchdog stall ayar",
      run: () => setStallMinutes(m),
      restoreFocus: true,
      group: "Eylemler",
    });
  }

  for (const t of teams) {
    if (t.id === activeId) continue;
    const members = agents.filter((a) => a.teamId === t.id);
    const busy = members.filter((a) => getStatus(a.id) === "working").length;
    cmds.push({
      id: `team:${t.id}`,
      title: `Takıma geç: ${t.name}`,
      hint: `${members.length ? `${members.length} ajan` : "boş"}${busy ? ` · ${busy} çalışıyor` : ""}`,
      keywords: "switch takım",
      run: () => selectTeam(t.id),
      group: "Takımlar",
    });
  }

  // Active team's agents first.
  const ordered = [...agents].sort((a, b) => Number(b.teamId === activeId) - Number(a.teamId === activeId));
  for (const a of ordered) {
    const where = teams.length > 1 ? ` · ${teamName(a.teamId)}` : "";
    const kind = a.kind === "leader" ? "lider" : "worker";
    cmds.push({ id: `focus:${a.id}`, title: `Odaklan: ${a.name}`, hint: `${kind}${where}`, keywords: "pane terminal", run: () => focus(a), group: "Ajanlar" });
    cmds.push({
      id: `open:${a.id}`,
      title: `Panelde aç: ${a.name}`,
      hint: `${kind}${where}`,
      keywords: "pane panel dock",
      run: () => {
        selectTeam(a.teamId);
        openAgent(a.teamId, a.id);
      },
      restoreFocus: a.teamId === activeId,
      group: "Ajanlar",
    });
    if (getStatus(a.id) === "offline") {
      cmds.push({
        id: `start:${a.id}`,
        title: `Başlat: ${a.name}`,
        hint: `${kind}${where}`,
        keywords: "claude çalıştır start",
        run: async () => {
          if (!requestStart(a.id)) await startAgent(a.id);
        },
        restoreFocus: a.teamId !== activeId,
        group: "Ajanlar",
      });
    } else {
      cmds.push({
        id: `stop:${a.id}`,
        title: `Durdur: ${a.name}`,
        hint: `${kind}${where}`,
        keywords: "kapat stop kill",
        run: () => killPty(a.id) as Promise<void>,
        restoreFocus: true,
        group: "Ajanlar",
      });
    }
  }
  return cmds;
}

const TASK_RE = /^\s*yeni\s+g[öo]rev\s*:\s*(.*)$/i;

/** "yeni görev: <metin>" → a one-off command adding the task to the team on screen. */
export function quickTask(query: string): Command | null {
  const m = TASK_RE.exec(query);
  if (!m) return null;
  const title = m[1].trim();
  return {
    id: "task:new",
    title: title ? `Yeni görev: ${title}` : "Yeni görev: …",
    hint: "Enter ile ekle",
    group: "Eylemler",
    transient: true,
    restoreFocus: true,
    run: async () => {
      if (!title) throw new Error("Görev metni boş");
      await createTask({ title });
      openBoard();
    },
  };
}
