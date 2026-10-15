// #1958: the Kart's lit layer (design #1937 mock `gl.ts`, A · Doku path only). One WebGL2 canvas under the
// office's 2D canvas: a full-screen ground pass (the tiled Blender maps + the strand relief + body
// shadows), then one quad per worker / Echo body with its own maps. Both passes share one light model, so
// every body sits in the ground's light. World builds the scene (CSS px); the strand layers are drawn
// here and re-uploaded only when the layout changes. A lost context reports `ready = false` (Office
// draws the 2D fallback) until `webglcontextrestored` rebuilds it.
import type { Body } from "./companion/coreXL";
import type { Point } from "./placement";
import { LASER, LOOK, beatAt, coreVeins, keyAzimuth, lightDir, linear, pathLength, scale, type Rect, type Rgb } from "./symbioteCard";

export interface MapSet { n: ImageBitmap; c: ImageBitmap; x: ImageBitmap }
export interface CardMaps { bg: MapSet; worker: MapSet; echo: MapSet }

/**
 * CSS px; `z` = height above the ground, `r` = falloff radius, `spec` = its specular share, `halo` = its
 * glow on the ground (#2020: a live sac's light around it; 0 / none: only the lighting).
 */
export interface PointLight { x: number; y: number; z: number; r: number; color: Rgb; spec: number; halo?: number }
/** A body's shadow on the ground (CSS px): `lift` shifts it away from the key light, `k` = strength. */
export interface Shadow { x: number; y: number; rx: number; ry: number; lift: number; k: number }
/**
 * Per strand (flow id = index + 1): active-work band, the returning drop (`packet` < 0: none), and #2020 its
 * life (0 dry .. 1 live) and colour (the live glow's and the drop's).
 */
export interface Flow { flow: number; packet: number; length: number; packetK: number; life: number; color: Rgb }
export interface BodyDraw {
  kind: "worker" | "echo";
  /** Quad centre and side (CSS px), rotation (rad). */
  x: number; y: number; size: number; rot: number;
  state: Rgb; intensity: number; translucency: number; alpha: number; desat: number; pulse: number;
  /** #2020: albedo darkening (a dormant sac), 0..1. */
  dim: number;
  /** Height range of the body's map (CSS px). */
  height: number;
}
/** #2036: a working sac's laser (CSS px): orbit centre, its x radius (y: × the sac's aspect), the head's angle (rad). */
export interface Laser { x: number; y: number; r: number; angle: number }
/** World units: the strand relief, Core anchors and the calm zones under labels. */
export interface Strands { key: string; routes: Point[][]; cores: Body[]; calm: Rect[] }
export interface LitScene {
  time: number; reduced: boolean;
  /** The pulse's origin (CSS px): the first Core. */
  core: { x: number; y: number } | null;
  lights: PointLight[]; shadows: Shadow[]; flows: Flow[]; bodies: BodyDraw[]; lasers: Laser[];
  /** The active-work band's colour (#2036: the theme red, every team). */
  flowColor: Rgb;
  strands: Strands;
}

const MAX_LIGHTS = 24;
const MAX_SHADOWS = 16;
/** #2020: a wide team (~20 workers) keeps every strand's own life; strands past it read dry. */
const MAX_FLOWS = 32;
/** The flow id of strands past MAX_FLOWS. */
const OVER_FLOW = 255;
/** #2036: lasers past it are not drawn (the sac's red still reads working). Ground FS uniforms: ~196 of 224. */
const MAX_LASERS = 16;
const glf = (v: number) => v.toFixed(4);

