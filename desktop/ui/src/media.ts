import { userCopy } from "./presentation";
import type { AttachmentDraft } from "./types";

function bytesToBase64(bytes: Uint8Array): string {
  let binary = "";
  const stride = 0x8000;
  for (let offset = 0; offset < bytes.length; offset += stride) {
    binary += String.fromCharCode(...bytes.subarray(offset, offset + stride));
  }
  return btoa(binary);
}

async function imageElement(file: File): Promise<HTMLImageElement> {
  const url = URL.createObjectURL(file);
  try {
    const image = new Image();
    image.src = url;
    await image.decode();
    return image;
  } finally {
    URL.revokeObjectURL(url);
  }
}

export async function prepareAttachment(
  file: File,
  maxBytes: number,
  durationMs = 0,
): Promise<AttachmentDraft> {
  if (file.type.startsWith("image/")) {
    const image = await imageElement(file);
    const longest = Math.max(image.naturalWidth, image.naturalHeight);
    const scale = Math.min(1, 1920 / Math.max(1, longest));
    const canvas = document.createElement("canvas");
    canvas.width = Math.max(1, Math.round(image.naturalWidth * scale));
    canvas.height = Math.max(1, Math.round(image.naturalHeight * scale));
    canvas.getContext("2d", { alpha: false })?.drawImage(image, 0, 0, canvas.width, canvas.height);
    for (const quality of [0.86, 0.74, 0.62, 0.5, 0.38]) {
      const blob = await new Promise<Blob | null>((resolve) =>
        canvas.toBlob(resolve, "image/jpeg", quality),
      );
      if (blob && blob.size <= maxBytes) {
        return {
          kind: "image",
          mime_type: "image/jpeg",
          duration_ms: 0,
          data_base64: bytesToBase64(new Uint8Array(await blob.arrayBuffer())),
          caption: "",
        };
      }
    }
    throw new Error(userCopy.photoTooLarge);
  }
  if (file.type.startsWith("audio/")) {
    if (file.size > maxBytes) throw new Error(userCopy.voiceTooLong);
    return {
      kind: "audio",
      mime_type: file.type || "audio/webm",
      duration_ms: durationMs,
      data_base64: bytesToBase64(new Uint8Array(await file.arrayBuffer())),
      caption: "",
    };
  }
  throw new Error("Choose a photo or audio recording.");
}
