/** 连接表单里的远程目录。空、不是绝对路径、或规范化后是 `/` 都不能保存。 */
export function remoteRootFieldError(raw: string | null | undefined): string | null {
  const value = (raw ?? "").trim();
  if (!value) return "先在连接里填远程目录";
  if (!value.startsWith("/")) return "远程目录必须是绝对路径";
  if (posixNormalizesToRoot(value)) return "远程目录不能是根目录 /";
  return null;
}

function posixNormalizesToRoot(raw: string): boolean {
  const parts: string[] = [];
  for (const part of raw.split("/")) {
    if (!part || part === ".") continue;
    if (part === "..") {
      parts.pop();
      continue;
    }
    parts.push(part);
  }
  return parts.length === 0;
}
