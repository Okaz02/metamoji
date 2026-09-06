import { describe, expect, it } from "vitest";

import { distanceToStroke, simplify, strokeBounds, strokeOutlineShapes, widthAt } from "./stroke";
import type { StrokeShape } from "./stroke";
import { screenToWorld, worldToScreen, zoomAbout, clampScale, fitRect } from "../render/viewport";
import type { InkPoint, PenAttributes, Stroke } from "./types";

/**
 * Nonzero-winding point-in-fill test, matching what `ctx.fill()` actually
 * computes for the subpaths `buildStrokePath` emits — NOT a naive "is this
 * point inside any shape" union. A union can't see two overlapping subpaths
 * wound in *opposite* directions quietly cancelling the fill between them
 * (winding +1 and -1 sum to zero — a hole — even though both shapes "contain"
 * the point). That is exactly the bug this once let through: the quads were
 * wound opposite to `ctx.arc`'s circles, so every sample point punched a hole
 * in the ink instead of adding to it. Only counting signed winding, the way
 * Canvas does, can catch that class of bug again.
 */
function isCovered(shapes: StrokeShape[], x: number, y: number): boolean {
  let winding = 0;
  for (const shape of shapes) {
    const poly = shape.kind === "circle" ? circlePolygon(shape) : shape.pts;
    winding += windingContribution(poly, x, y);
  }
  return winding !== 0;
}

/** Approximates `ctx.arc(cx, cy, r, 0, Math.PI * 2)` — increasing angle, the
 * same direction `buildStrokePath` actually draws circles in. */
function circlePolygon(circle: Extract<StrokeShape, { kind: "circle" }>): { x: number; y: number }[] {
  const segments = 64;
  const poly: { x: number; y: number }[] = [];
  for (let i = 0; i < segments; i++) {
    const t = (i / segments) * Math.PI * 2;
    poly.push({ x: circle.cx + circle.r * Math.cos(t), y: circle.cy + circle.r * Math.sin(t) });
  }
  return poly;
}

/** Signed edge-crossing count of a ray from `(x, y)` against one closed,
 * directed polygon — the standard nonzero winding-number algorithm. */
function windingContribution(poly: { x: number; y: number }[], x: number, y: number): number {
  let winding = 0;
  for (let i = 0; i < poly.length; i++) {
    const a = poly[i];
    const b = poly[(i + 1) % poly.length];
    const cross = (b.x - a.x) * (y - a.y) - (x - a.x) * (b.y - a.y);
    if (a.y <= y) {
      if (b.y > y && cross > 0) winding += 1;
    } else if (b.y <= y && cross < 0) {
      winding -= 1;
    }
  }
  return winding;
}

/**
 * Scans the stroke's bounding box for a row that is covered, then not, then
 * covered again — an interior hole, as opposed to just the stroke's own
 * outer edge. Point-sampling a specific spot can land exactly on a shape's
 * boundary (see `isCovered`'s own caveats); scanning whole rows does not
 * depend on guessing where the trouble spot is.
 */
function hasInteriorGap(shapes: StrokeShape[], points: InkPoint[]): boolean {
  const step = 0.5;
  const minX = Math.min(...points.map((p) => p.x)) - 8;
  const maxX = Math.max(...points.map((p) => p.x)) + 8;
  const minY = Math.min(...points.map((p) => p.y)) - 8;
  const maxY = Math.max(...points.map((p) => p.y)) + 8;

  for (let y = minY; y <= maxY; y += step) {
    let spans = 0;
    let wasCovered = false;
    for (let x = minX; x <= maxX; x += step) {
      const covered = isCovered(shapes, x, y);
      if (covered && !wasCovered) spans++;
      wasCovered = covered;
    }
    if (spans > 1) return true;
  }
  return false;
}

const pen: PenAttributes = {
  color: "#000000",
  width: 4,
  penType: "ballpoint",
  opacity: 1,
  pressureSensitivity: 1,
};

function stroke(points: InkPoint[]): Stroke {
  return { id: "s", points, pen, bounds: strokeBounds(points, pen.width) };
}

