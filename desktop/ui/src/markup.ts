export type MarkupColor = "red" | "yellow" | "white" | "black";
export type MarkupThickness = "thin" | "medium" | "thick";

export const MARKUP_COLORS: Record<MarkupColor, string> = {
  red: "#E53935",
  yellow: "#FFD400",
  white: "#FFFFFF",
  black: "#111111",
};

const THICKNESS: Record<MarkupThickness, number> = {
  thin: 0.006,
  medium: 0.013,
  thick: 0.026,
};

export type MarkupPoint = { x: number; y: number };
export type MarkupStroke = {
  color: MarkupColor;
  thickness: MarkupThickness;
  points: MarkupPoint[];
};
export type MarkupFit = { scale: number; offsetX: number; offsetY: number };

export function markupWidth(thickness: MarkupThickness, longestEdge: number): number {
  return Math.max(1, THICKNESS[thickness] * Math.max(1, longestEdge));
}

export function markupFit(
  imageWidth: number,
  imageHeight: number,
  viewWidth: number,
  viewHeight: number,
): MarkupFit {
  if (imageWidth <= 0 || imageHeight <= 0 || viewWidth <= 0 || viewHeight <= 0) {
    return { scale: 1, offsetX: 0, offsetY: 0 };
  }
  const scale = Math.min(viewWidth / imageWidth, viewHeight / imageHeight);
  return {
    scale,
    offsetX: (viewWidth - imageWidth * scale) / 2,
    offsetY: (viewHeight - imageHeight * scale) / 2,
  };
}

export function toImagePoint(fit: MarkupFit, view: MarkupPoint): MarkupPoint {
  return { x: (view.x - fit.offsetX) / fit.scale, y: (view.y - fit.offsetY) / fit.scale };
}

export async function compositeMarkup(
  source: string,
  strokes: MarkupStroke[],
  maxBytes: number,
): Promise<Blob> {
  const image = new Image();
  image.src = source;
  await image.decode();
  const canvas = document.createElement("canvas");
  canvas.width = image.naturalWidth;
  canvas.height = image.naturalHeight;
  const context = canvas.getContext("2d", { alpha: false });
  if (!context) throw new Error("Could not draw on that photo.");
  context.drawImage(image, 0, 0);
  const longest = Math.max(canvas.width, canvas.height);
  context.lineCap = "round";
  context.lineJoin = "round";
  for (const stroke of strokes) {
    if (!stroke.points.length) continue;
    context.strokeStyle = MARKUP_COLORS[stroke.color];
    context.lineWidth = markupWidth(stroke.thickness, longest);
    context.beginPath();
    context.moveTo(stroke.points[0].x, stroke.points[0].y);
    for (const point of stroke.points.slice(1)) context.lineTo(point.x, point.y);
    if (stroke.points.length === 1) {
      context.lineTo(stroke.points[0].x + 0.01, stroke.points[0].y);
    }
    context.stroke();
  }
  for (const quality of [0.86, 0.74, 0.62, 0.5, 0.38]) {
    const blob = await new Promise<Blob | null>((resolve) =>
      canvas.toBlob(resolve, "image/jpeg", quality),
    );
    if (blob && blob.size <= maxBytes) return blob;
  }
  throw new Error("That drawing made the photo too large to send.");
}
