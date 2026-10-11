// scripts/deploy/'u yapılandırılan VM'e kopyalayıp install.sh'ı koşar (SSH kullanıcısı, sudo).
// Tekrar koşmak güvenli. Geri dönüş ve elle yol: docs/deploy.md.
import { execFileSync } from "node:child_process";
import { resolve } from "node:path";
import { deployConfig } from "./deploy-config.mjs";

const { host, contents } = deployConfig();
if (process.argv.includes("--dry-run")) {
  console.log("deploy:install dry-run: local config loaded; tar/ssh/install skipped");
  process.exit(0);
}
const dir = resolve(import.meta.dirname, "deploy");
// Exclude the local file from the directory archive, then send the selected config explicitly.
const tar = execFileSync("tar", ["-c", "--exclude=deploy.conf", "-C", dir, "."], { maxBuffer: 64 << 20 });
const config = Buffer.from(contents, "utf8").toString("base64");
execFileSync("ssh", [host, `d=$(mktemp -d) && tar -x -C "$d" && printf '%s' '${config}' | base64 -d >"$d/deploy.conf" && sed -i 's/\\r$//' "$d"/*.sh "$d"/post-receive && bash "$d/install.sh" "$d"; r=$?; rm -rf "$d"; exit $r`], {
  input: tar,
  stdio: ["pipe", "inherit", "inherit"],
});