const HEADER = `#version 300 es
precision highp float;
precision highp int;
uniform vec2 uView;
uniform float uScale;
uniform vec2 uFB;
uniform float uUnit;
uniform float uTime;
uniform vec3 uKeyDir;
uniform vec3 uKeyCol;
uniform vec3 uFillDir;
uniform vec3 uFillCol;
uniform vec3 uAmb;
uniform float uSpec;
uniform float uExposure;
uniform int uNL;
uniform vec4 uLP[${MAX_LIGHTS}];
uniform vec4 uLC[${MAX_LIGHTS}];

float shininess(float r) { r = clamp(r, 0.04, 1.0); return clamp(2.0 / (r * r) - 2.0, 4.0, 1200.0); }

vec3 gDiff; vec3 gSpec; vec3 gTrans;
void lightAll(vec3 P, vec3 n, float rough, float wrap) {
  vec3 V = vec3(0.0, 0.0, 1.0);
  float sh = shininess(rough);
  float norm = (sh + 8.0) / 25.13;
  gDiff = uKeyCol * max((dot(n, uKeyDir) + wrap) / (1.0 + wrap), 0.0);
  vec3 H = normalize(uKeyDir + V);
  gSpec = uKeyCol * pow(max(dot(n, H), 0.0), sh) * norm * step(0.0, dot(n, uKeyDir));
  gDiff += uFillCol * max((dot(n, uFillDir) + wrap) / (1.0 + wrap), 0.0);
  gTrans = vec3(0.0);
  for (int i = 0; i < ${MAX_LIGHTS}; i++) {
    if (i >= uNL) break;
    vec3 L = vec3(uLP[i].xy - P.xy, uLP[i].z - P.z);
    float d = length(L);
    L /= max(d, 1e-3);
    float r = uLP[i].w;
    float att = smoothstep(r * 7.0, r * 2.0, d) / (1.0 + d * d / (r * r));
    gDiff += uLC[i].rgb * max((dot(n, L) + wrap) / (1.0 + wrap), 0.0) * att;
    vec3 Hh = normalize(L + V);
    gSpec += uLC[i].rgb * pow(max(dot(n, Hh), 0.0), sh) * norm * att * uLC[i].w;
    gTrans += uLC[i].rgb * att;
  }
}

vec3 finish(vec3 c) {
  c *= uExposure;
  c = (c * (2.51 * c + 0.03)) / (c * (2.43 * c + 0.59) + 0.14);
  c = clamp(c, 0.0, 1.0);
  return mix(c * 12.92, 1.055 * pow(c, vec3(1.0 / 2.4)) - 0.055, step(0.0031308, c));
}
`;

const GROUND_VS = `#version 300 es
void main() { vec2 p = vec2(float((gl_VertexID << 1) & 2), float(gl_VertexID & 2)); gl_Position = vec4(p * 2.0 - 1.0, 0.0, 1.0); }`;

