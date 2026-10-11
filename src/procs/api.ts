// #916: the process panel's two RPCs (crates/dexd/src/methods.rs). Agent ids are host-raw.
import { callHost } from "../api/hosts";
import type { ProcessMap } from "./view";

export const listAgentProcesses = (host: string) => callHost<ProcessMap>(host, "agent_processes", {});

export const killAgentProcess = (host: string, agent: string, pid: number) =>
  callHost<null>(host, "kill_agent_process", { agent, pid });
