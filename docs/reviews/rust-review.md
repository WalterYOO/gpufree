# Rust 版复核记录

日期：2026-11-25

## 背景

Go 版 v1 已获双方 PASS 后，用户指定改用 Rust。设计稿改写为 v1.1（ratatui + crossterm、线程 + mpsc、`process_group` 超时取消），SSH 协议、空闲判定、JSON schema、v1 范围保持不变。随后针对 Rust 特有实现发起一轮复核。

## 第一轮：双方 OBJECT

### codex 补充者

| 问题 | 处理 |
|---|---|
| 非 TUI 模式没有 SIGINT 处理，子进程在独立 pgid，Ctrl-C 会留孤儿 ssh | 用 `ctrlc`（termination feature）接 SIGINT/SIGTERM，置 cancel 并 kill 全部 pgid |
| JSON 模式等固定条数，worker panic 会永久挂死 | 改为读到 `mpsc` 关闭（sender 全 drop） |
| `wait()` 后才摘 pgid，pgid 可能被复用而误杀 | `try_wait` 返回 `Some` 时立即摘除 |
| 直接依赖 crossterm 可能与 ratatui 后端版本冲突 | 统一用 `ratatui::crossterm` 重导出 |

### codex 独立审核者

| 问题 | 处理 |
|---|---|
| spawn 后未复检取消；Condvar 等待不响应取消 | 在同一把 registry 锁内复检；`wait` 谓词含 `cancelled`，取消时 `notify_all` |
| mux master 是独立后台进程，不在 pgid 内，`q` 杀不到 | 明确 `q` 只清会话进程；master 交给 `ControlPersist` 过期，不主动 `-O exit`（会误关并发实例） |
| 双池共用队列会让 `parallel_remote` 失效 | direct / proxied 用独立队列和 worker |
| SIGKILL 后 `wait()` 遇 D 状态会卡死 | 改为有界 `try_wait`（2s）；超时先从 registry 摘除 pgid，再交给 detached reaper 线程 `wait()`，避免僵尸且不占 worker |

## 第二轮复核

第一轮 8 条全部落入设计稿 §5.1 / §5.3 / §2 / §11（D14–D19）。复核结论：

| 角色 | 结论 |
|---|---|
| codex 补充者 | **PASS**（最终冻结稿） |
| codex 独立审核者 | **PASS**（最终冻结稿） |

补充轮次：

- 第二轮修正 3 处：`-O exit` 会误关并发实例（改为不主动关、由 ControlPersist 过期）、D 状态放弃 Child 需 reaper 且读线程要可取消、`ssh -G` 的 `proxycommand none` 应视为直连。
- 第三轮修正 2 处：spawn 后早取消 kill 完不得裸 return（统一走 bounded reap / detached reaper）；ctrlc 取消要 `notify_all` 唤醒 Condvar 等待者。
- 第四轮修正 1 处：worker panic 会导致 JSON 缺 host、TUI 卡 spinner；每个 host 的探测用 `catch_unwind` 包裹，panic 转成 `internal_error` 结果照常发送。
- 第五轮修正 2 处：panic 若发生在 spawn 之后会漏 ssh；引入 RAII `ChildGuard`，Drop 统一 kill/reap/摘 pgid。另补 registry/许可 Mutex 的 poison 处理。
- 第六轮修正 1 处：正常 reap 后必须 `guard.disarm()`，否则 Drop 会按已被复用的 pgid 误杀；代码示例同步为 ChildGuard 写法。

最终设计：[`../DESIGN.md`](../DESIGN.md)。
