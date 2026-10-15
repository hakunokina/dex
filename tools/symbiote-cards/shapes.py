"""Procedural heightfields for the symbiote card, direction A — Doku (#1937 mock, shipped in #1957).

Pure numpy, so it runs both inside Blender (render_maps.py) and in plain Python (checks).
Every asset is a top-down heightfield: the camera is a fixed orthographic one looking down -Z,
+X right, +Y up, +Z toward the viewer (same convention as src/office/companion/assets/symbiote).

An asset function takes model-space x, y arrays and returns a dict of per-point arrays:
  h      height (model units)
  inside bool coverage (bodies only)
  albedo (N, 3) linear RGB
  emit   0..1  where the state / glow light shows
  thick  0..1  translucency (wrap diffuse + light bleed); see src/office/assets/symbiote-cards/HANDOFF.md
  rough  0..1
  fade   0..1  1 = body, 0 = merges into the ground (A never fades: always 1)
"""
import numpy as np

# ---------------------------------------------------------------- noise

_M32 = np.uint64(0xFFFFFFFF)


def _hash(ix, iy, seed):
    ix = np.asarray(ix, dtype=np.int64).astype(np.uint64)
    iy = np.asarray(iy, dtype=np.int64).astype(np.uint64)
    h = (ix * np.uint64(0x9E3779B1)) ^ (iy * np.uint64(0x85EBCA77)) ^ (np.uint64(seed) * np.uint64(0xC2B2AE3D))
    h &= _M32
    h ^= h >> np.uint64(15)
    h = (h * np.uint64(0x2C1B3C6D)) & _M32
    h ^= h >> np.uint64(12)
    h = (h * np.uint64(0x297A2D39)) & _M32
    h ^= h >> np.uint64(15)
    return h.astype(np.float64) / 4294967296.0


def _wrap(i, period):
    return i % period if period else i + (1 << 24)


def _fade(t):
    return t * t * t * (t * (t * 6 - 15) + 10)


def perlin(x, y, period=None, seed=0):
    """2D gradient noise in lattice units, ~[-1, 1]; periodic in `period` cells when given."""
    x0 = np.floor(x)
    y0 = np.floor(y)
    fx = x - x0
    fy = y - y0
    ix = x0.astype(np.int64)
    iy = y0.astype(np.int64)

    def grad(i, j, dx, dy):
        a = _hash(_wrap(i, period), _wrap(j, period), seed) * (2 * np.pi)
        return np.cos(a) * dx + np.sin(a) * dy

    u = _fade(fx)
    v = _fade(fy)
    n00 = grad(ix, iy, fx, fy)
    n10 = grad(ix + 1, iy, fx - 1, fy)
    n01 = grad(ix, iy + 1, fx, fy - 1)
    n11 = grad(ix + 1, iy + 1, fx - 1, fy - 1)
    a = n00 + (n10 - n00) * u
    b = n01 + (n11 - n01) * u
    return (a + (b - a) * v) * 1.41421356


def fbm(x, y, octaves, base, seed, periodic=True, gain=0.5):
    """Sum of octaves; `base` = lattice cells per model unit. Periodic over 1 model unit when asked."""
    total = np.zeros_like(x, dtype=np.float64)
    amp = 1.0
    norm = 0.0
    freq = base
    for o in range(octaves):
        p = int(round(freq)) if periodic else None
        total += amp * perlin(x * freq, y * freq, p, seed + 101 * o)
        norm += amp
        amp *= gain
        freq *= 2
    return total / norm


def worley(x, y, cells, seed, jitter=0.85, periodic=True):
    """F1, F2 (cell units), the nearest feature's id (0..1) and its position (cell units)."""
    X = x * cells
    Y = y * cells
    ix = np.floor(X).astype(np.int64)
    iy = np.floor(Y).astype(np.int64)
    f1 = np.full(X.shape, np.inf)
    f2 = np.full(X.shape, np.inf)
    cid = np.zeros(X.shape)
    fx = np.zeros(X.shape)
    fy = np.zeros(X.shape)
    p = cells if periodic else None
    for dx in (-1, 0, 1):
        for dy in (-1, 0, 1):
            cx = ix + dx
            cy = iy + dy
            wx = _wrap(cx, p)
            wy = _wrap(cy, p)
            px = cx + 0.5 + jitter * (_hash(wx, wy, seed) - 0.5)
            py = cy + 0.5 + jitter * (_hash(wx, wy, seed + 1) - 0.5)
            d = np.hypot(px - X, py - Y)
            closer = d < f1
            f2 = np.where(closer, f1, np.minimum(f2, d))
            cid = np.where(closer, _hash(wx, wy, seed + 2), cid)
            fx = np.where(closer, px, fx)
            fy = np.where(closer, py, fy)
            f1 = np.where(closer, d, f1)
    return f1, f2, cid, fx, fy


