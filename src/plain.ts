import { createElement, type ReactNode } from "react";

/** 文件内容、路径和名称只作为文本节点，不解析成 HTML。 */
export function Text({ value }: { value: string }): ReactNode {
  return createElement("span", null, value);
}
