# Draw on a photo before sending

Status: proposed, 2026-08-11. Scope is deliberately one feature: marking up a
photo *before it is sent*. Annotating a photo you already received is out of
scope here.

## Why

The reason a family member reaches for this on a ship is almost always the
same: circle a spot on a deck plan or a screenshot of the daily schedule and
send it as "meet me here." Typing "aft, deck 15, near the pool bar" is worse
than a circle. It has to be reachable in two taps from the photo they just
picked, and it has to work with a thumb on a moving ship.

## Surface

The photo already stages in the composer as `PendingPhotoCard` (both 1:1 and
group chat, from both the gallery picker and the camera). That card is the
only entry point:

1. User picks or shoots a photo. It stages exactly as it does today.
2. The staged card gains one affordance: **Draw**. Tapping the card's draw
   control opens the markup editor full-screen over the chat.
3. In the editor the user draws, then confirms or cancels.
   - **Confirm** replaces the staged photo with the annotated version and
     returns to the chat, caption and reply-target untouched.
   - **Cancel** discards the strokes and leaves the staged photo as it was.
4. Sending is unchanged — the annotated JPEG rides the existing attachment
   path.

The editor is not a separate step in the send flow. A user who never taps
Draw sees exactly today's behavior, unchanged.

## The editor

Deliberately small. This is a marker, not an art tool.

- **Freehand pen only.** No shapes, no text, no arrows, no stickers, no
  cropping, no rotation.
- **Colors:** four fixed swatches, chosen to stay visible on both bright
  photos and dark ones. Red is the default because circling is the dominant
  use.
- **Thickness:** one control with three steps (thin / medium / thick),
  defaulting to medium. Stroke width scales with the image's displayed size
  so a thick stroke looks the same on any photo.
- **Undo:** removes the last complete stroke. Repeatable back to a clean
  image. No redo.
- **Clear:** removes all strokes, with no confirmation prompt — undo is not
  needed after clear because clear is itself undoable by cancelling out.
- **Confirm / Cancel** as described above.

Touch targets follow the app's existing minimums. The controls sit clear of
the drawing surface so a stroke near the edge of the photo doesn't hit a
button.

## Image handling

The staged bytes are already a compressed JPEG (`MediaCompressor` runs before
staging). The editor therefore:

1. Decodes the staged JPEG.
2. Composites the strokes onto it at the **decoded image's** resolution, not
   the screen's — strokes are captured in view coordinates and mapped back to
   image coordinates, so annotating on a small phone screen does not produce
   a small-resolution photo.
3. Re-encodes through the same compression path used for staging, so quality
   and size behave identically to an unannotated photo.
4. **Re-runs the staging size guard.** Strokes can grow the encoded size, and
   an annotated photo that no longer fits must be rejected with the same
   plain-speech warning an oversized photo gets today, not sent silently or
   crashed on.

No EXIF or orientation regression: a photo that displays right side up today
must display right side up after a round trip through the editor with zero
strokes.

## What this does NOT touch

- **No core change.** No Rust, no UniFFI surface change, no bindgen
  regeneration. This is entirely shell UI over an existing attachment path.
- **No wire-format change.** An annotated photo is a photo. Older versions
  receive and display it with no awareness the feature exists, and no
  capability bit is needed.
- No change to the received-photo viewer.
- No change to voice memos, captions, replies, or group fan-out.

## Copy

All user-facing strings go in `strings.xml` / `Localizable.xcstrings` per the
localization gate — no hardcoded literals, sentence case, no jargon. The
control is called **Draw**, not "markup," "annotate," or "edit."

## Acceptance

- Pick a photo, tap Draw, scribble, confirm: the staged card shows the
  annotated image and sending delivers it.
- Cancel after drawing: the staged photo is byte-identical to before.
- Confirm with no strokes: the image is visually unchanged and still upright.
- Undo removes exactly one stroke at a time; clear empties the canvas.
- Works identically from the gallery picker and the camera, in both 1:1 and
  group chats.
- An annotated photo that exceeds the size limit warns rather than failing
  silently.