def smoothstep(e0, e1, v):
    t = np.clip((v - e0) / (e1 - e0), 0.0, 1.0)
    return t * t * (3 - 2 * t)


def srgb_to_linear(c):
    c = np.asarray(c, dtype=np.float64)
    return np.where(c <= 0.04045, c / 12.92, ((c + 0.055) / 1.055) ** 2.4)


def hexcol(h):
    h = h.lstrip("#")
    return srgb_to_linear(np.array([int(h[i:i + 2], 16) / 255.0 for i in (0, 2, 4)]))


def mix(a, b, t):
    t = np.asarray(t, dtype=np.float64)[..., None]
    a = np.asarray(a, dtype=np.float64)
    b = np.asarray(b, dtype=np.float64)
    return a * (1 - t) + b * t


def _cap(v):
    """sqrt dome profile: 1 at the centre, vertical tangent at the rim."""
    return np.sqrt(np.clip(1.0 - v * v, 0.0, 1.0))


def _out(n, h, inside=None, albedo=None, emit=None, thick=None, rough=None, fade=None):
    z = np.zeros(n)
    return {
        "h": h,
        "inside": np.ones(n, bool) if inside is None else inside,
        "albedo": albedo,
        "emit": z if emit is None else np.clip(emit, 0, 1),
        "thick": z if thick is None else np.clip(thick, 0, 1),
        "rough": np.full(n, 0.5) if rough is None else np.clip(rough, 0, 1),
        "fade": np.ones(n) if fade is None else np.clip(fade, 0, 1),
    }


# ================================================================ A — Doku (living tissue)

def a_bg(x, y):
    n = x.size
    wx = x + 0.035 * fbm(x, y, 3, 3, 11)
    wy = y + 0.035 * fbm(x, y, 3, 3, 12)
    f1, f2, cid, _, _ = worley(wx, wy, 7, 21)
    e = f2 - f1
    cell = smoothstep(0.0, 0.42, e)
    groove = 1 - smoothstep(0.0, 0.05, e)
    vein1 = 1 - smoothstep(0.0, 0.014, np.abs(fbm(x, y, 3, 3, 31)))
    vein2 = 1 - smoothstep(0.0, 0.018, np.abs(fbm(x, y, 2, 8, 41)))
    pores = smoothstep(0.0, 0.22, worley(x, y, 46, 61)[0])
    h = (0.030 * cell ** 0.55 - 0.006 * groove
         + 0.020 * np.sqrt(vein1) + 0.008 * np.sqrt(vein2)
         + 0.0025 * fbm(x, y, 3, 32, 51) - 0.0018 * (1 - pores) * cell)
    base = mix(hexcol("#1a0a0e"), hexcol("#2a1016"), cid * 0.7 + 0.3 * (fbm(x, y, 2, 12, 71) * 0.5 + 0.5))
    albedo = mix(base, hexcol("#0d0507"), groove * 0.8)
    albedo = mix(albedo, hexcol("#3a0f19"), np.maximum(vein1, vein2 * 0.7))
    emit = np.maximum(vein1, 0.55 * vein2)
    thick = 0.25 + 0.65 * cell
    rough = 0.20 + 0.30 * groove + 0.1 * (1 - pores)
    return _out(n, h, albedo=albedo, emit=emit, thick=thick, rough=rough)


def _a_worker_edge(x, y):
    t = np.arctan2(y, x)
    return 1.0 + 0.045 * np.sin(3 * t + 1.1) + 0.025 * np.sin(5 * t - 0.4)


