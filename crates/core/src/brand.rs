//! 产品名和 bundle id 的唯一 Rust 常量。
//! `src-tauri/tauri.conf.json` 的 productName / identifier 必须与这里一致，
//! 有测试核对。改 bundle id 时两处一起改。

pub const PRODUCT_NAME: &str = "HeTianXia";
pub const DISPLAY_NAME: &str = "合天下";
pub const BUNDLE_ID: &str = "com.hetianxia.app";

/// 命名集中在这里。产品还没发布过，不做旧目录迁移。
/// 钥匙串服务名已确认。账号名是连接 id，不再使用 `hetianxia:<id>` 这种拼法。
pub const KEYCHAIN_SERVICE: &str = "com.hetianxia.app.ssh";
pub const TEMP_TAG: &str = ".htx-";
pub const REMOTE_BACKUP_DIR: &str = ".hetianxia-backups";
pub const LOCAL_BACKUP_DIR: &str = "backups";

pub fn keychain_account(connection_id: &str) -> String {
    connection_id.to_string()
}

pub fn local_data_dir(home: &std::path::Path) -> std::path::PathBuf {
    #[cfg(target_os = "macos")]
    {
        home.join("Library/Application Support").join(PRODUCT_NAME)
    }
    #[cfg(target_os = "windows")]
    {
        home.join("AppData").join("Roaming").join(PRODUCT_NAME)
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        // 开发机是 Linux 时仍用产品名 HeTianXia，不另起目录名。
        home.join(".local/share").join(PRODUCT_NAME)
    }
}

pub fn local_backup_dir(home: &std::path::Path) -> std::path::PathBuf {
    local_data_dir(home).join(LOCAL_BACKUP_DIR)
}

pub fn remote_backup_dir(home: &std::path::Path) -> std::path::PathBuf {
    home.join(REMOTE_BACKUP_DIR)
}

pub fn temp_file_name(file_name: &str, random: &str) -> String {
    format!(".{file_name}{TEMP_TAG}{random}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tauri_conf_matches_brand() {
        let conf = include_str!("../../../src-tauri/tauri.conf.json");
        assert!(
            conf.contains(&format!("\"productName\": \"{PRODUCT_NAME}\"")),
            "tauri.conf.json productName 必须是 {PRODUCT_NAME}"
        );
        assert!(
            conf.contains(&format!("\"identifier\": \"{BUNDLE_ID}\"")),
            "tauri.conf.json identifier 必须是 {BUNDLE_ID}"
        );
    }

    #[test]
    fn names_match_the_v14_table() {
        let home = std::path::Path::new("/Users/me");
        let data = local_data_dir(home);
        assert!(data.ends_with("HeTianXia") || data.ends_with("HeTianXia\\") || data.ends_with("HeTianXia/"));
        assert_eq!(local_backup_dir(home).file_name().unwrap(), "backups");
        assert_eq!(remote_backup_dir(home), home.join(".hetianxia-backups"));
        assert_eq!(KEYCHAIN_SERVICE, "com.hetianxia.app.ssh");
        assert_eq!(keychain_account("conn-9"), "conn-9");
        assert_eq!(temp_file_name("app.config.js", "abc"), ".app.config.js.htx-abc");
        assert!(!TEMP_TAG.contains("upg"));
        assert!(!REMOTE_BACKUP_DIR.contains("file-upgrade"));
    }
}
