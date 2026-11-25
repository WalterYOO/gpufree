# 三方评审与签核记录

日期：2026-11-25

## 流程

1. Claude 写 `docs/design-draft-v0.md`。
2. codex（补充者）读初稿，输出 `codex-supplement.md`。
3. codex（独立审核者）读同一份初稿，输出 `codex-audit.md`；与补充者角色隔离，不参考对方结论。
4. Claude 合并三方意见，出 `docs/DESIGN.md` v1，并对分歧逐条裁决（见设计稿 §11）。
5. 两轮 OBJECT → 修改 → 复核，双方最终 PASS。

> 原计划的审核者是 agy，但本机 agy 未登录（Orca chat 与 CLI 均要求 Google OAuth）。经用户确认，改用第二位 codex 承担独立审核。

## 第一轮复核：双方 OBJECT

### 补充者

| 问题 | 修改 |
|---|---|
| samples / interval 没有传入远端 | 由 Go 模板写入脚本；默认 2 次 / 1s（NVML 窗口） |
| JSON 有 `driver` 但脚本没查 | 核心 query 增加 `driver_version`，降级时为 null |
| ControlPath 里的 `$HOME` 不会被 ssh 展开 | 改为 Go `os.UserHomeDir()` 展开的绝对路径 |

### 审核者

| 问题 | 修改 |
|---|---|
| `compute_mode=Prohibited` 不能判 free | 判 `unknown(reason=compute_prohibited)`；`Exclusive_*` 有进程判 busy |
| `--no-mux` 仍可能命中已有 master | 同时设 `ControlPath=none` |
| 全失败与 `--require-free` 的退出码优先级未定义 | 全失败 → 1 优先；并写明完整优先级 |
| `prefix` 在 settings 与 preset 间自相矛盾 | 统一为 preset 级，删除 settings 示例 |

## 第二轮复核：双方 OBJECT

| 来源 | 问题 | 修改 |
|---|---|---|
| 审核者 | 降级查询缺 tier-2 字段时，§6「字段缺失→unknown」会让所有降级卡永远 unknown | unknown 仅限关键字段（index/uuid/三个显存/util）；tier-2 缺失只置 `fields_complete=false` |
| 补充者 | ssh 的 `ControlPersist=0` 表示永久驻留，不是关闭 | Go 将 `control_persist=0` 映射为 `ControlMaster=no` + `ControlPath=none`，绝不把 0 传给 ssh |

## 最终签核

| 角色 | 结论 |
|---|---|
| codex 补充者 | **PASS** |
| codex 独立审核者 | **PASS** |
| Claude（整合/裁决） | 定稿 |

最终设计：[`../DESIGN.md`](../DESIGN.md)（Go 版 v1）。

## 语言变更：Go → Rust（2026-11-25）

签核后用户指定改用 Rust。设计稿改为 v1.1：

- 语言/UI：Rust edition 2024 + ratatui 0.30 + crossterm 0.29。
- 进程：`process_group(0)` + `try_wait` 轮询 + `kill(-pgid)`；取消用 `AtomicBool` + 活跃 pgid 表。
- 并发：固定 worker 线程 + `mpsc`，不引 tokio。
- SSH 协议、空闲判定、JSON schema、v1 范围保持不变。

针对 Rust 特有实现经过多轮复核（进程组取消、pgid 生命周期、panic 兜底、mux 归属、线程池队列），双方最终均 PASS；完整记录见 [`rust-review.md`](rust-review.md)。

**v1.1（Rust）为当前定稿。**

## v1.2 补充（2026-11-25）

用户评审 TUI 布局后追加两条面板需求，不改探测层与 JSON：

- 列表按机器显示 `空闲/总数`（HOST 组首行，§8.1）。
- `enter` 进入单机详情，渲染该机 GPU 全字段与计算进程（§8.5）。

已写入 `../DESIGN.md`（R5、D20），v1.2 为当前定稿。
