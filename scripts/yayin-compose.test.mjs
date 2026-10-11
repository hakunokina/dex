import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { mkdirSync, mkdtempSync, readFileSync, rmSync, symlinkSync, writeFileSync } from "node:fs";
import { resolve } from "node:path";
import { test } from "node:test";

const root = resolve(import.meta.dirname, "..");
const scratch = resolve(root, ".dex/tmp");
mkdirSync(scratch, { recursive: true });
const bash = process.platform === "win32" ? "C:/Program Files/Git/bin/bash.exe" : "bash";
const shellPath = (path) => process.platform === "win32"
  ? path.replaceAll("\\", "/").replace(/^([A-Za-z]):/, (_, drive) => `/${drive.toLowerCase()}`)
  : path;
const quote = (value) => `'${value.replaceAll("'", "'\\''")}'`;

function fixture(t) {
  const dir = mkdtempSync(resolve(scratch, "yayin-compose-"));
  t.after(() => rmSync(dir, { recursive: true, force: true }));
  mkdirSync(resolve(dir, "releases/revision"), { recursive: true });
  writeFileSync(resolve(dir, "releases/revision/compose.yaml"), "services: {}\n");
  return dir;
}

function prepare(dir, cfg, rollback = false, envFile = "/unused", cwd = root) {
  writeFileSync(resolve(dir, "config.json"), JSON.stringify(cfg));
  const source = readFileSync(resolve(root, "scripts/yayin/yayin-deploy"), "utf8");
  const functions = source.slice(source.indexOf("compose_file()"), source.indexOf("\nexec 9>"))
    .replaceAll("/usr/local/lib/yayin/compose-validate.py", shellPath(resolve(root, "scripts/yayin/compose-validate.py")));
  const script = `${functions}
A=${quote(shellPath(dir))}
P=fixture PORT=8101 ENVF=${quote(shellPath(envFile))} CPUS=2 MEM=1g
say() { echo "$*"; }
# Only Compose config is stubbed; build/up cannot run in this fixture.
docker() { [[ " $* " == *" config --services --no-env-resolution "* ]] && echo web; }
dc() { [[ "$2 $3 $4 $5" == "config --format json --no-env-resolution" ]] || { echo unexpected-daemon-call; return 99; }; cat "$A/config.json"; }
python3() { ${process.platform === "win32" ? "python" : "command python3"} "$@"; }
${rollback ? `prev=revision
status() { echo "status:$1"; }
health() { return 0; }
${source.slice(source.indexOf('say "GERİ ALMA:'))}` : "prepare revision unused"}
`;
  writeFileSync(resolve(dir, "prepare.sh"), script);
  return spawnSync(bash, [shellPath(resolve(dir, "prepare.sh"))], {
    encoding: "utf8", cwd,
  });
}

function config(dir, fields = {}, volumes = {}, networks = {}, resources = {}) {
  const result = spawnSync("docker", ["compose", "-p", "yayinfixture", "--project-directory", dir,
    "-f", "-", "config", "--format", "json", "--no-env-resolution"], {
    input: JSON.stringify({ services: { web: {
      image: "nginx:alpine", ports: ["127.0.0.1:8101:80"], ...fields,
    } }, volumes, networks, ...resources }), encoding: "utf8",
  });
  assert.equal(result.status, 0, result.stderr);
  return JSON.parse(result.stdout);
}

for (const hook of ["post_start", "pre_stop"]) {
  test(`deploy prepare rejects privileged ${hook} hook`, (t) => {
    const dir = fixture(t);
    const result = prepare(dir, config(dir, { [hook]: [{ command: "id", privileged: true }] }));
    assert.equal(result.status, 1, result.stdout + result.stderr);
    assert.match(result.stdout + result.stderr, new RegExp(`${hook} privileged`));
  });
  test(`unprivileged ${hook} hook passes the resolved Compose gate`, (t) => {
    const dir = fixture(t);
    const result = prepare(dir, config(dir, { [hook]: [{ command: "id", privileged: false }] }));
    assert.equal(result.status, 0, result.stdout + result.stderr);
  });
}

