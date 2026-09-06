import { beforeEach, describe, expect, it, vi } from "vitest";

import { unitsToSend } from "./sendUnits";
import { createDocument, createDrawUnit, createShapeUnit, createTextUnit } from "../model/factory";

/** jsdom has no 2D canvas backend; `rasterizeUnit` only needs a stable fake. */
function stubCanvas() {
  vi.spyOn(HTMLCanvasElement.prototype, "getContext").mockReturnValue({
    scale: () => {},
    translate: () => {},
    save: () => {},
    restore: () => {},
    fillRect: () => {},
    fillText: () => {},
    beginPath: () => {},
    fill: () => {},
    stroke: () => {},
    measureText: () => ({ width: 0 }),
  } as unknown as CanvasRenderingContext2D);
  vi.spyOn(HTMLCanvasElement.prototype, "toDataURL").mockReturnValue("data:image/png;base64,AQID");
}

/** jsdom has no 2D canvas backend, so it never defines the Path2D global either. */
class StubPath2D {
  moveTo(): void {}
  lineTo(): void {}
  arc(): void {}
  arcTo(): void {}
  ellipse(): void {}
  quadraticCurveTo(): void {}
  bezierCurveTo(): void {}
  rect(): void {}
  closePath(): void {}
}

beforeEach(() => {
  stubCanvas();
  vi.stubGlobal("Path2D", StubPath2D);
});

/** The classroom's own name for a layer, as the importer parks it. */
const BOOTH = "P1_[layer-forUser]_213163099101";

/**
 * A personal layer the room actually has a booth for.
 *
 * The parked `layerId` is the point of the fixture: on a note taken from a
 * class box it is what addresses the room, and it is not the same string as
 * the layer's `id`.
 */
function personalLayer(doc: ReturnType<typeof createDocument>, booth: string = BOOTH) {
  const layer = doc.pages[0].layers[0];
  layer.layerType = "system:personal";
  (layer as unknown as { _extra: Record<string, unknown> })._extra = { layerId: booth };
  return layer;
}

describe("unitsToSend", () => {
  it("only looks at personal layers", () => {
    const doc = createDocument();
    const layer = personalLayer(doc);
    layer.layerType = "content";
    layer.units.push(createTextUnit(0, 0));
    expect(unitsToSend(doc)).toEqual([]);
  });

  it("skips ink and degraded placeholders", () => {
    const doc = createDocument();
    const layer = personalLayer(doc);
    layer.units.push(createDrawUnit());
    expect(unitsToSend(doc)).toEqual([]);
  });

  it("addresses the layer by the classroom's name for it, not this app's", () => {
    // The regression this fixture exists for. `layer.id` is a model id; the
    // room knows the layer by the parked `layerId`, and a Direction sent to
    // the wrong one is accepted, recorded as sent, and never displayed. It is
    // why a student's shapes never reached the class while their ink did.
    const doc = createDocument();
    const layer = personalLayer(doc);
    layer.units.push(createTextUnit(0, 0));

    const [sent] = unitsToSend(doc);
    expect(sent.layerId).toBe(BOOTH);
    expect(sent.layerId).not.toBe(layer.id);
  });

  it("does not send from a layer the room has no booth for", () => {
    // A note taken by a build that overwrote the classroom's names with its
    // own model ids. Posting to that name reaches nobody and looks, from the
    // outside, exactly like sending working.
    const doc = createDocument();
    const layer = personalLayer(doc, "note_abc_l___subId_v2__x___page__4__layer_forUser__9");
    layer.units.push(createTextUnit(0, 0));
    expect(unitsToSend(doc)).toEqual([]);
  });

  it("sends $text as its own model, native", () => {
    const doc = createDocument();
    const layer = personalLayer(doc);
    const text = createTextUnit(10, 20);
    text.text = "hello";
    layer.units.push(text);

    const [sent] = unitsToSend(doc);
    expect(sent).toMatchObject({ kind: "native", unitId: text.id, layerId: BOOTH });
    if (sent.kind !== "native") throw new Error("expected native");
    expect(sent.models[0].modelType).toBe("$text");
    expect(sent.models[0].props.text).toBe("hello");
  });

  it("sends a plain rect or ellipse as a native shape element, not a picture", () => {
    // The room has a real drawing-engine element for these (`DrShapeElement`,
    // sharing a stroke's own pen-style model) — rasterising them would use a
    // different mechanism than the room's own for the same content.
    const doc = createDocument();
    const layer = personalLayer(doc);
    const rect = createShapeUnit(5, 6, 70, 80, "rect");
    const ellipse = createShapeUnit(0, 0, 10, 10, "ellipse");
    layer.units.push(rect, ellipse);

    const [sentRect, sentEllipse] = unitsToSend(doc);
    expect(sentRect).toMatchObject({
      kind: "shape",
      unitId: rect.id,
      layerId: BOOTH,
      shapeKind: "rect",
      x: 5, y: 6, width: 70, height: 80,
      strokeColor: rect.strokeColor,
      strokeWidth: rect.strokeWidth,
    });
    expect(sentEllipse).toMatchObject({ kind: "shape", shapeKind: "ellipse" });
  });

  it("falls back to a rasterised image for a filled shape or an unsupported kind", () => {
    const doc = createDocument();
    const layer = personalLayer(doc);
    const filled = createShapeUnit(5, 6, 70, 80, "rect", "#000000", "#ff0000");
    const diamond = createShapeUnit(0, 0, 10, 10, "diamond");
    layer.units.push(filled, diamond);

    const sent = unitsToSend(doc);
    expect(sent.every((u) => u.kind === "image")).toBe(true);
    const tickets = sent.map((u) => (u.kind === "image" ? u.ticket : null));
    expect(new Set(tickets).size).toBe(2);
  });
});
