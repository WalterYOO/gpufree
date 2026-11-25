# codex 补充意见（第一轮，2026-11-25）

> 角色：设计补充者。输入：`docs/design-draft-v0.md`。以下为原始回复，未删改。

**结论摘要**
Go + bubbletea 定案（锁定 v1.3）。初稿有一处严重错误：`-n` 把 stdin 指向 /dev/null，远端 `sh -s` 收不到脚本；另有三处假设会引发问题：multiplexing（mux）的实际收益、客户端重试、空进程列表的可靠性。其余方向可行。

**逐条回答**
1. **选 Go，不换 Rust**。171 台并行探测加逐台实时刷新，正是 goroutine/channel 适合的场景；Rust 没有性能收益，编译和 TUI 迭代更慢。4 个依赖可以接受，不要再引入 cobra。
2. **保留通过 shell 调用 ssh，但参数要改**。删 `-n`（它和 stdin 脚本互斥，只留 `-T`）；删 `--`（放在 host 后不可靠，会被并入远端命令）；`ControlPath` 传 Go 展开后的绝对短路径 `~/.ssh/gf-%C`（`%C` 为 40 个十六进制字符，macOS `sun_path` 上限 104B）。`-o` 参数可能让跳板连接也创建 master，因此默认 `ControlPersist=0`，仅 TUI/`--watch` 时设为 60s，并保留 `--no-mux`。**不要**用 `-O check` 预热：它只查本地 socket，不建立连接。超时处理：`Setpgid` + `TERM→200ms→KILL(-pgid)`，`WaitDelay=500ms`，先关闭 stdin；再补 `-o RemoteCommand=none -o PermitLocalCommand=no -o ConnectionAttempts=1`。
3. **回退策略不够**，而且不能放在客户端（那相当于第二次 ssh）。应放在远端脚本内：`o=$(LC_ALL=C nvidia-smi … 2>&1); rc=$?; printf '%s\n' "$o"; echo @@GPU_RC=$rc`；rc≠0 时再跑最小字段集并输出 `@@FALLBACK=1`，客户端根据 RC 分类，而不是“解析失败再试”。字段加 `pstate,power.draw,clocks.sm`（tier-2，失败不整体降级）；compute-apps 三级降级：去 `used_memory` → 去 `process_name`。
4. **阈值可以保留，但判定要改**：`free` 必须 `processes==0` 且 compute-apps `rc==0`；否则 0% util 的常驻进程（仅几百 MB 内存）会被误判为空闲。partial 改为绝对量 `free_mb ≥ max(6144, 0.25*total)`，因为百分比在 24G 和 143G 之间不可比。samples 默认 1。
5. **补字段**：`gpus_unknown`、per-host `probed_at`、`fields_complete`、`processes_complete`、`error.retryable`；N/A 用 `null`，禁止 `omitempty`。退出码改为：0=至少 1 台 ok，1=全败，2=用法错误，3=无缓存，4=require-free；`--strict` 下任一错误→1（原来 1/3 含义重叠，`set -e` 下不好用）。
6. **组合够用，但要显式状态机**：`Normal|Filter|Help|Detail`。Filter 态捕获 `q/f/r/space`，`enter` 提交、`esc` 清空再退出，`ctrl+c` 全局退出。多选后只刷新选中项放到 M4，不做也可以。`r` 用 generation 计数丢弃上一轮迟到结果，并 cancel 旧 context。
7. **只读 TOML 可接受**，补 `presets validate`、`--print-config`。`auto` 用本地网卡和 CIDR 判断不可靠（VPN/多网卡下会误判）；改为对 `ssh -G <alias>` 解析出的 host:port 做 `net.DialTimeout` 500ms TCP 探测，成功→direct，失败→remote 别名，结果缓存 60s；`--transport` 强制覆盖。
8. **砍掉**：`p` 面板内切换 preset（状态复杂，重开更快）、partial 独立列（字段保留）；`--watch` 只允许 `ndjson|table`，与 `--json` 互斥报错。**补** `--plain`、`--hosts-file -`；cache 按 preset 分文件。
9. **保留 `gpufree`**：它指“GPU 空闲”，与 gpustat 的“统计”定位不同。

**补充建议**
- §15 的 `1 MiB / 0 %` 样例带单位，与脚本 `nounits` 矛盾：解析器必须两种都能处理，去掉 `MiB|%|W`，`[N/A]|[Not Supported]`→nil，只认 `^\d+,` 行以过滤 stderr 噪声；名称含逗号时从右取 7 列。
- 并发：direct 8 / remote 4（每台经跳板有 2 个连接，会触发 `MaxStartups`），再按首跳 host 加 `semaphore(6)`，硬上限 16。
- 超时公式 `total=max(6s, 2*connect+2s*(jumps+1))`，默认 connect 4s / total 12s；6s 对双跳偏紧。
- TUI 用 viewport 虚拟化（1368 行别整屏 lipgloss），spinner 100ms，pending 占位保持 preset 顺序以防跳行。
- 测试：`ssh_binary=testdata/fake-ssh`（POSIX sh、755），用 `trap '' TERM; sleep 30` 验证进程组 KILL；注入 `now()` + golden JSON 保证字节稳定；TUI 只测 model 状态转移。
- `io.LimitReader` 到 8MiB 后必须读尽并杀进程，否则 `Wait` 阻塞。

**反对意见**
- “ControlMaster 让同批第二次查询 <1s”不成立：一批里每台只连接一次，mux 没有收益，只剩 socket 和后台进程。
- `2>&1` 把错误混进 CSV 却不带 RC，报错会被当成数据行，这是初稿最容易忽略的解析问题。
- 退出码 1/3 含义重叠；partial 用百分比。

**风险**
- 非 root、容器或 MIG 下 compute-apps 可能不全；必须设 `processes_complete=false`，该卡降为 `unknown`，不能算 `free`。
- 2FA/口令主机在 `BatchMode` 下必然失败，只能由 doctor 明示。
- HPC 非交互 shell 常无 `nvidia-smi`（需 module load），要 `remote_prefix` 配置。
- bubbletea/teatest 版本漂移，应锁版本。
