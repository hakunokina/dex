#!/usr/bin/env python3
"""Validate resolved Compose JSON before build/up; this is not a sandbox."""

import csv
import json
from pathlib import Path
import stat
import sys


def validate(config, project_root, port, trusted_env_file=None):
    root = Path(project_root).resolve(strict=True)
    trusted_env = Path(trusted_env_file).resolve() if trusted_env_file else None
    services = config["services"]
    if not root.is_dir() or not services:
        raise ValueError("missing project root/services")
    errors = []
    for name, volume in config.get("volumes", {}).items():
        if "driver_opts" in volume:
            errors.append(f"volume {name}: driver_opts")
    for name, network in config.get("networks", {}).items():
        if network.get("driver") == "host" or network.get("name") == "host":
            errors.append(f"network {name}: host")
    published = False
    for name, service in services.items():
        def reject(rule):
            errors.append(f"{name}: {rule}")

        def check_host_path(value, label, allowed_file=None):
            source = Path(value)
            resolved = source.resolve(strict=False)
            if not source.is_absolute() or (not resolved.is_relative_to(root) and resolved != allowed_file):
                reject(f"{label} proje veri kökü dışında")
            elif source.suffix.lower() == ".sock" or resolved.suffix.lower() == ".sock" or (
                resolved.exists() and stat.S_ISSOCK(resolved.stat().st_mode)
            ):
                reject(f"{label} daemon/socket erişimi")

        def check_resources(kind, references):
            for reference in references:
                key = reference if isinstance(reference, str) else reference["source"]
                resource = config[kind][key]
                if "file" in resource:
                    check_host_path(resource["file"], kind)

        for kind in ("secrets", "configs"):
            check_resources(kind, service.get(kind, []))
        for entry in service.get("env_file", []):
            value = entry if isinstance(entry, str) else entry["path"]
            # Only the deployer's injected file is exempt from the project boundary.
            check_host_path(value, "env_file", trusted_env)

        for binding in service.get("ports", []):
            if binding.get("host_ip") != "127.0.0.1":
                reject("port 127.0.0.1 değil")
            published |= str(binding.get("published")) == port
        for key in ("network_mode", "pid", "ipc", "userns_mode", "cgroup", "uts"):
            if service.get(key) == "host":
                reject(f"{key} host")
        for key in ("network_mode", "pid", "ipc"):
            if service.get(key, "").startswith("container:"):
                reject(f"{key} container namespace")
        for key in (
            "privileged", "cap_add", "devices", "device_cgroup_rules", "volumes_from",
            "use_api_socket", "cgroup_parent", "gpus", "provider",
        ):
            if service.get(key):
                reject(key)
        reservations = service.get("deploy", {}).get("resources", {}).get("reservations", {})
        if reservations.get("devices"):
            reject("reserved devices")
        for hook in ("post_start", "pre_stop", "pre_start"):
            if any(step.get("privileged") for step in service.get(hook, [])):
                reject(f"{hook} privileged")
        for watch in service.get("develop", {}).get("watch", []):
            check_host_path(watch["path"], "develop.watch")
            if watch.get("exec", {}).get("privileged"):
                reject("develop.watch exec privileged")
        for option in service.get("security_opt", []):
            if option not in ("no-new-privileges", "no-new-privileges:true", "no-new-privileges=true"):
                reject("security_opt gevşetmesi")
        build = service.get("build", {})
        if isinstance(build, dict) and (
            build.get("privileged") or build.get("entitlements") or build.get("network") == "host"
        ):
            reject("build host yetkisi")
        if isinstance(build, dict):
            for field, key in (("cache_to", "dest"), ("cache_from", "src")):
                for entry in build.get(field, []):
                    if "=" not in entry:  # Registry reference shorthand.
                        continue
                    try:
                        fields = next(csv.reader([entry], strict=True))
                        options = {k.lower(): v for k, v in (item.split("=", 1) for item in fields)}
                    except (csv.Error, ValueError, StopIteration):
                        reject(f"build.{field} cache girdisi doğrulanamadı")
                        continue
                    if options.get("type") == "local":
                        value = options.get(key)
                        if not value:
                            reject(f"build.{field} local {key} eksik")
                            continue
                        # Compose forwards cache paths unchanged. BuildKit opens them
                        # from the caller's CWD, shared by validation and dc build/up.
                        check_host_path(Path.cwd() / value, f"build.{field}.{key}")
            check_resources("secrets", build.get("secrets", []))
            if build.get("ssh"):
                reject("build.ssh")
            context = build.get("context")
            remote = context and context.startswith(("https://", "http://", "git://"))
            if context and not remote:
                check_host_path(context, "build.context")
            dockerfile = build.get("dockerfile")
            if dockerfile and (Path(dockerfile).is_absolute() or not remote):
                check_host_path(Path(context) / dockerfile, "build.dockerfile")
            for value in build.get("additional_contexts", {}).values():
                if not value.startswith(("https://", "http://", "git://", "docker-image://", "service:")):
                    check_host_path(value, "build.additional_contexts")
        for mount in service.get("volumes", []):
            kind = mount.get("type")
            if kind in ("volume", "tmpfs"):
                continue
            if kind != "bind":
                reject("bilinmeyen mount türü")
                continue
            check_host_path(mount["source"], "bind")
    if not published:
        errors.append(f"hiçbir servis 127.0.0.1:{port}'a yayınlamıyor")
    return errors


if __name__ == "__main__":
    try:
        errors = validate(json.load(sys.stdin), sys.argv[1], sys.argv[2], sys.argv[3] if len(sys.argv) > 3 else None)
    except (IndexError, KeyError, TypeError, ValueError, OSError, RuntimeError):
        errors = ["Compose config veya proje veri kökü doğrulanamadı"]
    for error in errors:
        print(error, file=sys.stderr)
    sys.exit(1 if errors else 0)
