import { useEffect } from "react";
import { hostOf, isHostUp, onHostUp, subscribeHosts } from "../api/hosts";
import { teamViewed } from "./api";
import { getActiveTeamId, subscribe } from "./store";
import { createTeamViewing } from "./viewed";

export function useTeamViewing() {
  useEffect(() => {
    const viewing = createTeamViewing(() => {
      const teamId = getActiveTeamId();
      return { teamId, viewed: document.visibilityState === "visible" && document.hasFocus(), connected: isHostUp(hostOf(teamId)) };
    }, async (teamId, viewed) => {
      // A dropped socket already releases this client's viewing in the core.
      if (isHostUp(hostOf(teamId))) await teamViewed(teamId, viewed);
    }, () => { console.warn("Takım görüntülenme durumu gönderilemedi"); });
    const sync = () => viewing.sync();
    const offTeams = subscribe(sync);
    const offHosts = subscribeHosts(sync);
    const offUp = onHostUp((host) => {
      if (hostOf(getActiveTeamId()) === host) viewing.sync();
    });
    document.addEventListener("visibilitychange", sync);
    window.addEventListener("focus", sync);
    window.addEventListener("blur", sync);
    sync();
    return () => {
      offTeams(); offHosts(); offUp();
      document.removeEventListener("visibilitychange", sync);
      window.removeEventListener("focus", sync);
      window.removeEventListener("blur", sync);
      viewing.dispose();
    };
  }, []);
}
