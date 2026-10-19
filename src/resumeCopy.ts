/** 设计 21 的行标签和标题。和核心里的 DETAIL_MODIFIED / resume_conflict_title 同一套文案。 */

export const DETAIL_MODIFIED = "升级后被修改";
export const DETAIL_DELETED = "升级后被删除";

export type ResumeFile = { path: string; detail: string };

export function resumeConflictTitle(files: ResumeFile[]): string {
  const n = files.length;
  if (files.some((file) => file.detail === DETAIL_DELETED)) {
    return `有 ${n} 个已经写完的文件被改过或删除`;
  }
  return `有 ${n} 个已经写完的文件被改过`;
}