def a_worker(x, y):
    """Kese — a wet spawn pod with a glowing nucleus."""
    n = x.size
    rr = np.hypot(x / 0.98, y / 0.80) / _a_worker_edge(x, y)
    inside = rr < 1.0
    dome = _cap(rr) ** 0.85
    cx, cy, cr = -0.07, 0.07, 0.36
    nd = np.hypot(x - cx, y - cy) / cr
    nuc = _cap(nd)
    folds = 1 - np.abs(fbm(x + 3.1, y, 3, 2.2, 81, periodic=False))
    h = (0.44 * dome + 0.11 * nuc - 0.035 * np.exp(-((nd - 1.1) / 0.13) ** 2)
         + 0.022 * smoothstep(0.75, 0.97, folds) * dome
         + 0.006 * fbm(x, y, 3, 9, 91, periodic=False) * dome)
    membrane = mix(hexcol("#2c1219"), hexcol("#3d1822"), smoothstep(-0.3, 0.6, fbm(x, y, 2, 4, 93, periodic=False)))
    albedo = mix(membrane, hexcol("#12070a"), smoothstep(1.15, 0.7, nd))
    emit = smoothstep(1.05, 0.35, nd)
    thick = 0.85 - 0.55 * nuc
    rough = 0.16 + 0.12 * (1 - dome)
    return _out(n, h, inside, albedo, emit, thick, rough)


def a_echo(x, y):
    """Medüz — a hovering bell with eight short glowing tentacles (patron's assistant)."""
    n = x.size
    r = np.hypot(x, y)
    t = np.arctan2(y, x)
    rb = 0.60 * (1 + 0.045 * np.cos(12 * t))
    bell = r < rb
    hb = 0.40 * _cap(r / rb) + 0.016 * np.cos(12 * t) * (r / rb) ** 1.5
    hb += 0.05 * _cap(r / 0.14) - 0.02 * np.exp(-((r - 0.27) / 0.035) ** 2)
    harm = np.zeros(n)
    arm_in = np.zeros(n, bool)
    tip = np.zeros(n)
    for k in range(8):
        a0 = k * np.pi / 4 + 0.2
        # the arm's centre line: radius s, angle a0 + wiggle(s)
        s = np.clip(r, 0.42, 1.04)
        ang = a0 + 0.16 * np.sin(5.0 * s + k)
        cxk, cyk = s * np.cos(ang), s * np.sin(ang)
        d = np.hypot(x - cxk, y - cyk)
        u = np.clip((s - 0.42) / 0.62, 0, 1)
        w = 0.075 * (1 - u) ** 0.7 + 0.016
        on = (d < w) & (r > 0.42) & (r < 1.04)
        arm_in |= on
        harm = np.maximum(harm, np.where(on, 0.07 * (1 - 0.6 * u) * _cap(d / w), 0))
        tip = np.maximum(tip, np.where(on, smoothstep(0.88, 1.0, u), 0))
    inside = bell | arm_in
    h = np.maximum(np.where(bell, hb, 0), harm)
    rim = smoothstep(0.78, 0.96, r / rb) * bell
    ring = np.exp(-((r - 0.27) / 0.05) ** 2) * bell
    albedo = mix(hexcol("#8fb9c2"), hexcol("#d7eef1"), rim)
    albedo = np.where(bell[:, None], albedo, mix(hexcol("#6f9ca6"), hexcol("#c9eef2"), tip))
    emit = np.clip(0.85 * rim + 0.6 * ring + tip + 0.25 * bell, 0, 1)
    thick = np.where(bell, 0.9, 0.7)
    rough = np.full(n, 0.14)
    return _out(n, h, inside, albedo, emit, thick, rough)


# ---------------------------------------------------------------- registry
# extent: model units covered by the square map; tile: periodic ground (period = 1 model unit).
ASSETS = {
    "A-bg": dict(fn=a_bg, tile=True, res=1024, extent=1.0, ao=0.020, samples=64),
    "A-worker": dict(fn=a_worker, tile=False, res=512, extent=2.3, ao=0.30, samples=96),
    "A-echo": dict(fn=a_echo, tile=False, res=512, extent=2.3, ao=0.25, samples=96),
}
