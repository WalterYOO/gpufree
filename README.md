# gpufree

一条命令看清一组服务器的 GPU 空闲情况：`--json` 给脚本，TTY 下开一个可检索的终端面板。

```bash
gpufree --preset company          # 终端面板（可搜索 / 过滤 / 刷新，enter 看单机详情）
gpufree --host 4090-1 --host gpu06 --json | jq '.summary'
gpufree 4090-1 gpu06              # 位置参数也可以
```

## 状态

**M1–M3 已实现，独立评审 PASS。** `cargo fmt --check`、`clippy -D warnings`、197 个测试、release build 全部通过；真实 `4090-1` 冒烟正常。

- 设计稿：[`docs/DESIGN.md`](docs/DESIGN.md)
- 独立验收计划：[`docs/reviews/codex-m1-m3-plan.md`](docs/reviews/codex-m1-m3-plan.md)
- 审查与整改记录：[`docs/reviews/codex-m1-m3-review.md`](docs/reviews/codex-m1-m3-review.md)
- 初稿：[`docs/design-draft-v0.md`](docs/design-draft-v0.md)
- 配置示例：[`config.example.toml`](config.example.toml)

## 核心设计

- **复用 ssh**：`std::process::Command` 调系统 `ssh`，`~/.ssh/config` 的别名、跳板、ProxyJump、密钥全部沿用；远端只跑一段只读 `sh` 脚本。
- **一次往返**：GPU 列表、进程属主、机器负载在同一次 ssh 会话里取回，两次采样也在远端完成。
- **两种输出**：`--json`（稳定 schema）或 TTY 下的 ratatui 面板；非 TTY 自动退化为纯文本表格。
- **面板可检索**：`/` 对 host、GPU 型号、用户、进程名做子序列过滤；`f` 按 free/busy/unknown/error 过滤；每台机器显示 `空闲/总数`。
- **单机详情**：`enter` 查看该机 GPU 全字段与计算进程（复用同一轮探测结果，不额外 ssh）。
- **空闲判定保守**：无 compute 进程、已用显存 ≤512 MiB、利用率 ≤10%，两次采样取最差。
- **坏机器不拖垮整批**：每台独立超时、进程组 kill、错误分类进 JSON。
- **终端消失就退出**：关闭窗口、kill tmux session、ssh 断开时取消全部探测并退出，不留 100% CPU 的孤儿进程（A29）。
- **预设即 ssh 别名**：不复制 IP/端口/用户，不制造第二份拓扑真相。

## 构建

```bash
make build      # cargo build --release，并复制到 bin/gpufree
make install    # -> ~/.local/bin/gpufree
make test
```

要求 Rust 1.85+（edition 2024）。主要依赖：`clap`、`ratatui`（crossterm 用其重导出）、`serde`、`toml`、`libc`。

不使用 tokio：ssh 是阻塞子进程，固定 worker 线程 + `mpsc` 足够。

## 配置

```bash
mkdir -p ~/.config/gpufree
cp config.example.toml ~/.config/gpufree/config.toml
```

`config.example.toml` 已按本机 ssh 拓扑预置 `company`、`hk-h20`、`h100`、`lab`、`jinan` 五组。
默认 `transport = "direct"`；在外网时把 preset 改成 `"remote"`（走 `-remote` 别名）或显式开启 `"auto"`。

## 里程碑

| 阶段 | 内容 | 状态 |
|---|---|---|
| M1 | 配置 + 探测 + JSON/表格 | 完成 |
| M2 | 终端面板 + 搜索/过滤/取消 + 单机详情 | 完成 |
| M3 | doctor + init + transport auto | 完成 |
| v2 | 缓存、watch/ndjson、partial、面板切 preset 等 | 未开始 |

## 目录

```
src/main.rs         CLI 入口
src/run.rs          子命令、来源解析、输出模式、退出码
src/cli.rs          clap 定义
src/config.rs       配置、preset、transport 校验
src/signal.rs       SIGINT/SIGTERM 统一取消
src/model.rs        数据模型与 free/busy/unknown 判定
src/probe/          ssh 进程管理、脚本模板、解析、transport
src/output/         json / table / 控制字符清理
src/tui/            面板 App、渲染、过滤
src/doctor.rs       自检
tests/              CLI 集成测试与 JSON golden
testdata/           fake-ssh 等测试夹具
docs/               设计稿与评审
```