const GROUND_FS = HEADER + `
uniform sampler2D uGN; uniform sampler2D uGC; uniform sampler2D uGX;
uniform float uTile;
uniform sampler2D uSH; uniform sampler2D uSF;
uniform vec2 uSTex; uniform float uSHk;
uniform vec3 uStrandAlb; uniform float uStrandRough; uniform vec3 uStrandGlow; uniform vec3 uStrandDry; uniform float uDryRough;
uniform vec3 uEmit; uniform vec2 uCore; uniform float uBeat; uniform float uVig;
uniform int uNB; uniform vec4 uBS[${MAX_SHADOWS}]; uniform vec4 uBX[${MAX_SHADOWS}];
uniform int uNR; uniform vec4 uRoute[${MAX_FLOWS}]; uniform vec4 uLife[${MAX_FLOWS}]; uniform vec3 uFlowCol;
uniform vec4 uLH[${MAX_LIGHTS / 4}];
uniform int uNLas; uniform vec4 uLas[${MAX_LASERS}];
out vec4 outColor;

void main() {
  vec2 css = vec2(gl_FragCoord.x, uFB.y - gl_FragCoord.y) / uScale;
  vec2 uv = css / uUnit / uTile;
  vec3 n = normalize(texture(uGN, uv).xyz * 2.0 - 1.0);
  vec4 c = texture(uGC, uv);
  vec4 x = texture(uGX, uv);
  n.y = -n.y;
  vec3 alb = c.rgb; float ao = c.a;
  float emit = x.r, thick = x.g, rough = x.b, hgt = (x.a * 255.0 - 1.0) / 254.0;

  // strand relief (R = height, B = calm zone under labels)
  vec2 suv = css / uView;
  vec4 s = texture(uSH, suv);
  float s0 = s.r;
  // #2020: the strand's life (Core's own veins and the ground off the strands: 1, as before)
  vec4 f = texture(uSF, suv);
  int id = int(f.g * 255.0 + 0.5) - 1;
  float life = id == ${OVER_FLOW - 1} ? 0.0 : id >= 0 && id < uNR ? uLife[id].a : 1.0;
  float sx = texture(uSH, suv + vec2(uSTex.x, 0.0)).r - texture(uSH, suv - vec2(uSTex.x, 0.0)).r;
  float sy = texture(uSH, suv + vec2(0.0, uSTex.y)).r - texture(uSH, suv - vec2(0.0, uSTex.y)).r;
  // A dry strand is thinner and flatter: only the tube's crest stays on.
  float on = smoothstep(mix(0.5, 0.02, life), mix(0.85, 0.4, life), s0);
  n = normalize(vec3(n.xy * (1.0 - 0.8 * on) - vec2(sx, sy) * uSHk * mix(0.5, 1.0, life), n.z));
  alb = mix(alb, mix(uStrandDry, uStrandAlb, life), on * 0.85);
  rough = mix(rough, mix(uDryRough, uStrandRough, life), on);
  thick = mix(thick, 0.5, on);
  ao *= 1.0 - 0.35 * smoothstep(0.0, 0.1, s0) * (1.0 - smoothstep(0.1, 0.45, s0));

  float shadow = 1.0;
  vec2 kd = uKeyDir.xy / max(uKeyDir.z, 0.3);
  for (int i = 0; i < ${MAX_SHADOWS}; i++) {
    if (i >= uNB) break;
    vec4 b = uBS[i]; vec4 e = uBX[i];
    float d = length((css - b.xy + kd * e.x) / (b.zw * (1.0 + e.x / max(b.z, 1.0) * 0.6)));
    shadow *= 1.0 - e.y * 0.62 * smoothstep(1.3, 0.35, d);
    float cd = length((css - b.xy) / b.zw);
    shadow *= 1.0 - e.y * 0.3 * smoothstep(1.22, 0.96, cd) * step(e.x, b.z * 0.4);
  }

  // the pulse: two rings leave Core and light the veins
  float dc = length(css - uCore) / uUnit;
  float wave = 0.0;
  for (int k = 0; k < 2; k++) {
    float ph = fract(uBeat + float(k) * 0.5);
    wave += exp(-pow((dc - ph * 560.0) / 34.0, 2.0)) * (1.0 - ph) * (1.0 - ph);
  }
  vec3 E = uEmit * emit * (0.035 + 0.7 * wave) * (1.0 - mix(0.95, 0.6, life) * on);

  if (id >= 0 && id < uNR && on > 0.01) {
    vec4 R = uRoute[id];
    float along = f.r + f.b / 255.0;
    float band = R.x * pow(0.5 + 0.5 * sin((along * R.z / 46.0 - uTime * 1.5) * 6.2831), 5.0);
    float pk = R.y >= 0.0 ? exp(-pow((along - R.y) * R.z / 10.0, 2.0)) : 0.0;
    // #2020: a live strand glows in its colour (still under reduced motion) and carries the band.
    E += (uLife[id].rgb * uLife[id].a * 0.55 * smoothstep(0.1, 0.7, s0) + uFlowCol * band * 1.6 + uLife[id].rgb * pk * R.w) * on;
  }
  E += uStrandGlow * on * smoothstep(0.15, 0.8, s0) * life;
  // #2020: a live sac's light pools on the ground around it.
  for (int i = 0; i < ${MAX_LIGHTS}; i++) {
    if (i >= uNL) break;
    float hk = uLH[i / 4][i % 4];
    if (hk > 0.0) E += uLC[i].rgb * hk * exp(-pow(length(css - uLP[i].xy) / uLP[i].w, 2.0));
  }
  // #2036: a working sac's laser (the work band's red) — a short lit arc orbiting it, the tail fading behind its head.
  for (int i = 0; i < ${MAX_LASERS}; i++) {
    if (i >= uNLas) break;
    vec4 L = uLas[i];
    vec2 q = (css - L.xy) / vec2(L.z, L.z * ${glf(LOOK.worker.aspect)});
    float d = abs(length(q) - 1.0) * L.z;
    if (d > 8.0 * uUnit) continue;
    float behind = mod(L.w - atan(q.y, q.x), 6.2831853);
    float tail = pow(max(0.0, 1.0 - behind / ${glf(LASER.tail)}), 1.6);
    float head = exp(-pow(min(behind, 6.2831853 - behind) * L.z / (3.0 * uUnit), 2.0));
    float line = exp(-pow(d / (${glf(LASER.width)} * uUnit), 2.0)) + 0.3 * exp(-pow(d / (4.0 * uUnit), 2.0));
    E += uFlowCol * line * (1.2 * tail + 1.0 * head);
  }

  vec3 P = vec3(css, (hgt * 4.0 + s0 * 3.0) * uUnit);
  lightAll(P, n, rough, 0.45 * thick);
  float calm = s.b;
  float F = 0.04 + 0.96 * pow(1.0 - clamp(n.z, 0.0, 1.0), 5.0);
  vec3 col = alb * (uAmb * ao + gDiff * mix(1.0, ao, 0.6)) * shadow;
  col += gSpec * uSpec * (0.25 + F) * shadow * mix(1.0, 0.15, calm);
  col += alb * gTrans * thick * 0.6 * shadow;
  col += E * mix(1.0, 0.2, calm);
  col *= mix(1.0, 0.5, calm);
  vec2 q = css / uView - 0.5;
  col *= clamp(1.0 - uVig * dot(q, q) * 1.6, 0.0, 1.0);
  outColor = vec4(finish(col), 1.0);
}`;

