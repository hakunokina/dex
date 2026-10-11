import { readFileSync } from "node:fs";
import { resolve } from "node:path";

export function deployConfig() {
  const file = process.env.DEX_DEPLOY_CONF || resolve(import.meta.dirname, "deploy/deploy.conf");
  const config = {};
  const keys = ["DEX_DEPLOY_HOST", "DEPLOY_USER", "DEPLOY_GROUP", "USER_HOME", "DEPLOY_HOME", "REPO", "SRC", "BIN", "DATA", "SERVICE", "PORT", "HEALTH_TIMEOUT", "KEEP_LOGS"];
  for (const line of readFileSync(file, "utf8").split(/\r?\n/)) {
    if (!line.trim() || line.startsWith("#")) continue;
    const match = /^([A-Z_]+)=([a-zA-Z0-9_./:@+-]*)$/.exec(line);
    if (!match || (!keys.includes(match[1]) && match[1] !== "DEX_APK_CERT") || (!match[2] && match[1] !== "DEX_APK_CERT")) throw new Error("deploy.conf: expected supported KEY=value");
    config[match[1]] = process.env[match[1]] || match[2];
  }
  const host = process.env.DEX_DEPLOY_HOST || config.DEX_DEPLOY_HOST;
  if (!host || host.startsWith("-") || !/^[a-zA-Z0-9_.@:-]+$/.test(host)) throw new Error("Invalid DEX_DEPLOY_HOST");
  for (const key of ["USER_HOME", "DEPLOY_HOME", "REPO", "SRC", "BIN", "DATA"]) {
    if (!/^\/[a-zA-Z0-9_./-]+$/.test(config[key] || "") || config[key].includes("..")) throw new Error(`Invalid ${key}`);
  }
  for (const key of ["DEPLOY_USER", "DEPLOY_GROUP"]) {
    if (!/^[a-z_][a-z0-9_-]*$/.test(config[key] || "")) throw new Error(`Invalid ${key}`);
  }
  if (!/^[a-zA-Z0-9_-]+\.service$/.test(config.SERVICE || "")) throw new Error("Invalid SERVICE");
  for (const key of ["PORT", "HEALTH_TIMEOUT", "KEEP_LOGS"]) {
    if (!/^[0-9]+$/.test(config[key] || "") || Number(config[key]) < 1) throw new Error(`Invalid ${key}`);
  }
  if (Number(config.PORT) > 65535) throw new Error("Invalid PORT");
  const apkCert = process.env.DEX_APK_CERT || config.DEX_APK_CERT;
  if (apkCert && !/^[a-fA-F0-9]{64}$/.test(apkCert)) throw new Error("Invalid DEX_APK_CERT: expected SHA-256 (64 hex characters)");
  config.DEX_DEPLOY_HOST = host;
  const contents = keys.map((key) => `${key}=${config[key]}\n`).join("");
  return { host, home: config.DEPLOY_HOME, service: config.SERVICE, file, contents, apkCert: apkCert?.toLowerCase() };
}
