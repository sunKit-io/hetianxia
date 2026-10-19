/** 两边编辑器的滚动对齐。锚点分段线性映射，再用一帧屏幕坐标修正。 */

export type Anchor = { left: number; right: number };

export type Hunk = {
  leftStart: number;
  leftEnd: number;
  rightStart: number;
  rightEnd: number;
};

export function mapDocY(anchors: Anchor[], y: number): number {
  if (anchors.length === 0) return y;
  if (y <= anchors[0].left) return anchors[0].right;
  const last = anchors[anchors.length - 1];
  if (y >= last.left) return last.right + (y - last.left);
  let lo = 0;
  let hi = anchors.length - 1;
  while (lo + 1 < hi) {
    const mid = (lo + hi) >> 1;
    if (anchors[mid].left <= y) lo = mid;
    else hi = mid;
  }
  const a = anchors[lo];
  const b = anchors[hi];
  const span = b.left - a.left;
  const t = span === 0 ? 0 : (y - a.left) / span;
  return a.right + t * (b.right - a.right);
}

/** 修正量 = (右栏屏幕 Y - 左栏屏幕 Y) / scale。 */
export function screenCorrection(rightScreenY: number, leftScreenY: number, scale: number): number {
  const z = scale || 1;
  return (rightScreenY - leftScreenY) / z;
}

export function lineHunks(left: string, right: string): Hunk[] {
  const a = left.split("\n");
  const b = right.split("\n");
  const n = a.length;
  const m = b.length;
  const dp: number[][] = Array.from({ length: n + 1 }, () => Array(m + 1).fill(0));
  for (let i = n - 1; i >= 0; i--) {
    for (let j = m - 1; j >= 0; j--) {
      dp[i][j] = a[i] === b[j] ? dp[i + 1][j + 1] + 1 : Math.max(dp[i + 1][j], dp[i][j + 1]);
    }
  }
  const hunks: Hunk[] = [];
  let i = 0;
  let j = 0;
  let open: Hunk | null = null;
  const flush = () => {
    if (open) hunks.push(open);
    open = null;
  };
  const touch = () => {
    if (!open) {
      open = { leftStart: i + 1, leftEnd: i + 1, rightStart: j + 1, rightEnd: j + 1 };
    }
  };
  while (i < n && j < m) {
    if (a[i] === b[j]) {
      flush();
      i += 1;
      j += 1;
      continue;
    }
    touch();
    if (dp[i + 1][j] >= dp[i][j + 1]) {
      open!.leftEnd = i + 2;
      i += 1;
    } else {
      open!.rightEnd = j + 2;
      j += 1;
    }
  }
  while (i < n) {
    touch();
    open!.leftEnd = i + 2;
    i += 1;
  }
  while (j < m) {
    touch();
    open!.rightEnd = j + 2;
    j += 1;
  }
  flush();
  return hunks;
}
