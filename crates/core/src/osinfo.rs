//! 远程系统识别。只看 SFTP 读到的文件，绝不执行 `uname`。三种结果都可以继续用。
//!
//! 判断顺序（命中即停）：
//! 1. 读得到 `SystemVersion.plist` → macOS，不告警。
//! 2. 否则读 `/etc/os-release`，读不到再读 `/usr/lib/os-release`。
//!    `ID` 是 ubuntu、debian、centos、rhel、rocky、almalinux 之一 → Linux，不告警。
//!    其他 ID（例如 alpine）→ 告警「未验证的系统（Linux / <ID>）」。
//! 3. 这些文件都读不到 → 系统类型只显示「未识别」，并提示「未验证的系统」。

pub const OS_RELEASE_PATH: &str = "/etc/os-release";
pub const OS_RELEASE_FALLBACK_PATH: &str = "/usr/lib/os-release";
pub const MACOS_PLIST_PATH: &str = "/System/Library/CoreServices/SystemVersion.plist";

/// 界面上系统类型这一行，识别失败时只能是这四个字，不带系统名，也不解释读了哪个文件。
pub const UNIDENTIFIED_LABEL: &str = "未识别";
pub const UNVERIFIED_NOTICE: &str = "未验证的系统";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RemoteOs {
    MacOs { product_version: Option<String> },
    Linux { id: String, verified: bool },
    Unidentified,
}

const VERIFIED_LINUX: &[&str] = &["ubuntu", "debian", "centos", "rhel", "rocky", "almalinux"];

/// `plist` 优先。`os_release` 为 `None` 表示 `/etc/os-release` 和 `/usr/lib/os-release` 都读不到。
pub fn classify(os_release: Option<&str>, plist: Option<&str>) -> RemoteOs {
    if let Some(text) = plist {
        return RemoteOs::MacOs { product_version: plist_version(text) };
    }
    if let Some(text) = os_release {
        let id = os_release_id(text).unwrap_or_default();
        let verified = VERIFIED_LINUX.iter().any(|v| v.eq_ignore_ascii_case(id.trim()));
        return RemoteOs::Linux { id, verified };
    }
    RemoteOs::Unidentified
}

/// 先 plist，再 `/etc/os-release`，再 `/usr/lib/os-release`。
pub fn classify_files(plist: Option<&str>, etc_os_release: Option<&str>, usr_lib_os_release: Option<&str>) -> RemoteOs {
    classify(etc_os_release.or(usr_lib_os_release), plist)
}

/// 标题旁的短状态。样张 16a：`Linux / alpine · 未验证的系统`。样张 17b：`未识别`。
pub fn connection_subtitle(os: &RemoteOs) -> String {
    match os {
        RemoteOs::Unidentified => UNIDENTIFIED_LABEL.to_string(),
        RemoteOs::MacOs { .. } => "macOS".to_string(),
        RemoteOs::Linux { id, verified: true } => linux_label(id),
        RemoteOs::Linux { id, verified: false } => format!("{} · {UNVERIFIED_NOTICE}", linux_slash(id)),
    }
}

/// 系统类型这一行。16a 写「未验证的系统（Linux / alpine）」。17b 只写「未识别」。
pub fn system_type_label(os: &RemoteOs) -> String {
    match os {
        RemoteOs::Unidentified => UNIDENTIFIED_LABEL.to_string(),
        RemoteOs::MacOs { .. } => "macOS".to_string(),
        RemoteOs::Linux { id, verified: true } => linux_label(id),
        RemoteOs::Linux { id, verified: false } => format!("未验证的系统（{}）", linux_slash(id)),
    }
}

/// 橙色提示。未验证发行版说明可以继续用；两边文件都读不到时，句子以「未验证的系统：」开头。
pub fn warning_banner(os: &RemoteOs) -> Option<String> {
    match os {
        RemoteOs::Linux { id, verified: false } => {
            let id = shown_id(id);
            Some(format!("这台服务器的 Linux 发行版（{id}）未经验证。你可以继续使用，但部分功能可能不正常。"))
        }
        RemoteOs::Unidentified => Some(
            "未验证的系统：没能识别出这台服务器的系统，第一版只验证了 Linux 和 macOS。你可以继续使用，但部分功能可能不正常。".into(),
        ),
        _ => None,
    }
}

