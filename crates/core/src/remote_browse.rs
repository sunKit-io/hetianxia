//! 远程目录浏览器。只允许停在连接的远程目录里面，不跟随符号链接。

use crate::connections::normalize_remote_root;
use crate::error::{Error, ErrorCode, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryKind {
    Dir,
    File,
    Link,
    Other,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawEntry {
    pub name: String,
    pub kind: EntryKind,
}

/// 当前目录可以是远程目录本身，也可以是它下面的文件夹。`..` 直接拒绝。
pub fn directory_inside(root: &str, path: &str) -> Result<String> {
    let Some(root) = normalize_remote_root(root)? else {
        return Err(Error::new(ErrorCode::InvalidState, "先在连接里填远程目录"));
    };
    let Some(path) = normalize_remote_root(path)? else {
        return Err(Error::new(ErrorCode::InvalidLocation, "不能走出远程目录"));
    };
    if path != root && !path.starts_with(&(root + "/")) {
        return Err(Error::new(ErrorCode::InvalidLocation, "不能走出远程目录"));
    }
    Ok(path)
}

fn one_segment(name: &str) -> bool {
    !name.is_empty()
        && name != "."
        && name != ".."
        && !name.contains('/')
        && !name.contains('\\')
        && !name.contains('\0')
}

/// 只留下真文件夹。符号链接、文件、`.`、`..` 和会越出远程目录的名字都丢掉。
pub fn directory_names(root: &str, current: &str, entries: &[RawEntry]) -> Result<Vec<String>> {
    let current = directory_inside(root, current)?;
    let mut names = Vec::new();
    for entry in entries {
        if entry.kind != EntryKind::Dir || !one_segment(&entry.name) {
            continue;
        }
        let child = format!("{current}/{}", entry.name);
        if directory_inside(root, &child).is_err() {
            continue;
        }
        if !names.contains(&entry.name) {
            names.push(entry.name.clone());
        }
    }
    names.sort();
    Ok(names)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stays_inside_the_remote_root_and_skips_links() {
        assert_eq!(directory_inside("/srv/site", "/srv/site").unwrap(), "/srv/site");
        assert_eq!(directory_inside("/srv/site/", "/srv/site/app").unwrap(), "/srv/site/app");
        assert!(directory_inside("/srv/site", "/srv/site/../etc").is_err());
        assert!(directory_inside("/srv/site", "/srv/site-extra").is_err());
        assert!(directory_inside("/srv/site", "/etc").is_err());
        assert!(directory_inside("/srv/site", "/").is_err());
        let entries = vec![
            RawEntry { name: "app".into(), kind: EntryKind::Dir },
            RawEntry { name: "readme.txt".into(), kind: EntryKind::File },
            RawEntry { name: "link".into(), kind: EntryKind::Link },
            RawEntry { name: "..".into(), kind: EntryKind::Dir },
            RawEntry { name: ".".into(), kind: EntryKind::Dir },
            RawEntry { name: "a/b".into(), kind: EntryKind::Dir },
            RawEntry { name: "other".into(), kind: EntryKind::Other },
        ];
        assert_eq!(directory_names("/srv/site", "/srv/site", &entries).unwrap(), vec!["app".to_string()]);
        assert!(directory_names("/srv/site", "/etc", &entries).is_err());
    }
}
