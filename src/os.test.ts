import assert from "node:assert/strict";
import test from "node:test";
import { UNKNOWN, warningBanner } from "./os.ts";

test("unidentified banner matches mockup 17b", () => {
  const banner = warningBanner(UNKNOWN);
  assert.equal(
    banner,
    "未验证的系统：没能识别这台服务器的系统，第一版只验证了 Linux 和 macOS。你可以继续使用，但部分功能可能不正常。",
  );
  assert.equal(banner?.includes("识别出"), false);
});