const BODY_VS = `#version 300 es
uniform vec2 uView; uniform vec4 uBody;
out vec2 vUV; out vec2 vCss;
void main() {
  vec2 corner = vec2(float(gl_VertexID & 1), float((gl_VertexID >> 1) & 1));
  vUV = corner;
  vec2 p = (corner - 0.5) * uBody.z;
  float c = cos(uBody.w), s = sin(uBody.w);
  vCss = uBody.xy + vec2(c * p.x - s * p.y, s * p.x + c * p.y);
  gl_Position = vec4(vCss.x / uView.x * 2.0 - 1.0, 1.0 - vCss.y / uView.y * 2.0, 0.0, 1.0);
}`;

const BODY_FS = HEADER + `
uniform sampler2D uBN; uniform sampler2D uBC; uniform sampler2D uBX;
// uMat: translucency, #2020 dim (a dormant sac's albedo darkening), alpha, desaturation
uniform vec4 uBody; uniform vec4 uState; uniform vec4 uMat; uniform vec4 uAnim;
in vec2 vUV; in vec2 vCss;
out vec4 outColor;
void main() {
  vec4 nt = texture(uBN, vUV);
  float cov = nt.a;
  if (cov < 0.004) discard;
  vec3 n = nt.xyz * 2.0 - 1.0;
  n.y = -n.y;
  float cr = cos(uBody.w), sr = sin(uBody.w);
  n.xy = vec2(cr * n.x - sr * n.y, sr * n.x + cr * n.y);
  n = normalize(n);
  vec4 c = texture(uBC, vUV);
  vec4 x = texture(uBX, vUV);
  float hgt = (x.a * 255.0 - 1.0) / 254.0;
  vec3 P = vec3(vCss, hgt * uAnim.y);
  lightAll(P, n, x.b, uMat.x * x.g * 0.8);
  vec3 alb = mix(c.rgb, vec3(dot(c.rgb, vec3(0.2126, 0.7152, 0.0722))), uMat.w) * (1.0 - uMat.y);
  float F = 0.04 + 0.96 * pow(1.0 - clamp(n.z, 0.0, 1.0), 5.0);
  vec3 col = alb * (uAmb * c.a + gDiff) + gSpec * uSpec * (0.35 + F);
  col += alb * gTrans * uMat.x * x.g * 0.5;
  float pulse = 0.55 + 0.45 * uAnim.x;
  col += uState.rgb * uState.a * x.r * pulse * 1.2;
  col += uState.rgb * uState.a * uMat.x * x.g * 0.22 * pulse;
  float a = cov * uMat.z;
  a *= 1.0 - 0.42 * uMat.x * x.g * (1.0 - x.r);
  outColor = vec4(finish(col), a);
}`;

function compile(gl: WebGL2RenderingContext, vs: string, fs: string) {
  const prog = gl.createProgram();
  for (const [type, src] of [[gl.VERTEX_SHADER, vs], [gl.FRAGMENT_SHADER, fs]] as const) {
    const sh = gl.createShader(type)!;
    gl.shaderSource(sh, src);
    gl.compileShader(sh);
    if (!gl.getShaderParameter(sh, gl.COMPILE_STATUS) && !gl.isContextLost()) throw new Error(gl.getShaderInfoLog(sh) ?? "shader");
    gl.attachShader(prog, sh);
    gl.deleteShader(sh);
  }
  gl.linkProgram(prog);
  if (!gl.getProgramParameter(prog, gl.LINK_STATUS) && !gl.isContextLost()) throw new Error(gl.getProgramInfoLog(prog) ?? "link");
  const cache = new Map<string, WebGLUniformLocation | null>();
  const u = (name: string) => {
    if (!cache.has(name)) cache.set(name, gl.getUniformLocation(prog, name));
    return cache.get(name)!;
  };
  return { prog, u };
}

