import { Button } from "@fluentui/react-components";
import { PointerEvent, useMemo, useRef, useState } from "react";
import {
  MARKUP_COLORS,
  MarkupColor,
  MarkupFit,
  MarkupStroke,
  MarkupThickness,
  compositeMarkup,
  markupFit,
  markupWidth,
  toImagePoint,
} from "./markup";
import { userCopy } from "./presentation";

export function PhotoMarkup({
  source,
  maxBytes,
  onConfirm,
  onCancel,
}: {
  source: string;
  maxBytes: number;
  onConfirm: (file: File) => void;
  onCancel: () => void;
}) {
  const stage = useRef<HTMLDivElement>(null);
  const image = useRef<HTMLImageElement>(null);
  const [strokes, setStrokes] = useState<MarkupStroke[]>([]);
  const [active, setActive] = useState<MarkupStroke>();
  const [color, setColor] = useState<MarkupColor>("red");
  const [thickness, setThickness] = useState<MarkupThickness>("medium");
  const [busy, setBusy] = useState(false);
  const fit = useMemo<MarkupFit>(() => {
    const box = stage.current?.getBoundingClientRect();
    const photo = image.current;
    if (!box || !photo?.naturalWidth) return { scale: 1, offsetX: 0, offsetY: 0 };
    return markupFit(photo.naturalWidth, photo.naturalHeight, box.width, box.height);
  }, [source, strokes, active]);

  function pointFromEvent(event: PointerEvent<HTMLDivElement>) {
    const box = stage.current?.getBoundingClientRect();
    if (!box) return { x: 0, y: 0 };
    return toImagePoint(fit, { x: event.clientX - box.left, y: event.clientY - box.top });
  }

  async function confirm() {
    setBusy(true);
    try {
      if (!strokes.length) {
        const response = await fetch(source);
        onConfirm(new File([await response.blob()], "photo.jpg", { type: "image/jpeg" }));
        return;
      }
      const blob = await compositeMarkup(source, strokes, maxBytes);
      onConfirm(new File([blob], "photo.jpg", { type: "image/jpeg" }));
    } finally {
      setBusy(false);
    }
  }

  const visible = active ? [...strokes, active] : strokes;
  return (
    <div className="photo-markup" role="dialog" aria-label="Draw on photo">
      <div
        ref={stage}
        className="markup-stage"
        onPointerDown={(event) => {
          event.currentTarget.setPointerCapture(event.pointerId);
          setActive({ color, thickness, points: [pointFromEvent(event)] });
        }}
        onPointerMove={(event) => {
          if (!active) return;
          setActive({ ...active, points: [...active.points, pointFromEvent(event)] });
        }}
        onPointerUp={() => {
          if (!active) return;
          setStrokes((current) => [...current, active]);
          setActive(undefined);
        }}
      >
        <img ref={image} src={source} alt="" />
        <svg className="markup-overlay" viewBox={`0 0 ${image.current?.naturalWidth || 1} ${image.current?.naturalHeight || 1}`}>
          {visible.map((stroke, index) => (
            <polyline
              key={index}
              fill="none"
              stroke={MARKUP_COLORS[stroke.color]}
              strokeLinecap="round"
              strokeLinejoin="round"
              strokeWidth={markupWidth(stroke.thickness, image.current?.naturalWidth || 1)}
              points={stroke.points.map((point) => `${point.x},${point.y}`).join(" ")}
            />
          ))}
        </svg>
      </div>
      <div className="markup-toolbar">
        {(["red", "yellow", "white", "black"] as MarkupColor[]).map((value) => (
          <button
            key={value}
            type="button"
            className={`markup-swatch ${color === value ? "selected" : ""}`}
            style={{ background: MARKUP_COLORS[value] }}
            aria-label={value}
            onClick={() => setColor(value)}
          />
        ))}
        {(["thin", "medium", "thick"] as MarkupThickness[]).map((value) => (
          <Button key={value} appearance={thickness === value ? "primary" : "secondary"} onClick={() => setThickness(value)}>
            {value}
          </Button>
        ))}
        <Button disabled={!strokes.length && !active} onClick={() => { setActive(undefined); setStrokes((current) => current.slice(0, -1)); }}>
          {userCopy.undo}
        </Button>
        <Button disabled={!strokes.length && !active} onClick={() => { setActive(undefined); setStrokes([]); }}>
          {userCopy.clearDrawing}
        </Button>
        <Button appearance="secondary" onClick={onCancel}>Cancel</Button>
        <Button appearance="primary" disabled={busy} onClick={() => void confirm()}>{userCopy.usePhoto}</Button>
      </div>
    </div>
  );
}
