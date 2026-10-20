/** 远程目录浏览器。路径从连接的远程目录出发，不能用 `..` 走出去。 */

export function normalizeBrowse(path: string): string | null {
  const value = path.trim();
  if (!value.startsWith("/") || value.includes("\\") || value.includes("\0")) return null;
  const parts: string[] = [];
  for (const part of value.split("/")) {
    if (part === "" || part === ".") continue;
    if (part === "..") return null;
    parts.push(part);
  }
  if (parts.length === 0) return null;
  return `/${parts.join("/")}`;
}

export function insideRoot(root: string, path: string): string | null {
  const base = normalizeBrowse(root);
  const here = normalizeBrowse(path);
  if (!base || !here) return null;
  if (here === base || here.startsWith(`${base}/`)) return here;
  return null;
}

export function browseCrumbs(root: string, current: string): { name: string; path: string }[] {
  const here = insideRoot(root, current);
  const base = normalizeBrowse(root);
  if (!here || !base) return [];
  const out = [{ name: base, path: base }];
  if (here === base) return out;
  let path = base;
  for (const name of here.slice(base.length + 1).split("/")) {
    path = `${path}/${name}`;
    out.push({ name, path });
  }
  return out;
}

export function joinChild(root: string, current: string, name: string): string | null {
  if (!name || name === "." || name === ".." || name.includes("/") || name.includes("\\") || name.includes("\0")) return null;
  const here = insideRoot(root, current);
  if (!here) return null;
  return insideRoot(root, `${here}/${name}`);
}
