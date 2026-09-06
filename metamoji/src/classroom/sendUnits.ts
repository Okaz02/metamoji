/**
 * What to send to the classroom for the note's non-ink content.
 *
 * `classbox_send_strokes` (Rust) already covers `$draw` on its own — it
 * re-reads the saved file and diffs it against a ledger. It cannot do the
 * same for everything else: the room only has a native wire format for
 * `$text` (`collabo/apply.rs`'s own vocabulary comment names
 * `$text`/`$image`/`$web`), and turning anything else into something the room
 * *can* show — a picture of it — needs a Canvas, which only exists on this
 * side of the IPC boundary. So this side decides what needs sending, and
 * passes the result into the *same* `classboxSendStrokes` call as the ink —
 * Rust skips whichever units it has already sent and posts the rest alongside
 * the strokes, in one connection rather than a second one right after.
 *
 * `$shape` gets the same treatment, but through a different native path:
 * the room's drawing engine has its own shape element (`DrShapeElement`),
 * sharing a stroke's own pen-style model for outline colour and width — see
 * `collabo/send.rs`'s `add_shape`. Only a plain, unfilled rectangle or
 * ellipse maps to it with any confidence; a filled shape, or any other
 * `ShapeKind`, falls back to a picture the same as `$image`/`$bgimage`/`$pdf`
 * always do — their `imageTicket` names a ticket in this app's own local
 * asset store, which means nothing in the room's shared-attachment space, so
 * there is no cheaper way to share their pixels than repainting them.
 */

import { newId } from "../model/ids";
import { unitToGenericModel } from "../model/converter";
import { rasterizeUnit } from "../render/renderer";
import type { AssetResolver } from "../render/renderer";
import type { NoteDocument, Unit } from "../model/types";
import type { UnitToSend } from "../ipc/api";

/** Personal-layer units the room already understands as their own model. */
const NATIVE_TYPES: ReadonlySet<Unit["type"]> = new Set(["$text"]);

/** Never worth sending: ink has its own path, and a broken placeholder is
 * this app's own bookkeeping, not something to hand another audience. */
const SKIP_TYPES: ReadonlySet<Unit["type"]> = new Set(["$draw", "$dummy"]);

/** `DrShapeType` kinds this build can send with confidence about their
 * geometry — see `collabo/send.rs::shape_type_for_kind`. */
const NATIVE_SHAPE_KINDS: ReadonlySet<string> = new Set(["rect", "ellipse"]);

/**
 * `assets` is the editor's own asset cache. Without it a unit whose picture
 * lives in the note's store — `$image`, `$bgimage`, `$pdf` — rasterises to an
 * empty rectangle, and the class is shown a blank where the picture should be.
 */
export function unitsToSend(doc: NoteDocument, assets?: AssetResolver): UnitToSend[] {
  const out: UnitToSend[] = [];
  for (const page of doc.pages) {
    for (const layer of page.layers) {
      if (layer.layerType !== "system:personal") continue;
      for (const unit of layer.units) {
        if (SKIP_TYPES.has(unit.type)) continue;
        out.push(unitToSend(unit, layer.id, assets));
      }
    }
  }
  return out;
}

function unitToSend(unit: Unit, layerId: string, assets?: AssetResolver): UnitToSend {
  if (NATIVE_TYPES.has(unit.type)) return nativeUnit(unit, layerId);
  if (unit.type === "$shape" && NATIVE_SHAPE_KINDS.has(unit.shape) && !unit.fillColor) {
    return shapeUnit(unit, layerId);
  }
  return rasterUnit(unit, layerId, assets);
}

function nativeUnit(unit: Unit, layerId: string): UnitToSend {
  return {
    kind: "native",
    unitId: unit.id,
    layerId,
    models: [unitToGenericModel(unit)],
  };
}

function shapeUnit(
  unit: Extract<Unit, { type: "$shape" }>,
  layerId: string,
): UnitToSend {
  return {
    kind: "shape",
    unitId: unit.id,
    layerId,
    shapeKind: unit.shape,
    x: unit.x,
    y: unit.y,
    width: unit.width,
    height: unit.height,
    strokeColor: unit.strokeColor,
    strokeWidth: unit.strokeWidth,
  };
}

function rasterUnit(unit: Unit, layerId: string, assets?: AssetResolver): UnitToSend {
  const dataUrl = rasterizeUnit(unit, assets);
  const pngBase64 = dataUrl.slice(dataUrl.indexOf(",") + 1);
  return {
    kind: "image",
    unitId: unit.id,
    layerId,
    ticket: newId("roomimg"),
    mime: "image/png",
    pngBase64,
    x: unit.x,
    y: unit.y,
    width: unit.width,
    height: unit.height,
  };
}
