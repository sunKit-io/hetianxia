/** 和 crates/core/src/osinfo.rs 的文案保持一致。样张 16a / 17b。 */

export const VERIFIED_LINUX = ["ubuntu", "debian", "centos", "rhel", "rocky", "almalinux"] as const;

export type RemoteOs =
  | { kind: "macos" }
  | { kind: "linux"; id: string; verified: boolean }
  | { kind: "unidentified" };

export function classify(osRelease: string | null, plist: string | null): RemoteOs {
  if (plist != null) return { kind: "macos" };
  if (osRelease != null) {
    const id = osReleaseId(osRelease) ?? "";
    const verified = VERIFIED_LINUX.some((v) => v === id.toLowerCase());
    return { kind: "linux", id, verified };
  }
  return { kind: "unidentified" };
}

function osReleaseId(text: string): string | null {
  for (const line of text.split("\n")) {
    const trimmed = line.trim();
    if (!trimmed.startsWith("ID=")) continue;
    const id = trimmed.slice(3).trim().replace(/^["']|["']$/g, "");
    if (id) return id;
  }
  return null;
}

export function connectionSubtitle(os: RemoteOs): string {
  if (os.kind === "unidentified") return "未识别";
  if (os.kind === "macos") return "macOS";
  if (os.verified) return pretty(os.id);
  return `Linux / ${os.id || "unknown"} · 未验证的系统`;
}

export function systemTypeLabel(os: RemoteOs): string {
  if (os.kind === "unidentified") return "未识别";
  if (os.kind === "macos") return "macOS";
  if (os.verified) return pretty(os.id);
  return `未验证的系统（Linux / ${os.id || "unknown"}）`;
}

export function warningBanner(os: RemoteOs): string | null {
  if (os.kind === "linux" && !os.verified) {
    const id = os.id || "unknown";
    return `这台服务器的 Linux 发行版（${id}）未经验证。你可以继续使用，但部分功能可能不正常。`;
  }
  if (os.kind === "unidentified") {
    return "未验证的系统：没能识别这台服务器的系统，第一版只验证了 Linux 和 macOS。你可以继续使用，但部分功能可能不正常。";
  }
  return null;
}

export function canContinue(_os: RemoteOs): boolean {
  return true;
}

function pretty(id: string): string {
  switch (id.toLowerCase()) {
    case "ubuntu":
      return "Ubuntu";
    case "debian":
      return "Debian";
    case "centos":
      return "CentOS";
    case "rhel":
      return "RHEL";
    case "rocky":
      return "Rocky Linux";
    case "almalinux":
      return "AlmaLinux";
    default:
      return id || "Linux";
  }
}

export const ALPINE = classify('ID="alpine"\n', null);
export const UNKNOWN = classify(null, null);
export const UBUNTU = classify("ID=ubuntu\n", null);
