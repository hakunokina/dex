"""Pack render_maps.py's multilayer EXRs into the runtime maps (#1937; output: src/office/assets/symbiote-cards).

  python pack_maps.py <exr_dir> <out_dir> [asset ...]

Per asset, three lossless WebP (exact RGB under alpha):
  <asset>-n.webp  RGB = camera-space normal  N = 2*RGB/255 - 1 (+X right, +Y up, +Z viewer),  A = coverage
  <asset>-c.webp  RGB = albedo (sRGB),  A = ambient occlusion (linear)
  <asset>-x.webp  R = emit, G = thick, B = rough, A = 1 + 254 * height01   (all linear data)
and meta.json (extent, height range, coverage bounds) for the mock / runtime.
Normal / mask / data channels are never gamma converted.
"""
import json
import os
import struct
import sys

import numpy as np
from PIL import Image

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import shapes  # noqa: E402

NUL = bytes([0])
MAGIC = bytes([0x76, 0x2F, 0x31, 0x01])


def _attrs(data, pos):
    attrs = {}
    while True:
        end = data.index(NUL, pos)
        name = data[pos:end].decode()
        pos = end + 1
        if not name:
            return attrs, pos
        end = data.index(NUL, pos)
        kind = data[pos:end].decode()
        pos = end + 1
        size = struct.unpack("<i", data[pos:pos + 4])[0]
        pos += 4
        attrs[name] = (kind, data[pos:pos + size])
        pos += size


def read_exr(path):
    """Uncompressed scanline EXR, single- or multi-part (Blender 5 multilayer) -> {channel: float32 [h, w]}."""
    data = open(path, "rb").read()
    if data[:4] != MAGIC:
        raise ValueError("not an EXR")
    flags = struct.unpack("<I", data[4:8])[0] >> 8
    if flags & 0x0A:  # tiled / deep
        raise ValueError(f"unsupported EXR flags {flags:#x}")
    multi = bool(flags & 0x10)
    pos = 8
    headers = []
    while True:
        attrs, pos = _attrs(data, pos)
        if not attrs:
            break
        headers.append(attrs)
        if not multi:
            break
    parts = []
    for attrs in headers:
        if attrs["compression"][1][0] != 0:
            raise ValueError("compressed EXR")
        chl = attrs["channels"][1]
        channels = []
        p = 0
        while chl[p] != 0:
            end = chl.index(NUL, p)
            cname = chl[p:end].decode()
            p = end + 1
            channels.append((cname, struct.unpack("<i", chl[p:p + 4])[0]))
            p += 16
        prefix = attrs["name"][1].rstrip(NUL).decode() + "." if multi and "name" in attrs else ""
        xmin, ymin, xmax, ymax = struct.unpack("<iiii", attrs["dataWindow"][1])
        parts.append((prefix, channels, xmin, ymin, xmax - xmin + 1, ymax - ymin + 1))
    tables = []
    for part in parts:
        h = part[5]
        tables.append(np.frombuffer(data, "<u8", h, pos))
        pos += 8 * h
    out = {}
    for (prefix, channels, xmin, ymin, w, h), table in zip(parts, tables):
        planes = {prefix + c: np.empty((h, w), np.float32) for c, _ in channels}
        for off in table:
            q = int(off) + (4 if multi else 0)
            y = struct.unpack("<i", data[q:q + 4])[0]
            q += 8
            for cname, ptype in channels:
                size = 2 if ptype == 1 else 4
                dtype = "<f2" if ptype == 1 else "<f4" if ptype == 2 else "<u4"
                planes[prefix + cname][y - ymin] = np.frombuffer(data, dtype, w, q)
                q += size * w
        out.update(planes)
    return out


def find(ch, *names):
    for n in names:
        for k in ch:
            if k.endswith("." + n) or k == n:
                return ch[k]
    raise KeyError(f"{names} not in {sorted(ch)}")


def srgb(v):
    v = np.clip(v, 0, 1)
    return np.where(v <= 0.0031308, v * 12.92, 1.055 * v ** (1 / 2.4) - 0.055)


def u8(v):
    return np.clip(np.round(v * 255), 0, 255).astype(np.uint8)


def dilate(rgb, alpha, steps=6):
    """Edge-extend colour data under thin alpha so bilinear filtering never pulls in black."""
    rgb = rgb.copy()
    known = alpha > 0.02
    for _ in range(steps):
        acc = np.zeros_like(rgb)
        cnt = np.zeros(alpha.shape)
        for dy, dx in ((-1, 0), (1, 0), (0, -1), (0, 1)):
            k = np.roll(known, (dy, dx), (0, 1))
            acc += np.roll(rgb, (dy, dx), (0, 1)) * k[..., None]
            cnt += k
        grow = (~known) & (cnt > 0)
        rgb[grow] = acc[grow] / cnt[grow][:, None]
        known = known | grow
    return rgb


