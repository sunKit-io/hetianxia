import assert from "node:assert/strict";
import test from "node:test";
import { lineHunks, mapDocY, screenCorrection } from "./syncScroll.ts";

test("mapDocY interpolates between anchors and correction is one frame of screen delta", () => {
  const anchors = [
    { left: 0, right: 0 },
    { left: 100, right: 140 },
    { left: 200, right: 180 },
  ];
  assert.equal(mapDocY(anchors, 0), 0);
  assert.equal(mapDocY(anchors, 50), 70);
  assert.equal(mapDocY(anchors, 200), 180);
  assert.equal(mapDocY(anchors, 220), 200);
  assert.equal(screenCorrection(128, 100, 2), 14);
  assert.equal(screenCorrection(100, 100, 1), 0);
});

test("line hunks cover a replacement and an insertion", () => {
  const left = ["a", "old", "c"].join("\n");
  const right = ["a", "new", "c", "extra"].join("\n");
  const hunks = lineHunks(left, right);
  assert.equal(hunks.length, 2);
  assert.deepEqual(hunks[0], { leftStart: 2, leftEnd: 3, rightStart: 2, rightEnd: 3 });
  assert.deepEqual(hunks[1], { leftStart: 4, leftEnd: 4, rightStart: 4, rightEnd: 5 });
});
