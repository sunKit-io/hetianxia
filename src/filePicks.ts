/** 勾选状态按相对路径记「没勾上的那些」。新扫出来、不在这份名单里的文件默认勾上。 */

export function slashPath(path: string): string {
  return path.replace(/\\/g, "/");
}

export function skippedOf(paths: readonly string[], picked: ReadonlySet<string>): string[] {
  return paths.filter((path) => !picked.has(path));
}

export function pickedFromSkipped(paths: readonly string[], skipped: readonly string[]): Set<string> {
  const skip = new Set(skipped);
  return new Set(paths.filter((path) => !skip.has(path)));
}

/** 反选只翻转文件行。目录行的勾选由下面的文件推出来。 */
export function invertPicked(paths: readonly string[], picked: ReadonlySet<string>): Set<string> {
  const next = new Set<string>();
  for (const path of paths) {
    if (!picked.has(path)) next.add(path);
  }
  return next;
}
