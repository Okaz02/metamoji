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

describe("unitsToSend", () => {
  it("only looks at personal layers", () => {
    const doc = createDocument();
    const layer = doc.pages[0].layers[0];
    layer.layerType = "content";
    layer.units.push(createTextUnit(0, 0));
    expect(unitsToSend(doc)).toEqual([]);
  });

  it("skips ink and degraded placeholders", () => {
    const doc = createDocument();
    const layer = doc.pages[0].layers[0];
    layer.layerType = "system:personal";
    layer.units.push(createDrawUnit());
    expect(unitsToSend(doc)).toEqual([]);
  });

  it("sends $text as its own model, native", () => {
    const doc = createDocument();
    const layer = doc.pages[0].layers[0];
    layer.layerType = "system:personal";
    const text = createTextUnit(10, 20);
    text.text = "hello";
    layer.units.push(text);

    const [sent] = unitsToSend(doc);
    expect(sent).toMatchObject({ kind: "native", unitId: text.id, layerId: layer.id });
    if (sent.kind !== "native") throw new Error("expected native");
    expect(sent.models[0].modelType).toBe("$text");
    expect(sent.models[0].props.text).toBe("hello");
  });

  it("sends a plain rect or ellipse as a native shape element, not a picture", () => {
    // The room has a real drawing-engine element for these (`DrShapeElement`,
    // sharing a stroke's own pen-style model) — rasterising them would use a
    // different mechanism than the room's own for the same content.
    const doc = createDocument();
    const layer = doc.pages[0].layers[0];
    layer.layerType = "system:personal";
    const rect = createShapeUnit(5, 6, 70, 80, "rect");
    const ellipse = createShapeUnit(0, 0, 10, 10, "ellipse");
    layer.units.push(rect, ellipse);

    const [sentRect, sentEllipse] = unitsToSend(doc);
    expect(sentRect).toMatchObject({
      kind: "shape",
      unitId: rect.id,
      layerId: layer.id,
      shapeKind: "rect",
      x: 5, y: 6, width: 70, height: 80,
      strokeColor: rect.strokeColor,
      strokeWidth: rect.strokeWidth,
    });
    expect(sentEllipse).toMatchObject({ kind: "shape", shapeKind: "ellipse" });
  });

  it("falls back to a rasterised image for a filled shape or an unsupported kind", () => {
    const doc = createDocument();
    const layer = doc.pages[0].layers[0];
    layer.layerType = "system:personal";
    const filled = createShapeUnit(5, 6, 70, 80, "rect", "#000000", "#ff0000");
    const diamond = createShapeUnit(0, 0, 10, 10, "diamond");
    layer.units.push(filled, diamond);

    const sent = unitsToSend(doc);
    expect(sent.every((u) => u.kind === "image")).toBe(true);
    const tickets = sent.map((u) => (u.kind === "image" ? u.ticket : null));
    expect(new Set(tickets).size).toBe(2);
  });
});