for (const kind of ["secrets", "configs"]) {
  for (const [name, source] of [
    ["docker socket", () => "/var/run/docker.sock"],
    ["outside file", (dir) => resolve(dir, "../outside.txt")],
    ["project socket", (dir) => resolve(dir, "docker.sock")],
  ]) {
    test(`deploy prepare rejects ${kind} ${name}`, (t) => {
      const dir = fixture(t);
      const result = prepare(dir, config(dir, { [kind]: ["resource"] }, {}, {}, {
        [kind]: { resource: { file: source(dir) } },
      }));
      assert.equal(result.status, 1, result.stdout + result.stderr);
      assert.match(result.stdout + result.stderr, new RegExp(kind));
    });
  }
  test(`project file ${kind} pass the resolved Compose gate`, (t) => {
    const dir = fixture(t);
    writeFileSync(resolve(dir, "resource.txt"), "fixture");
    const result = prepare(dir, config(dir, { [kind]: [{ source: "resource", target: "renamed" }] }, {}, {}, {
      [kind]: { resource: { file: resolve(dir, "resource.txt") } },
    }));
    assert.equal(result.status, 0, result.stdout + result.stderr);
  });
  test(`deploy prepare rejects ${kind} symlink escape`, (t) => {
    const dir = fixture(t);
    const outside = fixture(t);
    symlinkSync(outside, resolve(dir, "escape"), process.platform === "win32" ? "junction" : "dir");
    const result = prepare(dir, config(dir, { [kind]: ["resource"] }, {}, {}, {
      [kind]: { resource: { file: resolve(dir, "escape/resource.txt") } },
    }));
    assert.equal(result.status, 1, result.stdout + result.stderr);
    assert.match(result.stdout + result.stderr, new RegExp(kind));
  });
}

test("deploy prepare rejects a host root bind before build/up", (t) => {
  const dir = fixture(t);
  const result = prepare(dir, config(dir, { volumes: [{ type: "bind", source: "/", target: "/host" }] }));
  assert.equal(result.status, 1, result.stdout + result.stderr);
  assert.match(result.stdout + result.stderr, /bind/);
});

for (const [name, fields, rule] of [
  ["docker socket", { volumes: [{ type: "bind", source: "/var/run/docker.sock", target: "/daemon" }] }, /bind/],
  ["podman socket", { volumes: [{ type: "bind", source: "/run/podman/podman.sock", target: "/daemon" }] }, /bind/],
  ["privileged", { privileged: true }, /privileged/],
  ["host network", { network_mode: "host", ports: [] }, /network_mode host/],
  ["host PID", { pid: "host" }, /pid host/],
  ["host IPC", { ipc: "host" }, /ipc host/],
  ["capability", { cap_add: ["SYS_ADMIN"] }, /cap_add/],
  ["device", { devices: ["/dev/kvm:/dev/kvm"] }, /devices/],
  ["device rules", { device_cgroup_rules: ["a *:* rwm"] }, /device_cgroup_rules/],
  ["GPU devices", { gpus: "all" }, /gpus/],
  ["reserved devices", { deploy: { resources: { reservations: { devices: [{ capabilities: ["gpu"] }] } } } }, /reserved devices/],
  ["host provider", { provider: { type: "fixture-provider" } }, /provider/],
  ["unconfined seccomp", { security_opt: ["seccomp=unconfined"] }, /security_opt/],
  ["unconfined apparmor", { security_opt: ["apparmor:unconfined"] }, /security_opt/],
  ["disabled labels", { security_opt: ["label:disable"] }, /security_opt/],
  ["disabled no-new-privileges", { security_opt: ["no-new-privileges:false"] }, /security_opt/],
  ["public port", { ports: ["0.0.0.0:8101:80"] }, /127.0.0.1/],
  ["missing project port", { ports: ["127.0.0.1:8102:80"] }, /8101/],
  ["volumes_from", { volumes_from: ["container:daemon"] }, /volumes_from/],
  ["host user namespace", { userns_mode: "host" }, /userns_mode/],
  ["host cgroup namespace", { cgroup: "host" }, /cgroup host/],
  ["host UTS namespace", { uts: "host" }, /uts host/],
  ["custom cgroup parent", { cgroup_parent: "host-group" }, /cgroup_parent/],
  ["external container network", { network_mode: "container:daemon" }, /network_mode container/],
  ["external container PID", { pid: "container:daemon" }, /pid container/],
  ["external container IPC", { ipc: "container:daemon" }, /ipc container/],
  ["daemon API socket injection", { use_api_socket: true }, /use_api_socket/],
  ["privileged build", { build: { context: ".", privileged: true } }, /build host/],
  ["build entitlement", { build: { context: ".", entitlements: ["security.insecure"] } }, /build host/],
]) {
  test(`deploy prepare rejects ${name}`, (t) => {
    const dir = fixture(t);
    const result = prepare(dir, config(dir, fields));
    assert.equal(result.status, 1, result.stdout + result.stderr);
    assert.match(result.stdout + result.stderr, rule);
  });
}

