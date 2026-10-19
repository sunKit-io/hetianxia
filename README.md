# 合天下（HeTianXia）

本地文件覆盖升级工具。第一版支持本地目录、zip 升级包和 SSH/SFTP。界面文字是简体中文。

许可证：[MIT](LICENSE)（Copyright (c) 2026 sunKit-io）。

代码仓库：<https://github.com/sunKit-io/hetianxia>（公开）。签名材料、`.env`、钥匙和 `hetianxia-testenv/` 不进仓库。

## 本地运行界面

```bash
pnpm install
pnpm dev
```

浏览器打开 <http://127.0.0.1:43127>。连接页可以切换：

- `build-01`：os-release 是 alpine。标题是「Linux / alpine · 未验证的系统」，系统类型一行是「未验证的系统（Linux / alpine）」，可以继续用。
- `edge-01`：plist 和 os-release 都读不到。标题和系统类型都是「未识别」，提示以「未验证的系统：」开头，可以继续用。
- `intranet-jump`：含 ProxyJump，显示「需要跳板机，第二版支持」，不能保存。

预演页里不勾选「覆盖只读文件」时，只读文件不计入覆盖，单独显示「跳过（只读）」。勾选后计入覆盖。

## 测试

需要 Rust 1.83。

```bash
cargo test -p hetianxia-core
cargo test -p hetianxia-core --features fault-inject
```

依赖许可证由 CI 检查。Rust 用 `cargo deny check`：允许 MIT、Apache-2.0、ISC、BSD-2-Clause、BSD-3-Clause、Zlib、Unicode、BSL-1.0 等宽松许可，不放行 GPL、AGPL、LGPL。npm 生产依赖用 `pnpm run licenses`（license-checker 的 `--onlyAllow`）。

`fault-inject` 不要打进正式包。故障注入构建用：

```bash
cargo build --features fault-inject
```

环境变量 `UPG_FAULT` 是逗号分隔的 `阶段:N[:方式]`。阶段：`backup`、`write`、`verify-pre`、`verify-post`、`rename`、`rollback`、`log`、`sftp-disconnect`。方式默认 `error`，也可以是 `panic`。`sftp-disconnect` 只有 `drop`。`log:N` 按事务日志写入次数计数，第 1 次是打开事务；`log:begin` 等于 `log:1`，`log:commit` 在最终提交前注入。语法错误在启动时打印 `UPG_FAULT:` 并以非零码退出。

单独的故障注入测试包请命名为 `HeTianXia-fault-inject`，不要和正式包混用。

## 命名

都在 `crates/core/src/brand.rs`：

| 用途 | 值 |
| --- | --- |
| 产品名 | HeTianXia / 合天下 |
| bundle id | `com.hetianxia.app` |
| 本地备份 | `~/Library/Application Support/HeTianXia/backups`（Windows：`%APPDATA%\HeTianXia\backups`） |
| 远程备份 | `~/.hetianxia-backups/` |
| 临时文件 | `.<文件名>.htx-<随机串>` |
| 钥匙串 | 服务名 `com.hetianxia.app.ssh`，账号是连接 id |

没有发布过，不做旧目录迁移。

## 远程系统识别

只通过 SFTP 读文件，不执行 `uname`。先读 `SystemVersion.plist`，是 macOS。否则读 `/etc/os-release`，没有再读 `/usr/lib/os-release`。`ID` 属于 ubuntu、debian、centos、rhel、rocky、almalinux 时不告警。其他发行版告警「未验证的系统（Linux / &lt;ID&gt;）」，标题写成「Linux / &lt;ID&gt; · 未验证的系统」。两个文件都读不到时，标题和系统类型都是「未识别」，提示以「未验证的系统：」开头。三种情况都可以继续用。

服务器上允许的命令只有 `sha256sum`。每次执行都记一条 `remote-exec` 日志，路径脱敏。

## macOS 安装包

这台 Linux 环境打不出 dmg。在 Mac 上：

```bash
rustup target add aarch64-apple-darwin x86_64-apple-darwin
pnpm install
pnpm tauri build --target universal-apple-darwin
```

没有 Developer ID 时，`src-tauri/tauri.conf.json` 里的 `signingIdentity` 是 `-`，等价于 `codesign -s -`。正式签名和公证只从环境变量读取，不写进仓库：

- `APPLE_SIGNING_IDENTITY`
- `APPLE_CERTIFICATE`
- `APPLE_CERTIFICATE_PASSWORD`
- `APPLE_ID`
- `APPLE_PASSWORD`（App 专用密码）
- `APPLE_TEAM_ID`

CI 使用 `pull_request`，不用 `pull_request_target`。签名任务只在推送到 `main` 或 tag 时运行，fork 的 PR 拿不到这些 secret。

## 执行引擎

写入、回滚、删除只走 `crates/core/src/exec.rs`。每个文件先备份，再写 `*.htx-*` 临时文件，读回校验，然后原子改名。全部写完再做一次校验。事务日志写失败立刻停止，不自动回滚。回滚依据是已写入的日志和备份目录里逐行追加的 `manifest.json`。回滚冲突有三个选择：继续回滚（先备份当前版本）、跳过冲突的文件、取消。单个文件回滚失败会继续处理其余文件，状态是 `ROLLBACK_PARTIAL`。
