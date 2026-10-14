// #56: sağlayıcı kimliği → etiket, TM'nin marka rengi, logo. Tek kaynak; kimlikler TM'nin limit
// kataloğundan (src/shared/limitProviders.js), renkler usageCharts.js clientColors'tan, logolar
// assets/icons'tan (lisans: ./icons/NOTICE.md). Bilinmeyen kimlik: baş harf + kimlikten türetilmiş renk.
import alibaba from "./icons/alibaba.svg";
import antigravity from "./icons/antigravity.svg";
import claude from "./icons/claude.svg";
import commandcode from "./icons/commandcode.svg";
import copilot from "./icons/copilot.svg";
import cursor from "./icons/cursor.svg";
import deepseek from "./icons/deepseek.svg";
import droid from "./icons/droid.svg";
import gemini from "./icons/gemini.svg";
import kimi from "./icons/kimi.svg";
import kiro from "./icons/kiro.svg";
import meta from "./icons/meta.svg";
import minimax from "./icons/minimax.svg";
import ollama from "./icons/ollama.svg";
import openai from "./icons/openai.svg";
import opencode from "./icons/opencode.svg";
import openrouter from "./icons/openrouter.svg";
import qoder from "./icons/qoder.svg";
import trae from "./icons/trae.svg";
import volcengine from "./icons/volcengine.svg";
import workbuddy from "./icons/workbuddy.svg";
import xai from "./icons/xai.svg";
import xiaomi from "./icons/xiaomi.svg";
import zai from "./icons/zai.svg";
import zed from "./icons/zed.svg";

export interface ProviderMeta {
  label: string;
  /** Halka rengi; koyu yüzeyde okunmayan (siyah) marka renkleri metin rengine düşer. */
  color: string;
  /** Logo URL'i (maske olarak boyanır); yoksa `glyph`. */
  icon?: string;
  glyph: string;
}

const P = (label: string, brand: string, icon?: string): Omit<ProviderMeta, "color"> & { brand: string } =>
  ({ label, brand, icon, glyph: label.slice(0, 1).toUpperCase() });

const PROVIDERS: Record<string, ReturnType<typeof P>> = {
  claude: P("Claude", "#cc7c5e", claude),
  openai: P("OpenAI", "#49a3b0", openai),
  opencode: P("OpenCode", "#000000", opencode),
  cursor: P("Cursor", "#000000", cursor),
  antigravity: P("Antigravity", "#4285f4", antigravity),
  factory: P("Factory Droid", "#000000", droid),
  kimi: P("Kimi", "#16191e", kimi),
  grok: P("Grok", "#000000", xai),
  copilot: P("GitHub Copilot", "#000000", copilot),
  zed: P("Zed", "#4173e7", zed),
  commandcode: P("Command Code", "#8C4EDD", commandcode),
  mimo: P("MiMo", "#ff6700", xiaomi),
  zai: P("GLM", "#000000", zai),
  glm: P("GLM", "#000000", zai),
  zaiteam: P("GLM Team", "#000000", zai),
  kiro: P("Kiro", "#9046FF", kiro),
  workbuddy: P("WorkBuddy", "#0DC8A5", workbuddy),
  qoder: P("Qoder", "#2ADB5C", qoder),
  deepseek: P("DeepSeek", "#4d6bfe", deepseek),
  muse: P("Muse Code", "#0866FF", meta),
  openrouter: P("OpenRouter", "#6566F1", openrouter),
  minimax: P("Minimax", "#f23f5d", minimax),
  volcengine: P("Volcengine", "#006EFF", volcengine),
  ollama: P("Ollama", "#888888", ollama),
  trae: P("Trae CN", "#32F08C", trae),
  alibaba: P("Alibaba Cloud", "#615CED", alibaba),
  gemini: P("Gemini", "#4285f4", gemini),
};

function luminance(hex: string): number {
  const [r, g, b] = [1, 3, 5].map((i) => {
    const v = parseInt(hex.slice(i, i + 2), 16) / 255;
    return v <= 0.03928 ? v / 12.92 : ((v + 0.055) / 1.055) ** 2.4;
  });
  return 0.2126 * r + 0.7152 * g + 0.0722 * b;
}

/** Şerit yüzeyi (--surface-1); TM'nin readableColor'ı gibi kontrast < 1.8 ise metin rengi. */
const SURFACE = "#121316";

export function readableColor(hex: string): string {
  const [a, b] = [luminance(hex), luminance(SURFACE)];
  return (Math.max(a, b) + 0.05) / (Math.min(a, b) + 0.05) < 1.8 ? "var(--text)" : hex;
}

export function providerMeta(id: string): ProviderMeta {
  const known = PROVIDERS[id];
  if (known) return { label: known.label, icon: known.icon, glyph: known.glyph, color: readableColor(known.brand) };
  let h = 0;
  for (const c of id) h = (h * 31 + c.charCodeAt(0)) % 360;
  return { label: id, glyph: id.slice(0, 1).toUpperCase(), color: `hsl(${h} 45% 60%)` };
}