type Program = ReturnType<typeof compile>;

/** The look's constant light (linear colours, once). */
const KEY_COL = scale(linear(LOOK.key.color), LOOK.key.k);
const FILL_COL = scale(linear(LOOK.key.fill), LOOK.key.fillK);
const AMBIENT = scale(linear(LOOK.ambient), LOOK.ambientK);
const EMIT = linear(LOOK.emit);
const STRAND_ALB = linear(LOOK.strand.albedo);
const STRAND_GLOW = scale(linear(LOOK.strand.glow), LOOK.strand.glowK);
const STRAND_DRY = linear(LOOK.strand.dry);

/** The relief's resolution (× CSS px). */
const strandRes = (cssW: number) => Math.min(1, Math.max(0.4, 1600 / Math.max(1, cssW)));

export class LitLayer {
  private gl: WebGL2RenderingContext;
  private ground!: Program;
  private body!: Program;
  private tex = new Map<string, WebGLTexture>();
  private strandH!: WebGLTexture;
  private strandF!: WebGLTexture;
  private strandSize = [1, 1];
  private strandKey = "";
  private lastKey = "";
  private lost = false;
  private readonly heightCanvas = document.createElement("canvas");
  private readonly flowCanvas = document.createElement("canvas");
  private readonly tmpCanvas = document.createElement("canvas");

  constructor(readonly canvas: HTMLCanvasElement, private maps: CardMaps) {
    const gl = canvas.getContext("webgl2", { alpha: false, antialias: false, premultipliedAlpha: false, depth: false, stencil: false });
    if (!gl) throw new Error("WebGL2 yok");
    this.gl = gl;
    canvas.addEventListener("webglcontextlost", this.onLost);
    canvas.addEventListener("webglcontextrestored", this.onRestored);
    this.init();
  }

  /** False while the context is lost (Office draws the 2D fallback meanwhile). */
  get ready() { return !this.lost && !this.gl.isContextLost(); }

  destroy() {
    this.canvas.removeEventListener("webglcontextlost", this.onLost);
    this.canvas.removeEventListener("webglcontextrestored", this.onRestored);
    this.gl.getExtension("WEBGL_lose_context")?.loseContext();
  }

  private onLost = (e: Event) => {
    e.preventDefault(); // lets the browser restore it
    this.lost = true;
  };

  private onRestored = () => {
    try {
      this.init();
      this.lost = false;
    } catch { /* still lost: the fallback stays */ }
  };