test("project bind, named volume and loopback port pass the resolved Compose gate", (t) => {
  const dir = fixture(t);
  const result = prepare(dir, config(dir, {
    volumes: [
      { type: "bind", source: resolve(dir, "data/new"), target: "/data" },
      { type: "volume", source: "database", target: "/database" },
    ], security_opt: ["no-new-privileges:true"],
  }, { database: {} }));
  assert.equal(result.status, 0, result.stdout + result.stderr);
});

for (const [name, source] of [
  ["sibling prefix", (dir) => `${dir}-other/data`],
  ["parent traversal", (dir) => resolve(dir, "../outside")],
  ["project socket", (dir) => resolve(dir, "docker.sock")],
]) {
  test(`deploy prepare rejects ${name} bind`, (t) => {
    const dir = fixture(t);
    const result = prepare(dir, config(dir, { volumes: [{ type: "bind", source: source(dir), target: "/data" }] }));
    assert.equal(result.status, 1, result.stdout + result.stderr);
    assert.match(result.stdout + result.stderr, /bind/);
  });
}

test("symlink escape through an existing ancestor of a missing bind is rejected", (t) => {
  const dir = fixture(t);
  const outside = fixture(t);
  symlinkSync(outside, resolve(dir, "escape"), process.platform === "win32" ? "junction" : "dir");
  const result = prepare(dir, config(dir, { volumes: [{ type: "bind", source: resolve(dir, "escape/new/data"), target: "/data" }] }));
  assert.equal(result.status, 1, result.stdout + result.stderr);
  assert.match(result.stdout + result.stderr, /bind/);
});

test("symlink that resolves within the project passes", (t) => {
  const dir = fixture(t);
  mkdirSync(resolve(dir, "data"));
  symlinkSync(resolve(dir, "data"), resolve(dir, "alias"), process.platform === "win32" ? "junction" : "dir");
  const result = prepare(dir, config(dir, { volumes: [{ type: "bind", source: resolve(dir, "alias/new"), target: "/data" }] }));
  assert.equal(result.status, 0, result.stdout + result.stderr);
});

test("malformed config fails closed", (t) => {
  const result = prepare(fixture(t), { services: {} });
  assert.equal(result.status, 1, result.stdout + result.stderr);
  assert.match(result.stdout + result.stderr, /doğrulanamadı/);
});

test("preparation is repeatable and preserves the deployment overrides", (t) => {
  const dir = fixture(t);
  const cfg = config(dir);
  assert.equal(prepare(dir, cfg).status, 0);
  const override = readFileSync(resolve(dir, "releases/revision/.yayin.override.yaml"), "utf8");
  assert.equal(prepare(dir, cfg).status, 0);
  assert.equal(readFileSync(resolve(dir, "releases/revision/.yayin.override.yaml"), "utf8"), override);
  assert.match(override, /restart: unless-stopped/);
  assert.match(override, /env_file: \["\/unused"\]/);
  assert.match(override, /limits: \{cpus: "2", memory: "1g"\}/);
});

