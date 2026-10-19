import type { ReactNode } from "react";

export function WindowFrame({
  title,
  subtitle,
  theme = "light",
  tools,
  children,
}: {
  title: string;
  subtitle: string;
  theme?: "light" | "dark";
  tools?: ReactNode;
  children: ReactNode;
}) {
  return (
    <div className="desktop" data-theme={theme}>
      <div className="window" data-theme={theme}>
        <header className="titlebar">
          <span className="lights" aria-hidden="true">
            <i /><i /><i />
          </span>
          <div className="titleblock">
            <strong>{title}</strong>
            <span>{subtitle}</span>
          </div>
          <div className="titletools">{tools}</div>
        </header>
        <div className="winbody">{children}</div>
      </div>
    </div>
  );
}
