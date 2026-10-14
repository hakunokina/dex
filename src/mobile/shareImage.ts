// Paylaşılan görseli gönderime hazırlar: izinli biçim + ≤ 700 KB + uzun kenar ≤ 1600 değilse
// canvas'ta küçültüp JPEG'e çevirir. Sınır kararları saf mantıkta (shareLogic.ts, birim testli).
import {
  IMAGE_MAX_EDGE,
  base64Bytes,
  fitUnderLimit,
  imageFitsAsIs,
  isSubmitMime,
  scaleToFit,
  type SubmitImage,
} from "./shareLogic";

export const TOO_BIG = "Görsel çok büyük";
export const UNREADABLE = "Görsel okunamadı";

function toBlob(base64: string, mime: string): Blob {
  const bin = atob(base64);
  const bytes = new Uint8Array(bin.length);
  for (let i = 0; i < bin.length; i++) bytes[i] = bin.charCodeAt(i);
  return new Blob([bytes], { type: mime });
}

function blobBase64(blob: Blob): Promise<string> {
  return new Promise((resolve, reject) => {
    const r = new FileReader();
    r.onload = () => resolve(String(r.result).split(",", 2)[1] ?? "");
    r.onerror = () => reject(r.error);
    r.readAsDataURL(blob);
  });
}

/** Native tarafın verdiği görsel → `inbox_submit`'e girecek görsel. Hata metni = gösterilecek metin. */
export async function prepareImage(raw: { mime: string; base64: string }): Promise<SubmitImage> {
  let bitmap: ImageBitmap;
  try {
    bitmap = await createImageBitmap(toBlob(raw.base64, raw.mime));
  } catch {
    // Çözülemiyor (ör. HEIC): ham hali sınır içinde ve izinli biçimdeyse yine de gönderilebilir.
    if (isSubmitMime(raw.mime) && imageFitsAsIs(raw.mime, base64Bytes(raw.base64), 0, 0)) return { mime: raw.mime, base64: raw.base64 };
    throw UNREADABLE;
  }
  try {
    if (isSubmitMime(raw.mime) && imageFitsAsIs(raw.mime, base64Bytes(raw.base64), bitmap.width, bitmap.height)) {
      return { mime: raw.mime, base64: raw.base64 };
    }
    const { width, height } = scaleToFit(bitmap.width, bitmap.height, IMAGE_MAX_EDGE);
    const canvas = document.createElement("canvas");
    canvas.width = width;
    canvas.height = height;
    const ctx = canvas.getContext("2d");
    if (!ctx) throw UNREADABLE;
    ctx.fillStyle = "#fff"; // JPEG'de saydamlık siyaha dönmesin
    ctx.fillRect(0, 0, width, height);
    ctx.drawImage(bitmap, 0, 0, width, height);
    const out = await fitUnderLimit<SubmitImage>(async (q) => {
      const blob = await new Promise<Blob | null>((res) => canvas.toBlob(res, "image/jpeg", q));
      if (!blob) throw UNREADABLE;
      return { value: { mime: "image/jpeg", base64: await blobBase64(blob) }, bytes: blob.size };
    });
    if (!out) throw TOO_BIG;
    return out;
  } finally {
    bitmap.close();
  }
}

export const previewUrl = (img: SubmitImage) => `data:${img.mime};base64,${img.base64}`;