test("named volume driver_opts cannot bypass the host bind restriction", (t) => {
  const dir = fixture(t);
  const result = prepare(dir, config(dir, {
    volumes: [{ type: "volume", source: "hostroot", target: "/host" }],
  }, { hostroot: { driver: "local", driver_opts: { type: "none", device: "/", o: "bind" } } }));
  assert.equal(result.status, 1, result.stdout + result.stderr);
  assert.match(result.stdout + result.stderr, /driver_opts/);
});

test("automatic rollback rejects unsafe previous Compose before up/build", (t) => {
  const dir = fixture(t);
  const result = prepare(dir, config(dir, { privileged: true }), true);
  assert.equal(result.status, 2, result.stdout + result.stderr);
  assert.match(result.stdout + result.stderr, /privileged/);
  assert.match(result.stdout, /status:rollback_failed/);
  assert.doesNotMatch(result.stdout, /unexpected-daemon-call/);
});

test("external host network cannot bypass network_mode restriction", (t) => {
  const dir = fixture(t);
  const result = prepare(dir, config(dir, {}, {}, { default: { external: true, name: "host" } }));
  assert.equal(result.status, 1, result.stdout + result.stderr);
  assert.match(result.stdout + result.stderr, /network.*host/);
});

test("build host network is rejected", (t) => {
  const dir = fixture(t);
  const result = prepare(dir, config(dir, { build: { context: ".", network: "host" } }));
  assert.equal(result.status, 1, result.stdout + result.stderr);
  assert.match(result.stdout + result.stderr, /build host/);
});

for (const [name, build, rule] of [
  ["outside context", { context: ".." }, /build.context/],
  ["outside additional context", { context: ".", additional_contexts: { extra: ".." } }, /build.additional_contexts/],
  ["outside Dockerfile", { context: ".", dockerfile: "../Dockerfile" }, /build.dockerfile/],
  ["SSH agent", { context: ".", ssh: ["default"] }, /build.ssh/],
]) {
  test(`deploy prepare rejects build ${name}`, (t) => {
    const dir = fixture(t);
    const result = prepare(dir, config(dir, { build }));
    assert.equal(result.status, 1, result.stdout + result.stderr);
    assert.match(result.stdout + result.stderr, rule);
  });
}

test("deploy prepare rejects build SSH key in resolved JSON", (t) => {
  const dir = fixture(t);
  const cfg = config(dir, { build: { context: "." } });
  // Compose 5.1.4 cannot marshal a Windows SSH key path to JSON.
  cfg.services.web.build.ssh = ["key=/fixture/key.pem"];
  const result = prepare(dir, cfg);
  assert.equal(result.status, 1, result.stdout + result.stderr);
  assert.match(result.stdout + result.stderr, /build.ssh/);
});

for (const source of ["/var/run/docker.sock", "../build-secret.txt"]) {
  test(`deploy prepare rejects build secret ${source}`, (t) => {
    const dir = fixture(t);
    const result = prepare(dir, config(dir, { build: { context: ".", secrets: ["resource"] } }, {}, {}, {
      secrets: { resource: { file: source } },
    }));
    assert.equal(result.status, 1, result.stdout + result.stderr);
    assert.match(result.stdout + result.stderr, /secrets/);
  });
}

test("project build inputs and file secret pass the resolved Compose gate", (t) => {
  const dir = fixture(t);
  writeFileSync(resolve(dir, "resource.txt"), "fixture");
  const result = prepare(dir, config(dir, { build: {
    context: ".", dockerfile: "Dockerfile", additional_contexts: ["extra=./extra"],
    secrets: [{ source: "resource", target: "renamed" }],
  } }, {}, {}, { secrets: { resource: { file: "./resource.txt" } } }));
  assert.equal(result.status, 0, result.stdout + result.stderr);
});

