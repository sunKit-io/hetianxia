//! 给 QA 用的小程序：校验 `UPG_FAULT` 是否能用于给定计划长度。
//! 只在 `--features fault-inject` 时编译。
//! 用法：`fault-check <spec> <plan_len> <kinds>`
//! kinds 是逗号分隔的 create,overwrite,delete,move。

fn main() {
    let mut args = std::env::args().skip(1);
    let spec = args.next().unwrap_or_default();
    let len: usize = args.next().unwrap_or_else(|| "0".into()).parse().unwrap_or(0);
    let kinds = args.next().unwrap_or_default();
    let mut ops = Vec::new();
    for k in kinds.split(',').filter(|s| !s.is_empty()) {
        ops.push(match k {
            "create" => hetianxia_core::model::OpKind::Create,
            "overwrite" => hetianxia_core::model::OpKind::Overwrite,
            "delete" => hetianxia_core::model::OpKind::Delete,
            "move" => hetianxia_core::model::OpKind::Move,
            other => {
                eprintln!("UPG_FAULT: 未知操作类型 {other}");
                std::process::exit(2);
            }
        });
    }
    if ops.len() != len {
        while ops.len() < len {
            ops.push(hetianxia_core::model::OpKind::Overwrite);
        }
        ops.truncate(len);
    }
    match hetianxia_core::fault::load_for_plan(&spec, &ops) {
        Ok(_) => {}
        Err(e) => {
            eprintln!("UPG_FAULT: {} ({})", e.message, e.code_str());
            std::process::exit(1);
        }
    }
}
