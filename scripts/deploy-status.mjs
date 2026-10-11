// Yapılandırılan VM'deki son otomatik dexd deploy'u — status.json + birimlerin durumu.
// `--log`: son deploy log'unun kuyruğu.
import { execFileSync } from "node:child_process";
import { deployConfig } from "./deploy-config.mjs";

const { host, home, service } = deployConfig();
if (process.argv.includes("--dry-run")) {
  console.log("deploy:status dry-run: local config loaded; ssh/status/log skipped");
  process.exit(0);
}
const out = execFileSync("ssh", [host, "bash -s"], {
  input: `cat '${home}/status.json' 2>/dev/null || echo '{}'
for u in ${service} dexd-deploy.path dexd-deploy.service; do echo "unit $u $(systemctl is-active $u)"; done`,
  encoding: "utf8",
});
const [json, ...units] = out.trim().split("\n");
const s = JSON.parse(json);
if (!s.sha) console.log("henüz otomatik deploy yok");
else console.log(`son deploy: ${s.sha} → ${s.result} (${s.state}, ${s.time})${s.prev ? `, önceki ${s.prev}` : ""}\nlog: ${s.log}`);
for (const u of units) console.log(u.replace(/^unit /, "  "));
if (process.argv.includes("--log") && s.log) {
  if (typeof s.log !== "string" || !s.log.startsWith(`${home}/logs/`) || !/^[a-zA-Z0-9_./-]+$/.test(s.log) || s.log.includes("..")) {
    throw new Error("Invalid deploy log path");
  }
  console.log("---");
  execFileSync("ssh", [host, `tail -n 40 '${s.log}'`], { stdio: "inherit" });
}