test("remote build inputs pass without granting host file access", (t) => {
  const dir = fixture(t);
  const result = prepare(dir, config(dir, { build: {
    context: "https://example.com/project.git", additional_contexts: {
      extra: "https://example.com/extra.git", base: "docker-image://alpine:3.23",
    },
  } }));
  assert.equal(result.status, 0, result.stdout + result.stderr);
});

for (const [field, key] of [["cache_to", "dest"], ["cache_from", "src"]]) {
  for (const [name, source] of [
    ["outside path", (dir) => resolve(dir, "../other-project/cache")],
    ["project socket", (dir) => resolve(dir, "docker.sock")],
  ]) {
    test(`deploy prepare rejects build ${field} local ${name}`, (t) => {
      const dir = fixture(t);
      const result = prepare(dir, config(dir, { build: {
        context: ".", [field]: [`type=local,${key}=${source(dir)}`],
      } }));
      assert.equal(result.status, 1, result.stdout + result.stderr);
      assert.match(result.stdout + result.stderr, new RegExp(`build\\.${field}`));
      assert.match(result.stdout, /Compose güvenlik kapısı/);
    });
  }

  test(`deploy prepare rejects build ${field} local symlink escape`, (t) => {
    const dir = fixture(t);
    const outside = fixture(t);
    symlinkSync(outside, resolve(dir, "escape"), process.platform === "win32" ? "junction" : "dir");
    const result = prepare(dir, config(dir, { build: {
      context: ".", [field]: [`type=local,${key}=${resolve(dir, "escape/new/cache")}`],
    } }));
    assert.equal(result.status, 1, result.stdout + result.stderr);
    assert.match(result.stdout + result.stderr, new RegExp(`build\\.${field}`));
  });

  test(`project absolute build ${field} local cache passes`, (t) => {
    const dir = fixture(t);
    const result = prepare(dir, config(dir, { build: {
      context: ".", [field]: [`type=local,${key}=${resolve(dir, "cache/new")}`],
    } }));
    assert.equal(result.status, 0, result.stdout + result.stderr);
  });

  test(`build ${field} relative local cache uses the build process cwd`, (t) => {
    const dir = fixture(t);
    const cfg = config(dir, { build: {
      context: "./nested", [field]: [`type=local,${key}=./cache`],
    } });
    assert.equal(cfg.services.web.build[field][0], `type=local,${key}=./cache`);
    // Compose's project directory and build.context do not rebase cache paths.
    const outside = prepare(dir, cfg);
    assert.equal(outside.status, 1, outside.stdout + outside.stderr);
    assert.match(outside.stdout + outside.stderr, new RegExp(`build\\.${field}`));
    const inside = prepare(dir, cfg, false, "/unused", resolve(dir, "releases/revision"));
    assert.equal(inside.status, 0, inside.stdout + inside.stderr);
  });

  test(`build ${field} registry cache passes`, (t) => {
    const dir = fixture(t);
    const result = prepare(dir, config(dir, { build: {
      context: ".", [field]: ["type=registry,ref=example.com/fixture/cache:latest"],
    } }));
    assert.equal(result.status, 0, result.stdout + result.stderr);
  });

  test(`deploy prepare rejects build ${field} local cache with quoted uppercase keys`, (t) => {
    const dir = fixture(t);
    const path = resolve(dir, "../outside/cache").replaceAll("\\", "/");
    const result = prepare(dir, config(dir, { build: {
      context: ".", [field]: [`"TYPE=local","${key.toUpperCase()}=${path}"`],
    } }));
    assert.equal(result.status, 1, result.stdout + result.stderr);
    assert.match(result.stdout + result.stderr, new RegExp(`build\\.${field}`));
  });

  test(`deploy prepare rejects build ${field} local cache without a path`, (t) => {
    const dir = fixture(t);
    const result = prepare(dir, config(dir, { build: { context: ".", [field]: ["type=local"] } }));
    assert.equal(result.status, 1, result.stdout + result.stderr);
    assert.match(result.stdout + result.stderr, new RegExp(`build\\.${field}`));
  });
}