def pack(name, exr_dir, out_dir):
    spec = shapes.ASSETS[name]
    ch = read_exr(os.path.join(exr_dir, name + ".exr"))
    alpha = find(ch, "Combined.A", "Image.A", "A")
    if spec["tile"]:
        alpha = np.ones_like(alpha)
    safe = np.maximum(alpha, 1e-6)[..., None]

    def un(*names):
        return np.stack([find(ch, n) for n in names], -1) / safe

    normal = un("Normal.X", "Normal.Y", "Normal.Z")
    normal /= np.maximum(np.linalg.norm(normal, axis=-1, keepdims=True), 1e-6)
    normal[alpha < 1e-4] = (0, 0, 1)
    ao = np.clip(find(ch, "Ambient Occlusion.R", "AO.R") / safe[..., 0], 0, 1)
    albedo = un("albedo.R", "albedo.G", "albedo.B")
    data = {k: np.clip(find(ch, k + ".X", k + ".V", k) / safe[..., 0], 0, 1) for k in ("emit", "thick", "rough", "fade")}
    height = find(ch, "height.X", "height.V", "height") / safe[..., 0]
    hmin, hmax = float(height[alpha > 0.5].min()), float(height[alpha > 0.5].max())
    h01 = np.clip((height - hmin) / max(hmax - hmin, 1e-6), 0, 1)
    coverage = np.clip(alpha * data["fade"], 0, 1)

    if not spec["tile"]:
        normal = dilate(normal, alpha)
        normal /= np.maximum(np.linalg.norm(normal, axis=-1, keepdims=True), 1e-6)
        albedo = dilate(albedo, alpha)
        packed = dilate(np.stack([data["emit"], data["thick"], data["rough"], h01], -1), alpha)
        data["emit"], data["thick"], data["rough"], h01 = (packed[..., i] for i in range(4))
        ao = np.where(alpha > 0.02, ao, 1.0)

    n_img = np.dstack([u8(normal * 0.5 + 0.5), u8(coverage)])
    c_img = np.dstack([u8(srgb(albedo)), u8(ao)])
    x_img = np.dstack([u8(data["emit"]), u8(data["thick"]), u8(data["rough"]), (1 + np.round(h01 * 254)).astype(np.uint8)])
    os.makedirs(out_dir, exist_ok=True)
    for suffix, img in (("n", n_img), ("c", c_img), ("x", x_img)):
        Image.fromarray(img, "RGBA").save(os.path.join(out_dir, f"{name}-{suffix}.webp"), lossless=True, quality=100, method=6, exact=True)
    ys, xs = np.nonzero(coverage > 0.5)
    inner = alpha > 0.5
    return {
        "extent": spec["extent"], "res": int(alpha.shape[0]), "tile": spec["tile"],
        "height": [hmin, hmax],
        "bounds": [int(xs.min()), int(ys.min()), int(xs.max()), int(ys.max())] if len(xs) else None,
        "stats": {
            "coverage": round(float((coverage > 0.5).mean()), 4),
            "ao_mean": round(float(ao[inner].mean()), 4),
            "ao_p05": round(float(np.quantile(ao[inner], 0.05)), 4),
            "nz_p05": round(float(np.quantile(normal[..., 2][inner], 0.05)), 4),
            "nz_mean": round(float(normal[..., 2][inner].mean()), 4),
            "emit_mean": round(float(data["emit"][inner].mean()), 4),
            "albedo_lum": round(float((albedo[inner] @ [0.2126, 0.7152, 0.0722]).mean()), 5),
        },
    }


def main():
    exr_dir, out_dir = sys.argv[1], sys.argv[2]
    names = sys.argv[3:] or list(shapes.ASSETS)
    os.makedirs(out_dir, exist_ok=True)
    meta_path = os.path.join(out_dir, "meta.json")
    metas = json.load(open(meta_path)) if os.path.exists(meta_path) else {}
    for name in names:
        metas[name] = pack(name, exr_dir, out_dir)
        print(name, json.dumps(metas[name]["stats"]), metas[name]["bounds"], flush=True)
    json.dump(metas, open(meta_path, "w"), indent=1)


if __name__ == "__main__":
    main()