describe("stroke geometry", () => {
  it("bounds cover every point plus room for the stroke width", () => {
    const points: InkPoint[] = [
      { x: 10, y: 10, p: 0.5, t: 0 },
      { x: 50, y: 30, p: 0.5, t: 10 },
    ];
    const b = strokeBounds(points, 4);
    expect(b.x).toBeLessThan(10);
    expect(b.y).toBeLessThan(10);
    expect(b.x + b.width).toBeGreaterThan(50);
    expect(b.y + b.height).toBeGreaterThan(30);
  });

  it("bounds of an empty stroke are empty rather than infinite", () => {
    expect(strokeBounds([], 4)).toEqual({ x: 0, y: 0, width: 0, height: 0 });
  });

  it("width tracks pressure, and stops doing so at zero sensitivity", () => {
    expect(widthAt(pen, 1)).toBeGreaterThan(widthAt(pen, 0));
    const flat = { ...pen, pressureSensitivity: 0 };
    expect(widthAt(flat, 0)).toBe(widthAt(flat, 1));
  });

  it("a highlighter ignores pressure entirely", () => {
    const highlighter: PenAttributes = {
      ...pen,
      penType: "highlighter",
      pressureSensitivity: 1,
    };
    expect(widthAt(highlighter, 0)).toBe(highlighter.width);
    expect(widthAt(highlighter, 1)).toBe(highlighter.width);
  });

  it("simplify drops near-duplicate samples but keeps the endpoints", () => {
    const dense: InkPoint[] = Array.from({ length: 50 }, (_, i) => ({
      x: i * 0.1,
      y: 0,
      p: 0.5,
      t: i,
    }));
    const out = simplify(dense, 1);
    expect(out.length).toBeLessThan(dense.length);
    expect(out[0]).toEqual(dense[0]);
    expect(out[out.length - 1]).toEqual(dense[dense.length - 1]);
  });

  it("simplify leaves short strokes alone", () => {
    const two: InkPoint[] = [
      { x: 0, y: 0, p: 0.5, t: 0 },
      { x: 1, y: 1, p: 0.5, t: 1 },
    ];
    expect(simplify(two)).toEqual(two);
  });

  it("distance to a stroke is zero on the line and grows away from it", () => {
    const s = stroke([
      { x: 0, y: 0, p: 0.5, t: 0 },
      { x: 100, y: 0, p: 0.5, t: 10 },
    ]);
    expect(distanceToStroke(s, 50, 0)).toBeCloseTo(0);
    expect(distanceToStroke(s, 50, 10)).toBeCloseTo(10);
    // Past the end, distance is measured to the endpoint, not the infinite line.
    expect(distanceToStroke(s, 130, 0)).toBeCloseTo(30);
  });

  it("distance to an empty stroke is infinite rather than NaN", () => {
    expect(distanceToStroke(stroke([]), 0, 0)).toBe(Infinity);
  });

  it("fills solid when the pen loops back over itself tighter than its own width", () => {
    // A tiny scribble: the loop's radius (1) is smaller than the pen's half-width
    // (2 for this 4-wide pen), so the whole disc the tip sweeps should be covered,
    // including dead centre. The old offset-outline approach left a hole there
    // because the two offset sides crossed and the fill rule read it as empty.
    const points: InkPoint[] = [];
    for (let i = 0; i <= 120; i++) {
      const t = (i / 40) * Math.PI * 2;
      points.push({ x: 30 + 1 * Math.cos(t), y: 30 + 1 * Math.sin(t), p: 0.5, t: i });
    }
    const shapes = strokeOutlineShapes(stroke(points));
    expect(isCovered(shapes, 30, 30)).toBe(true);
  });

  it("a loop wider than the pen still leaves its centre unpainted", () => {
    // Sanity check for the test above: a loop bigger than the pen never sweeps
    // its own centre, so that hole is physically correct and must stay a hole.
    const points: InkPoint[] = [];
    for (let i = 0; i <= 120; i++) {
      const t = (i / 40) * Math.PI * 2;
      points.push({ x: 30 + 12 * Math.cos(t), y: 30 + 12 * Math.sin(t), p: 0.5, t: i });
    }
    const shapes = strokeOutlineShapes(stroke(points));
    expect(isCovered(shapes, 30, 30)).toBe(false);
  });

  it("fills solid along a plain stroke, at any angle — no per-sample holes", () => {
    // Regression for a real bug: the bridging quad was wound opposite to
    // `ctx.arc`'s circles, so nonzero-rule fill read every sample point as a
    // *cancellation* rather than a union — every one of them punched a
    // circular hole in an otherwise ordinary line. A plain, non-self-crossing
    // stroke should never have an interior hole, and that has to hold
    // regardless of which way the stroke happens to point (the bug was
    // direction-dependent: it only showed up for some quad orientations) —
    // and it only showed up for realistically-spaced samples: too dense a
    // test stroke, and neighbouring circles alone paper over a cancelled
    // quad, which is exactly why an earlier version of this test passed even
    // against the broken code. A sample spacing close to the pen's own
    // radius is what actually exercises the quad away from the centreline.
    const pen: PenAttributes = {
      color: "#000000",
      width: 10,
      penType: "ballpoint",
      opacity: 1,
      pressureSensitivity: 0,
    };
    for (let deg = 0; deg < 360; deg += 30) {
      const rad = (deg * Math.PI) / 180;
      const points: InkPoint[] = [];
      for (let i = 0; i <= 10; i++) {
        points.push({ x: 50 + i * 4 * Math.cos(rad), y: 50 + i * 4 * Math.sin(rad), p: 0.5, t: i });
      }
      const shapes = strokeOutlineShapes({ ...stroke(points), pen });
      expect(hasInteriorGap(shapes, points)).toBe(false);
    }
  });
});

describe("viewport", () => {
  it("screenToWorld inverts worldToScreen", () => {
    const vp = { scale: 2.5, tx: -130, ty: 64 };
    const screen = worldToScreen(vp, 42, 17);
    const world = screenToWorld(vp, screen.x, screen.y);
    expect(world.x).toBeCloseTo(42);
    expect(world.y).toBeCloseTo(17);
  });

  it("zooming about a point keeps that point fixed on screen", () => {
    const vp = { scale: 1, tx: 20, ty: 30 };
    const anchor = { x: 400, y: 300 };
    const before = screenToWorld(vp, anchor.x, anchor.y);

    const zoomed = zoomAbout(vp, anchor.x, anchor.y, 1.8);
    const after = screenToWorld(zoomed, anchor.x, anchor.y);

    expect(after.x).toBeCloseTo(before.x);
    expect(after.y).toBeCloseTo(before.y);
  });

  it("scale is clamped to a usable range", () => {
    expect(clampScale(1000)).toBeLessThanOrEqual(8);
    expect(clampScale(0.0001)).toBeGreaterThanOrEqual(0.1);
  });

  it("fitRect centres the rect within the view", () => {
    const vp = fitRect({ x: 0, y: 0, width: 1000, height: 500 }, 800, 600, 0);
    const topLeft = worldToScreen(vp, 0, 0);
    const bottomRight = worldToScreen(vp, 1000, 500);

    // Equal margins on both axes means it is centred.
    expect(topLeft.x).toBeCloseTo(800 - bottomRight.x);
    expect(topLeft.y).toBeCloseTo(600 - bottomRight.y);
  });
});