  private init() {
    const gl = this.gl;
    this.tex.clear();
    this.strandKey = this.lastKey = "";
    this.ground = compile(gl, GROUND_VS, GROUND_FS);
    this.body = compile(gl, BODY_VS, BODY_FS);
    const aniso = gl.getExtension("EXT_texture_filter_anisotropic");
    // HANDOFF §2: data, not pictures — no premultiply, no colour conversion; only `-c` is sRGB.
    gl.pixelStorei(gl.UNPACK_PREMULTIPLY_ALPHA_WEBGL, false);
    gl.pixelStorei(gl.UNPACK_COLORSPACE_CONVERSION_WEBGL, gl.NONE);
    for (const [key, set] of Object.entries(this.maps) as [keyof CardMaps, MapSet][]) {
      for (const ch of ["n", "c", "x"] as const) {
        const t = gl.createTexture();
        gl.bindTexture(gl.TEXTURE_2D, t);
        gl.texImage2D(gl.TEXTURE_2D, 0, ch === "c" ? gl.SRGB8_ALPHA8 : gl.RGBA8, gl.RGBA, gl.UNSIGNED_BYTE, set[ch]);
        gl.generateMipmap(gl.TEXTURE_2D);
        const wrap = key === "bg" ? gl.REPEAT : gl.CLAMP_TO_EDGE;
        gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_S, wrap);
        gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_T, wrap);
        gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MIN_FILTER, gl.LINEAR_MIPMAP_LINEAR);
        gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MAG_FILTER, gl.LINEAR);
        if (aniso) gl.texParameterf(gl.TEXTURE_2D, aniso.TEXTURE_MAX_ANISOTROPY_EXT, 8);
        this.tex.set(`${key}.${ch}`, t);
      }
    }
    const blank = (filter: number) => {
      const t = gl.createTexture();
      gl.bindTexture(gl.TEXTURE_2D, t);
      gl.texImage2D(gl.TEXTURE_2D, 0, gl.RGBA8, 1, 1, 0, gl.RGBA, gl.UNSIGNED_BYTE, new Uint8Array(4));
      for (const p of [gl.TEXTURE_WRAP_S, gl.TEXTURE_WRAP_T]) gl.texParameteri(gl.TEXTURE_2D, p, gl.CLAMP_TO_EDGE);
      gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MIN_FILTER, filter);
      gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MAG_FILTER, filter);
      return t;
    };
    this.strandH = blank(gl.LINEAR);
    this.strandF = blank(gl.NEAREST);
  }

  /** §6: the strand relief (R), calm zones (B) and flow ids / positions, drawn when the layout changes. */
  private buildStrands(s: Strands, cssW: number, cssH: number, unit: number) {
    const k = strandRes(cssW);
    const w = Math.max(2, Math.round(cssW * k)), h = Math.max(2, Math.round(cssH * k));
    for (const c of [this.heightCanvas, this.flowCanvas, this.tmpCanvas]) { c.width = w; c.height = h; }
    const tg = this.tmpCanvas.getContext("2d")!;
    tg.fillStyle = "#000"; tg.fillRect(0, 0, w, h);
    tg.setTransform(k * unit, 0, 0, k * unit, 0, 0);
    tg.globalCompositeOperation = "lighten";
    tg.lineCap = "round"; tg.lineJoin = "round";
    const half = LOOK.strand.width / 2;
    for (const route of s.routes) strokeProfile(tg, route, half, false);
    for (const body of s.cores) for (const vein of coreVeins(body)) strokeProfile(tg, vein, half * 0.75, true);

    const hg = this.heightCanvas.getContext("2d")!;
    hg.globalCompositeOperation = "copy";
    hg.filter = `blur(${Math.max(0.5, LOOK.strand.blur * unit * k)}px)`;
    hg.drawImage(this.tmpCanvas, 0, 0);
    hg.filter = `blur(${Math.max(2, 5 * unit * k)}px)`;
    hg.globalCompositeOperation = "lighten";
    hg.setTransform(k * unit, 0, 0, k * unit, 0, 0);
    hg.fillStyle = "rgb(0,0,255)";
    for (const r of s.calm) hg.fillRect(r.x - 7, r.y - 6, r.w + 14, r.h + 12);
    hg.filter = "none";
    hg.setTransform(1, 0, 0, 1, 0, 0);
    hg.globalCompositeOperation = "source-over";

    const fg = this.flowCanvas.getContext("2d")!;
    fg.fillStyle = "#000"; fg.fillRect(0, 0, w, h);
    fg.setTransform(k * unit, 0, 0, k * unit, 0, 0);
    fg.lineCap = "round";
    fg.lineWidth = LOOK.strand.width * 1.3;
    s.routes.forEach((route, i) => {
      const id = i < MAX_FLOWS ? i + 1 : OVER_FLOW;
      const total = pathLength(route) || 1;
      let acc = 0;
      for (let j = 1; j < route.length; j++) {
        const a = route[j - 1], b = route[j];
        const len = Math.hypot(b.x - a.x, b.y - a.y);
        const u = (acc + len / 2) / total;
        acc += len;
        const hi = Math.min(255, Math.floor(u * 255));
        fg.strokeStyle = `rgb(${hi},${id},${Math.round((u * 255 - hi) * 255)})`;
        fg.beginPath(); fg.moveTo(a.x, a.y); fg.lineTo(b.x, b.y); fg.stroke();
      }
    });
    fg.setTransform(1, 0, 0, 1, 0, 0);

    const gl = this.gl;
    gl.pixelStorei(gl.UNPACK_PREMULTIPLY_ALPHA_WEBGL, false);
    gl.pixelStorei(gl.UNPACK_COLORSPACE_CONVERSION_WEBGL, gl.NONE);
    gl.bindTexture(gl.TEXTURE_2D, this.strandH);
    gl.texImage2D(gl.TEXTURE_2D, 0, gl.RGBA8, gl.RGBA, gl.UNSIGNED_BYTE, this.heightCanvas);
    gl.bindTexture(gl.TEXTURE_2D, this.strandF);
    gl.texImage2D(gl.TEXTURE_2D, 0, gl.RGBA8, gl.RGBA, gl.UNSIGNED_BYTE, this.flowCanvas);
    this.strandSize = [w, h];
  }

  private common(p: Program, cssW: number, cssH: number, unit: number, scene: LitScene, keyDir: Rgb, fillDir: Rgb) {
    const gl = this.gl;
    gl.uniform2f(p.u("uView"), cssW, cssH);
    gl.uniform1f(p.u("uScale"), this.canvas.width / cssW);
    gl.uniform2f(p.u("uFB"), this.canvas.width, this.canvas.height);
    gl.uniform1f(p.u("uUnit"), unit);
    // The flow band repeats every 2/3 s: an hour's wrap keeps float32 exact without a seam.
    gl.uniform1f(p.u("uTime"), scene.time % 3600);
    gl.uniform3fv(p.u("uKeyDir"), keyDir);
    gl.uniform3fv(p.u("uKeyCol"), KEY_COL);
    gl.uniform3fv(p.u("uFillDir"), fillDir);
    gl.uniform3fv(p.u("uFillCol"), FILL_COL);
    gl.uniform3fv(p.u("uAmb"), AMBIENT);
    gl.uniform1f(p.u("uSpec"), LOOK.spec);
    gl.uniform1f(p.u("uExposure"), LOOK.exposure);
    const lights = scene.lights.slice(0, MAX_LIGHTS);
    gl.uniform1i(p.u("uNL"), lights.length);
    if (lights.length) {
      gl.uniform4fv(p.u("uLP"), lights.flatMap((l) => [l.x, l.y, l.z, l.r]));
      gl.uniform4fv(p.u("uLC"), lights.flatMap((l) => [...l.color, l.spec]));
    }
  }

  private bind(p: Program, unit: number, name: string, tex: WebGLTexture) {
    const gl = this.gl;
    gl.activeTexture(gl.TEXTURE0 + unit);
    gl.bindTexture(gl.TEXTURE_2D, tex);
    gl.uniform1i(p.u(name), unit);
  }

  /** One frame; a still scene (reduced motion, nothing changed) keeps the last image. */
  draw(scene: LitScene, cssW: number, cssH: number, unit: number) {
    if (!this.ready || cssW <= 0 || cssH <= 0) return;
    const strandKey = `${scene.strands.key}|${cssW}x${cssH}|${unit}`;
    if (strandKey !== this.strandKey) {
      this.buildStrands(scene.strands, cssW, cssH, unit);
      this.strandKey = strandKey;
    }
    if (scene.reduced) {
      const key = `${strandKey}|${this.canvas.width}x${this.canvas.height}|${JSON.stringify([scene.core, scene.lights, scene.shadows, scene.flows, scene.bodies, scene.lasers])}`;
      if (key === this.lastKey) return;
      this.lastKey = key;
    } else this.lastKey = "";

    const gl = this.gl;
    const az = keyAzimuth(scene.time, scene.reduced);
    const keyDir = lightDir(az, LOOK.key.elevation);
    const fillDir = lightDir(az + Math.PI, 0.7);
    gl.viewport(0, 0, this.canvas.width, this.canvas.height);
    gl.disable(gl.BLEND);
    const g = this.ground;
    gl.useProgram(g.prog);
    this.common(g, cssW, cssH, unit, scene, keyDir, fillDir);
    this.bind(g, 0, "uGN", this.tex.get("bg.n")!);
    this.bind(g, 1, "uGC", this.tex.get("bg.c")!);
    this.bind(g, 2, "uGX", this.tex.get("bg.x")!);
    this.bind(g, 3, "uSH", this.strandH);
    this.bind(g, 4, "uSF", this.strandF);
    gl.uniform1f(g.u("uTile"), LOOK.tile);
    gl.uniform2f(g.u("uSTex"), 1 / this.strandSize[0], 1 / this.strandSize[1]);
    gl.uniform1f(g.u("uSHk"), (LOOK.strand.height * 3.2 * unit * strandRes(cssW)) / 2);
    gl.uniform3fv(g.u("uStrandAlb"), STRAND_ALB);
    gl.uniform1f(g.u("uStrandRough"), LOOK.strand.rough);
    gl.uniform3fv(g.u("uStrandGlow"), STRAND_GLOW);
    gl.uniform3fv(g.u("uStrandDry"), STRAND_DRY);
    gl.uniform1f(g.u("uDryRough"), LOOK.strand.dryRough);
    const halos = new Float32Array(MAX_LIGHTS);
    scene.lights.slice(0, MAX_LIGHTS).forEach((l, i) => { halos[i] = l.halo ?? 0; });
    gl.uniform4fv(g.u("uLH"), halos);
    gl.uniform3fv(g.u("uEmit"), EMIT);
    // No Core: the rings start far off and never reach the board.
    gl.uniform2f(g.u("uCore"), scene.core?.x ?? -1e5, scene.core?.y ?? -1e5);
    gl.uniform1f(g.u("uBeat"), beatAt(scene.time, scene.reduced));
    gl.uniform1f(g.u("uVig"), LOOK.vignette);
    const sh = scene.shadows.slice(0, MAX_SHADOWS);
    gl.uniform1i(g.u("uNB"), sh.length);
    if (sh.length) {
      gl.uniform4fv(g.u("uBS"), sh.flatMap((s) => [s.x, s.y, s.rx, s.ry]));
      gl.uniform4fv(g.u("uBX"), sh.flatMap((s) => [s.lift, s.k, 0, 0]));
    }
    const flows = scene.flows.slice(0, MAX_FLOWS);
    gl.uniform1i(g.u("uNR"), flows.length);
    if (flows.length) {
      gl.uniform4fv(g.u("uRoute"), flows.flatMap((r) => [r.flow, r.packet, r.length, r.packetK]));
      gl.uniform4fv(g.u("uLife"), flows.flatMap((r) => [...r.color, r.life]));
    }
    gl.uniform3fv(g.u("uFlowCol"), scene.flowColor);
    const lasers = scene.lasers.slice(0, MAX_LASERS);
    gl.uniform1i(g.u("uNLas"), lasers.length);
    if (lasers.length) gl.uniform4fv(g.u("uLas"), lasers.flatMap((l) => [l.x, l.y, l.r, l.angle]));
    gl.drawArrays(gl.TRIANGLES, 0, 3);

    const b = this.body;
    gl.useProgram(b.prog);
    this.common(b, cssW, cssH, unit, scene, keyDir, fillDir);
    gl.enable(gl.BLEND);
    gl.blendFuncSeparate(gl.SRC_ALPHA, gl.ONE_MINUS_SRC_ALPHA, gl.ONE, gl.ONE_MINUS_SRC_ALPHA);
    for (const body of scene.bodies) {
      this.bind(b, 0, "uBN", this.tex.get(`${body.kind}.n`)!);
      this.bind(b, 1, "uBC", this.tex.get(`${body.kind}.c`)!);
      this.bind(b, 2, "uBX", this.tex.get(`${body.kind}.x`)!);
      gl.uniform4f(b.u("uBody"), body.x, body.y, body.size, body.rot);
      gl.uniform4f(b.u("uState"), ...body.state, body.intensity);
      gl.uniform4f(b.u("uMat"), body.translucency, body.dim, body.alpha, body.desat);
      gl.uniform4f(b.u("uAnim"), body.pulse, body.height, 0, 0);
      gl.drawArrays(gl.TRIANGLE_STRIP, 0, 4);
    }
    gl.disable(gl.BLEND);
  }
}

/** The tube's cross-section: half width at height h (0..1). */
const tube = (h: number) => Math.sqrt(1 - h * h);

/** A strand as 14 stacked strokes (`lighten`): R = its height across the tube. */
function strokeProfile(g: CanvasRenderingContext2D, pts: Point[], halfWidth: number, taper: boolean) {
  if (pts.length < 2) return;
  const steps = 14;
  for (let j = 0; j < steps; j++) {
    const h = (j + 1) / steps;
    const w = 2 * halfWidth * tube(h);
    if (w <= 0.05) continue;
    g.strokeStyle = `rgb(${Math.round(h * 255)},0,0)`;
    if (!taper) {
      g.lineWidth = w;
      g.beginPath(); g.moveTo(pts[0].x, pts[0].y);
      for (let i = 1; i < pts.length; i++) g.lineTo(pts[i].x, pts[i].y);
      g.stroke();
    } else {
      for (let i = 1; i < pts.length; i++) {
        g.lineWidth = w * (1 - i / pts.length) + 0.3;
        g.beginPath(); g.moveTo(pts[i - 1].x, pts[i - 1].y); g.lineTo(pts[i].x, pts[i].y); g.stroke();
      }
    }
  }
}