fn shown_id(id: &str) -> &str {
    if id.is_empty() { "unknown" } else { id }
}

fn linux_slash(id: &str) -> String {
    format!("Linux / {}", shown_id(id))
}

pub fn show_unverified_notice(os: &RemoteOs) -> bool {
    match os {
        RemoteOs::Unidentified => true,
        RemoteOs::Linux { verified, .. } => !verified,
        RemoteOs::MacOs { .. } => false,
    }
}

pub fn unverified_notice(os: &RemoteOs) -> Option<String> {
    warning_banner(os)
}

pub fn can_continue(_os: &RemoteOs) -> bool {
    true
}

fn linux_label(id: &str) -> String {
    match id.to_ascii_lowercase().as_str() {
        "ubuntu" => "Ubuntu".into(),
        "debian" => "Debian".into(),
        "centos" => "CentOS".into(),
        "rhel" => "RHEL".into(),
        "rocky" => "Rocky Linux".into(),
        "almalinux" => "AlmaLinux".into(),
        "" => "Linux".into(),
        other => other.to_string(),
    }
}

fn os_release_id(text: &str) -> Option<String> {
    for line in text.lines() {
        let line = line.trim();
        let Some(rest) = line.strip_prefix("ID=") else { continue };
        let rest = rest.trim().trim_matches('"').trim_matches('\'');
        if !rest.is_empty() {
            return Some(rest.to_string());
        }
    }
    None
}

fn plist_version(text: &str) -> Option<String> {
    let key = "<key>ProductVersion</key>";
    let idx = text.find(key)?;
    let after = &text[idx + key.len()..];
    let start = after.find("<string>")? + "<string>".len();
    let end = after[start..].find("</string>")? + start;
    let v = after[start..end].trim();
    if v.is_empty() { None } else { Some(v.to_string()) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unidentified_label_is_only_those_two_characters() {
        let os = classify(None, None);
        assert_eq!(os, RemoteOs::Unidentified);
        let label = system_type_label(&os);
        assert_eq!(label, "未识别");
        assert_eq!(connection_subtitle(&os), "未识别");
        assert!(!label.contains("os-release"));
        assert!(!label.contains("Linux"));
        let banner = warning_banner(&os).unwrap();
        assert!(banner.starts_with("未验证的系统："), "{banner}");
        assert!(can_continue(&os));
    }

    #[test]
    fn plist_is_macos_and_verified_linux_ids() {
        let mac = classify(Some("ID=ubuntu\n"), Some("<key>ProductVersion</key><string>14.6</string>"));
        assert!(matches!(mac, RemoteOs::MacOs { .. }));
        assert_eq!(system_type_label(&mac), "macOS");
        assert!(!show_unverified_notice(&mac));

        let ubuntu = classify(Some("NAME=\"Ubuntu\"\nID=ubuntu\n"), None);
        assert_eq!(system_type_label(&ubuntu), "Ubuntu");
        assert!(!show_unverified_notice(&ubuntu));

        for id in ["ubuntu", "debian", "centos", "rhel", "rocky", "almalinux"] {
            let os = classify(Some(&format!("ID={id}\n")), None);
            assert!(!show_unverified_notice(&os), "{id}");
            assert!(unverified_notice(&os).is_none());
            assert!(can_continue(&os));
        }

        let alpine = classify_files(None, None, Some("ID=alpine\n"));
        assert!(matches!(alpine, RemoteOs::Linux { verified: false, .. }));
        assert_eq!(connection_subtitle(&alpine), "Linux / alpine · 未验证的系统");
        assert_eq!(system_type_label(&alpine), "未验证的系统（Linux / alpine）");
        let banner = warning_banner(&alpine).unwrap();
        assert!(banner.contains("alpine"));
        assert!(banner.contains("可以继续使用"));
        assert!(can_continue(&alpine));
    }
}