for (const [name, source] of [
  ["outside file", (dir) => resolve(dir, "../absent.env")],
  ["project socket", (dir) => resolve(dir, "docker.sock")],
]) {
  test(`deploy prepare rejects env_file ${name}`, (t) => {
    const dir = fixture(t);
    const result = prepare(dir, config(dir, { env_file: [{ path: source(dir), required: false }] }));
    assert.equal(result.status, 1, result.stdout + result.stderr);
    assert.match(result.stdout + result.stderr, /env_file/);
  });
}

test("project env_file passes the resolved Compose gate", (t) => {
  const dir = fixture(t);
  writeFileSync(resolve(dir, "fixture.env"), "FIXTURE=example\n");
  const result = prepare(dir, config(dir, { env_file: ["./fixture.env"] }));
  assert.equal(result.status, 0, result.stdout + result.stderr);
});

test("only the injected platform env_file is allowed outside the project", (t) => {
  const dir = fixture(t);
  const outside = fixture(t);
  const envFile = resolve(outside, "platform.env");
  writeFileSync(envFile, "FIXTURE=example\n");
  const cfg = config(dir, { env_file: [envFile] });
  assert.equal(prepare(dir, cfg, false, envFile).status, 0);
  cfg.services.web.env_file.push({ path: resolve(outside, "other.env"), required: false });
  const result = prepare(dir, cfg, false, envFile);
  assert.equal(result.status, 1, result.stdout + result.stderr);
  assert.match(result.stdout + result.stderr, /env_file/);
});

test("deploy prepare rejects develop.watch outside path", (t) => {
  const dir = fixture(t);
  const result = prepare(dir, config(dir, { develop: { watch: [
    { action: "sync", path: "..", target: "/data" },
  ] } }));
  assert.equal(result.status, 1, result.stdout + result.stderr);
  assert.match(result.stdout + result.stderr, /develop.watch/);
});

test("deploy prepare rejects develop.watch privileged exec", (t) => {
  const dir = fixture(t);
  const result = prepare(dir, config(dir, { develop: { watch: [
    { action: "sync+exec", path: ".", target: "/data", exec: { command: "id", privileged: true } },
  ] } }));
  assert.equal(result.status, 1, result.stdout + result.stderr);
  assert.match(result.stdout + result.stderr, /develop.watch.*privileged/);
});

test("project develop.watch with unprivileged exec passes", (t) => {
  const dir = fixture(t);
  const result = prepare(dir, config(dir, { develop: { watch: [
    { action: "sync+exec", path: ".", target: "/data", exec: { command: "id", privileged: false } },
  ] } }));
  assert.equal(result.status, 0, result.stdout + result.stderr);
});

test("deploy prepare rejects privileged pre_start in resolved JSON", (t) => {
  const dir = fixture(t);
  const cfg = config(dir);
  // pre_start requires Compose 5.3; exercise its resolved shape on older CLIs too.
  cfg.services.web.pre_start = [{ command: "id", privileged: true }];
  const result = prepare(dir, cfg);
  assert.equal(result.status, 1, result.stdout + result.stderr);
  assert.match(result.stdout + result.stderr, /pre_start privileged/);
});

test("private namespaces and namespaced sysctls pass", (t) => {
  const dir = fixture(t);
  const result = prepare(dir, config(dir, { cgroup: "private", sysctls: { "net.core.somaxconn": "1024" } }));
  assert.equal(result.status, 0, result.stdout + result.stderr);
});

test("external extends cannot hide inherited host privileges", (t) => {
  const dir = fixture(t);
  const outside = fixture(t);
  const file = resolve(outside, "base.yaml");
  writeFileSync(file, JSON.stringify({ services: { base: { image: "fixture", privileged: true } } }));
  const cfg = config(dir, { extends: { file, service: "base" } });
  assert.equal(cfg.services.web.extends, undefined);
  const result = prepare(dir, cfg);
  assert.equal(result.status, 1, result.stdout + result.stderr);
  assert.match(result.stdout + result.stderr, /privileged/);
});
