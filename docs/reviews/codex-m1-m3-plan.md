# gpufree M1–M3 独立验收计划（准备稿）

- 日期：2026-11-25
- 角色：独立 reviewer/verifier（与实现者隔离）
- 依据：`docs/DESIGN.md` v1.2（663 行）、`README.md`、`Cargo.toml`、`config.example.toml`、当前骨架
- 基线：`1ab8e2d chore: scaffold gpufree design and Rust skeleton`（工作树干净）
- 状态：**准备完成，待实现交付后执行**。本文件不含对未完成代码的结论。

---

## 0. 范围与约束

1. 本计划只从规格生成验收项，不参考实现者代码。执行阶段才读实现，且以黑盒行为为主、源码只用于定位 `file:line`。
2. 验收 R1–R5，覆盖 §3 CLI/退出码、§4 配置、§5 探测与 ssh 生命周期、§6 状态机、§7 JSON、§8 TUI（含详情页）、§9 doctor、§10 安全。
3. 不修改 `src/`、`tests/`、`Cargo.toml`、`docs/DESIGN.md`、`README.md`、`config.example.toml`。唯一允许写入的仓库文件是本计划；执行阶段允许新增 `docs/reviews/codex-m1-m3-review.md`。
4. 所有脚本、夹具、日志放 `/tmp/gpufree-review/`。不触碰真实 `~/.ssh`：测试用独立 `HOME`/`XDG_CONFIG_HOME`/`TMPDIR`。
5. 准备阶段只运行了 `cargo check`（通过）确认骨架可编译；当时 `src/main.rs:18-21` 仍固定打印“尚未实现”并退出 2，`src/cli.rs:12-16` 只有 `--preset`，属预期骨架状态，不作为缺陷。

---

## 1. 执行前冻结与证据协议

实现者与本 reviewer 共用工作树，必须防止“测到一半代码变了”。

1. 开始前记录：
   - `git rev-parse HEAD`
   - `git status --porcelain=v1`
   - `git diff | shasum -a 256`（含未跟踪文件时另记 `git ls-files -o --exclude-standard` 与逐文件 hash）
   - `shasum -a 256 target/debug/gpufree`（构建后）
2. 只构建一次并在整轮复用；不运行 `cargo fmt`、`cargo clippy --fix` 等会改文件的命令。
3. 每轮结束重新计算上述 hash；任一变化则受影响检查作废并重跑。若实现者仍在改，暂停并升级给协调者，不把半成品判 OBJECT。
4. 每条检查的证据目录：`/tmp/gpufree-review/evidence/<run-id>/<CHECK-ID>/`，至少保存：
   - `exit.txt`（退出码）、`stdout`、`stderr`、`elapsed_ms`
   - `fake-log.jsonl`（argv、stdin hash、pid/pgid、时间、场景、并发计数）
   - `ps-before.txt` / `ps-after.txt`（含 `pgrep -fl fake-ssh`、`ps -o pid,ppid,pgid,stat,command`）
   - TUI 场景另存 `screen.txt`（`tmux capture-pane -p -e`）和终端状态 `stty.txt`
5. 时间容差：超时类允许 +2.5s（20ms 轮询 + 200ms TERM 升级 + 调度抖动）；并发类允许 1 个瞬时越界的采样误差，但不得持续超限。

---

## 2. 测试夹具

### 2.1 fake-ssh（`/tmp/gpufree-review/fake-ssh`，POSIX sh，`chmod 755`）

同一二进制承担三种调用：

1. `fake-ssh -G <alias>`：输出 `hostname`、`port`、`proxyjump`、`proxycommand`、`user`，由 `/tmp/gpufree-review/fixtures/<alias>.g` 决定。`proxycommand none` 必须原样输出。
2. `fake-ssh -V`：输出 `OpenSSH_10.3p1, LibreSSL 3.3.6`（供 doctor）。
3. `fake-ssh <ssh flags...> <alias> sh -s`：
   - 读完全部 stdin 到 `stdin.bin`，校验非空并记录 sha256；绝不执行它。
   - 记录完整 argv（一行一个参数）和 `pid/ppid/pgid` 到 JSONL。
   - 用 `flock` 更新并发计数文件（start/end、direct/proxied、jump key），用于峰值并发断言。
   - 按 alias 场景输出 §5.2 协议；场景由 `fixtures/<alias>.scenario` 选择。
   - 可选 `delay`、`flood-bytes`、`ignore-term`、`spawn-child`、`exit-code`、`stderr`。

必须支持场景：`ok`、`ok-two-gpu`、`ok-mixed`、`no-gpu`、`missing-smi`、`fallback`、`fallback-fail`、`mig`、`proc-fail`、`ps-fail`、`csv-weird`、`csv-quoted-comma`、`malformed-section`、`malformed-csv`、`timeout`、`ignore-term`、`flood-9m`、`slow-ok`、`auth`、`hostkey`、`unreachable`、`dns-fail`、`ssh-timeout-text`、`gpu-fail`、`dup-index`、`huge-pids`、`weird-hostname`。

### 2.2 配置夹具

每个场景独立 `XDG_CONFIG_HOME=/tmp/gpufree-review/xdg/<case>`、`HOME=/tmp/gpufree-review/home/<case>`；`config.toml` 的 `ssh_binary` 指向 fake-ssh。另备：

- `cfg/min.toml`：`--host` + settings 默认值。
- `cfg/preset-direct.toml`、`cfg/preset-remote.toml`、`cfg/preset-auto.toml`。
- `cfg/broken-*.toml`：语法、类型、缺 alias、未知键、越界数值。
- `hosts.txt`：空行、`#` 注释、前后空格、CRLF、重复项。

### 2.3 TUI 驱动

用 `tmux -L gf-review` 独立 socket：

```sh
tmux -L gf-review new-session -d -x 140 -y 40 "$GF --config $CFG --preset tui"
tmux -L gf-review capture-pane -p -e -t 0 > screen.txt
tmux -L gf-review send-keys -t 0 q
tmux -L gf-review display-message -p '#{alternate_on}' > alt.txt
```

非 TTY 用普通 pipe（`script` 仅在必须制造 pty 时用，macOS BSD `script` 参数与 GNU 不同）。终端恢复检查在 app 退出后向同一 pane 发送 `stty -a > stty.txt`，确认 `icanon`/`echo` 恢复。

### 2.4 变量

```sh
GF=/Users/walter/dsh/temp/gpufree/target/debug/gpufree
R=/tmp/gpufree-review
FX=$R/fake-ssh
CFG=... ; export XDG_CONFIG_HOME=... HOME=... TMPDIR=$R/tmp
```

---

## 3. 验收矩阵

每条均为黑盒检查；“预期证据”列出判定所需的可观测事实。`E`=退出码，`O`=stdout，`R`=stderr，`L`=fake-ssh 日志，`P`=进程表，`S`=tmux 屏幕，`T`=耗时。

### 3.1 CLI 表面与退出码（§3，R1/R2）

- **CLI-01 help/version** — `$GF --help`、`$GF --version`；E=0，O 含用法/版本，L 为空（不探测），R 为空。
- **CLI-02 无参数 + default_preset** — 配置含 `default_preset`；非 TTY 运行；E=0，L 恰好探测该 preset 的主机，O 为表格。
- **CLI-03 无参数 + 无 default_preset** — 配置无该键且无来源；E=2，R 给出配置示例/`init` 提示，O 无 JSON/表格，L 为空。
- **CLI-04 五种来源等价** — `--preset`、重复 `--host`、`--hosts a,b`、位置参数、`--hosts-file -` 分别给出同一 hosts 集合；JSON 的 `hosts[].host` 顺序等于输入顺序；`--hosts "a, b"` 去除空格；`--hosts-file -` 忽略空行与 `#` 行。
- **CLI-05 来源互斥矩阵** — 对五种来源做 10 组两两组合；全部 E=2，R 为用法错误，O 为空，L 为空（未探测）。
- **CLI-06 空来源值** — `--host ""`、`--hosts ""`、`--hosts ",,"`、全注释的 hosts-file；E=2 或明确“空主机列表”错误，不得探测 0 台后返回 0。
- **CLI-07 未知 flag/缺值/未知子命令** — E=2，R 含用法，L 为空。
- **CLI-08 preset 不存在** — E=2，R 含 preset 名与可用 preset，L 为空。
- **CLI-09 非法 transport** — `--transport foo` 或配置值非法；E=2，R 可读，L 为空。
- **CLI-10 配置解析失败** — TOML 语法/类型错误；E=2，R 指向文件与键，O 为空，无 panic/backtrace。
- **CLI-11 全失败** — 2 台均 `auth`/`timeout`；E=1；JSON 仍含 2 个 error host；表格仍列错误行。
- **CLI-12 部分失败、无 --strict** — 1 ok + 1 error；E=0；JSON `hosts_error=1`。
- **CLI-13 部分失败 + --strict** — 1 ok + 1 error 加 `--strict`；E=1（无论是否有空闲卡）；再加 `--require-free` 且无空闲仍 E=1。
- **CLI-14 全失败 + --require-free** — E=1（全失败优先于 3）；另测“1 ok 但无空闲 + 1 error + --require-free（无 --strict）”→ E=3（有成功、无空闲，优先于 0）。
- **CLI-15 成功但无空闲 + --require-free** — 所有 GPU busy；E=3。
- **CLI-16 成功但无空闲、无 --require-free** — E=0（不得因无空闲而失败）。
- **CLI-17 成功且有自由卡 + --require-free** — E=0。
- **CLI-18 --free-only** — 输出/JSON 只保留至少一张 `free` 的 host；error 与全 busy host 被移除；`summary.hosts_shown` 等于保留数；退出码仍按 CLI-11..17 的规则（尤其全 busy 时无 `--require-free` 仍 0）。
- **CLI-19 用法/配置错误 + --json** — O 必须为空（不得输出半个 JSON），R 可读，E=2。
- **CLI-20 --json 与 --format 的别名/冲突** — `--json` 与 `--format json` 结构一致（时间戳除外）；`--json --format table` 采用确定行为：优先报 E=2，若实现选择“后者覆盖”，需在输出中只出现一种格式且无混合；记录实际并对照 §3 的别名措辞判 AMBIGUOUS。

### 3.2 配置与 transport（§4，R1/R3）

- **CFG-01 默认路径与 XDG** — 只设 `XDG_CONFIG_HOME` 时读取 `$XDG_CONFIG_HOME/gpufree/config.toml`；未设时读取 `$HOME/.config/gpufree/config.toml`；`--config` 覆盖两者。
- **CFG-02 无配置文件 + --host** — 无 config.toml 时 `--host` 仍用内置默认值工作；E=0，L 有探测（§0 R1 隐含；若实现报 2，记 AMBIGUOUS 而非直接 OBJECT）。
- **CFG-03 显式 --config 不存在** — E=2，R 指明路径，L 为空。
- **CFG-04 默认 settings** — 不写 settings 时实际生效：parallel=8、parallel_remote=4、connect_timeout=6s、total_timeout=15s、samples=2、sample_interval=1s、ssh_binary=ssh、mux=true、control_persist=60s、color=auto、used_mem_max_mb=512、util_max_pct=10；用 fake log/脚本内容间接验证。
- **CFG-05 default_preset 不存在** — E=2，R 含键值，L 为空。
- **CFG-06 时长解析** — `"500ms"`、`"6s"`、`"1m"` 被接受并映射到 argv/脚本；`"6"`、`"-1s"`、`"abc"` E=2，无 panic。
- **CFG-07 数值越界** — parallel=0、parallel=-1、parallel_remote 超大、samples=0、samples=-1、util_max_pct=101、used_mem_max_mb=-1；不得死锁/panic/建无限线程；实现应给 E=2（或对 hard cap 做有记录的上限钳制）。实际行为与 §4/§5.3 的“硬上限 16”对照，记 AMBIGUOUS。
- **CFG-08 remote 缺 alias 预检** — `transport="remote"` 但 `remote_aliases` 缺某 host；E=2，R 同时含 preset 与 host，L 为空（探测前失败）。
- **CFG-09 remote 映射生效** — `resolved_alias` 等于映射值，ssh argv 使用映射别名；未映射的 host 不静默直连。
- **CFG-10 auto 直连可达** — `ssh -G` 给出 127.0.0.1:<本地监听端口>，TCP 可连；`transport_used="direct"`，使用原 alias。
- **CFG-11 auto 直连不可达** — 同上但端口关闭；使用 remote 映射，`transport_used="remote"`；若映射缺失则 E=2（CFG-12）。
- **CFG-12 auto 映射缺失** — 直连探测失败且无映射；E=2，R 可读；不得回退直连或输出半批 JSON。
- **CFG-13 auto 结果缓存 60s** — 同一 alias 在一次运行内重复出现（或 doctor 解析多 preset）只产生一次 TCP 连接（监听器计数）；`doctor`/`--print-config` 能显示 auto 判定；记录缓存是否仅进程内（§4 未要求跨进程持久化）。
- **CFG-14 --transport 覆盖优先级** — CLI 覆盖 preset；`--transport direct` 时即使 preset=remote 也不查映射；`--transport remote` 时即使 preset=direct 也查映射。
- **CFG-15 prefix 仅 preset 级** — preset 的 `prefix` 原样出现在远端脚本最前；`[settings].prefix` 必须被拒绝或忽略（§4:202 明确 settings 里没有）。注意 `config.example.toml:18` 在 `[settings]` 下注释了 `# prefix`，与 §4:202 矛盾，记录。
- **CFG-16 重复/空白 host** — `--host a --host a`、`--hosts "a,a"`、hosts-file 重复/前后空格；行为确定且顺序稳定；不得重复并发失控或丢失 host。去重与否 §7:451 未定义，记 AMBIGUOUS。
- **CFG-17 未知配置键** — `settings.typo=1`；确定行为：E=2 且指出键，或忽略并告警；不得静默改变阈值。
- **CFG-18 presets list/validate** — `presets list` 列出全部 preset；`presets validate` 对合法配置 E=0、对缺 alias/缺映射 E 非 0 且 R 可读；两个子命令不得触发探测。
- **CFG-19 示例配置可解析** — 用 `config.example.toml` 跑 `presets validate`/`--print-config`；五个 preset 全部解析，无未知键错误。

### 3.3 ssh 调用与进程生命周期（§5.1/§5.3，R1/R3）

- **SSH-01 默认 argv 精确性** — L 中每个探测包含：`-T`、`-o BatchMode=yes`、`-o ConnectTimeout=<cfg>`、`-o ConnectionAttempts=1`、`-o ControlMaster=auto`、`-o ControlPath=<HOME 绝对路径>/.ssh/gf-%C`、`-o ControlPersist=60s`、`<alias>`、`sh`、`-s`；不含 `-n`、不含 `--`；`$HOME` 未被字面传入。
- **SSH-02 stdin 脚本** — 每次探测 stdin 非空、以 `@@END` 结束、只写一次；不得因管道关闭提前 EOF。
- **SSH-03 mux 默认** — 配置默认/显式 `mux=true` 时 SSH-01 的 mux 参数齐全。
- **SSH-04 --no-mux** — 出现 `ControlMaster=no`、`ControlPath=none`，不出现 `ControlMaster=auto`，不出现 `ControlPersist=0`；可保留无害的 `ControlPersist=60s` 以外值，但不得启用复用。
- **SSH-05 mux=false** — 与 SSH-04 相同。
- **SSH-06 control_persist=0** — `"0"` 与 `"0s"` 都映射为 SSH-04；不得把 `0` 原样传给 ssh。
- **SSH-07 禁止 ControlPersist=0** — 所有场景 grep L，任何 argv 都不得含 `ControlPersist=0`（含 `--no-mux`、mux=false、0/0s、doctor）。
- **SSH-08 ssh -G 分类** — `proxyjump` 非空且非 `none` → proxied；`proxycommand` 非空且非 `none` → proxied；两者均 none/缺失 → direct；分类不受 `transport` 模式影响。
- **SSH-09 proxycommand none** — 仅 `proxycommand none` 时归 direct（不是 “有值” 就 proxied）。
- **SSH-10 direct + ProxyJump** — preset `transport=direct` 但 `ssh -G` 有 proxyjump；仍进 proxied 队列，受 `parallel_remote` 限制。
- **SSH-11 双池并发** — 20 台 direct + 12 台 proxied，fake 每台 sleep 300ms；日志峰值 direct ≤ parallel、proxied ≤ parallel_remote，且两池可同时运行；不得出现 direct worker 抢 proxied 主机。
- **SSH-12 线程硬上限** — 默认总 worker ≤12；配置 parallel 超限时总线程 ≤16（或 E=2），不得按配置值无界创建。
- **SSH-13 每跳 6 个软限** — 10 台同 proxyjump 的 host 同时探测；该 jump key 峰值 ≤6，其他 jump 不受影响；失败/取消后许可释放（后续 host 可继续）。
- **SSH-14 总超时与 TERM→KILL** — `total_timeout=2s`，fake `timeout` sleep 30s；约 2s 发 TERM，200ms 后 KILL；E/JSON 为 `ssh_timeout`，`retryable=true`。
- **SSH-15 超时上界** — SSH-14 的 T ≤ total_timeout + 2.5s；即使 fake 不响应 TERM，也不得等到 30s。
- **SSH-16 CLI SIGINT 清理** — 启动 8 台 slow host，0.6s 后 `kill -INT $GF`；进程在 2s 内退出；fake pid/pgid 全部消失；`ps` 无 defunct；R 无 panic。
- **SSH-17 CLI SIGTERM 清理** — 同 SSH-16 用 SIGTERM；行为一致。
- **SSH-18 TUI q 清理** — TUI 中 8 台 slow host，按 `q`；进程退出后无 fake/子进程；`alternate_on=0`，`stty` 恢复 `icanon echo`。
- **SSH-19 spawn/取消竞态** — 启动后立即 SIGINT，重复 5 次；每次无孤儿、无僵尸、无 “刚 spawn 就漏掉” 的 fake 进程。
- **SSH-20 D 状态/忽略 TERM** — fake `trap '' TERM` 且子进程也忽略；SIGKILL 后 ~2.2s 内返回；若 2s 后仍不退，worker 不得挂死（有界 try_wait + detached reaper），JSON/TUI 仍完成。macOS 无法制造真实 D 状态，另做源码检查确认 reaper 路径。
- **SSH-21 pgid disarm/误杀** — 正常成功 host 结束后不得再向旧 pgid 发信号；用“成功 host + 独立长命进程”做近似，并在源码确认 `try_wait()==Some` 后立即摘除/`disarm`。无法可靠强制 pgid 复用时，以源码证据 + 反向测试（不得杀掉无关进程）共同判定。
- **SSH-22 8 MiB 上限** — fake 输出 9 MiB 到 stdout（及另一场景 9 MiB 到 stderr）；超过 8 MiB 后触发 kill，T 有界，L 记录进程被终止；随后继续读到 EOF，无管道背压死锁。
- **SSH-23 worker panic/通道关闭** — 用极端畸形输入尝试触发解析 panic；即使某 host panic，JSON 仍输出全部 host（该 host `internal_error`/`parse_error`），进程不挂；TUI 不留永久 spinner。若黑盒无法稳定触发，源码检查 `catch_unwind` 与 sender drop。
- **SSH-24 大输出背压** — 同一 host 同时写 4 MiB stdout + 4 MiB stderr、慢速；不得因只读一路而卡死，两读线程均能收尾。
- **SSH-25 mux master 不被 q 杀** — fake 模拟独立 pgid 的 master 进程；TUI `q` 后 master 仍存在（或至少不发送 `-O exit`）；会话进程消失。真实 ssh master 属手工验证项。

### 3.4 远端脚本、解析与错误分类（§5.2/§5.4，R1/R2）

- **PRS-01 分节协议** — stdin 脚本与远端输出按 `@@HOST` → `@@NOGPU` → `@@MIG` → `@@GPU` → `@@PROC` → `@@PS` → `@@END` 顺序；fake 输出乱序/缺段时分别验证容错与 `parse_error`。
- **PRS-02 采样参数模板化** — `samples=2/interval=1s` 时脚本循环 2 次、sleep 1；`samples=3/interval=500ms` 时脚本实际变为 3 次/0.5s；不得硬编码 §5.2 示例中的 `2`/`1`。
- **PRS-03 NOGPU=1** — 无 `/dev/nvidia0` 且无工具；host `status=ok`、`gpus=[]`、`note=no_gpu`、计入 `hosts_ok`，E=0。
- **PRS-04 NOGPU=2** — 有 `/dev/nvidia0` 但无工具；host error `nvidia_smi_missing`、`retryable=false`、`gpus=[]`；E 按 host 失败规则。
- **PRS-05 降级触发** — 10 列 query rc≠0、7 列 query 成功；输出含 `@@FALLBACK=1`；`fields_complete=false`；`driver/compute_mode/temp_c=null`；GPU 行仍存在。
- **PRS-06 降级后状态** — 7 列数据（含 index/uuid/mem/util）仍能判 free/busy；不得因 tier-2 缺失把整机判 unknown。
- **PRS-07 降级也失败** — 两次 query 都 rc≠0；host `nvidia_smi_failed`，不得输出半截 GPU 或伪造 0。
- **PRS-08 MIG 探测** — `nvidia-smi -L` 含 MIG → `@@MIG=1`；所有 GPU `state=unknown`、`reason="mig"`、`mig=true`；不虚构实例。
- **PRS-09 PROC 失败** — compute-apps rc≠0；`processes=null`、`processes_complete=false`；低 util/mem 的 GPU 为 `unknown`/`no_process_data`（STA-06），高 util/mem 仍 busy（STA-07）。
- **PRS-10 PS 失败** — PROC 成功但 `ps` 失败/为空；进程列表仍存在，`user` 为 null；`processes_complete` 的语义记录（§7 未定义 PS 失败是否影响该标志），不得把 host 判 parse_error。
- **PRS-11 PID 过滤** — PROC 含 `[N/A]`、提示文本、负数、`123abc`；只有纯数字 PID 进入 `ps -p`；不得因非数字 PID 报错或执行注入。
- **PRS-12 分节结构损坏** — 缺 `@@END`、重复 `@@GPU`、`@@NOGPU` 后无值；host `parse_error`，`retryable=false`，无 panic。
- **PRS-13 CSV 损坏** — 核心行少列/多列、非数字关键列、空行；不得 panic；预期为 `parse_error` 或关键字段缺失导致的 `unknown`（§5.4 只明确“分节不可解析”为 parse_error，逐行损坏需记录实际判定）。
- **PRS-14 单位/空白/[N/A]** — `"24564 MiB"`、`" 0 %"`、`"[N/A]"`、`"[Not Supported]"`、空字段；可解析值转数字，不可解析关键值转 null→unknown；不得把 `[N/A]` 当 0。
- **PRS-15 逗号进程名** — `python train.py --name "a,b"` 以 CSV 引号返回；进程名完整保留，不拆成多列；JSON 值精确。
- **PRS-16 Unicode/超长进程名** — 中文、emoji、4 KiB 名称；JSON 原样保留（合法 UTF-8），TUI 截断不 panic；不得按字节截断产生非法 UTF-8。
- **PRS-17 多卡/重复 index** — 8 卡、重复 index、乱序 index；输出保持稳定顺序（或按 index 排序），重复项行为确定；不得覆盖丢卡或 panic。
- **PRS-18 hostname/load/nproc 异常** — 缺失、空、`4.0 1.2 0.9`、`abc`; `hostname`/load1/cpus 为 null 或正确值，host 仍 `ok`；不得因 tier-1 辅助字段失败丢掉 GPU 数据。
- **PRS-19 两次采样合并** — 样本 A util=5/used=100，样本 B util=20/used=600；结果 util=20、used=600（取最差）；记录 `memory_free_mb` 的实际来源（案例中 total-used ≠ 示例 free，见 §4 歧义 A2），并要求 JSON 内部自洽。
- **PRS-20 stderr 噪声/RC** — ssh rc=0 但 stderr 含 `Warning: Permanently added ...` → 成功；GPU rc≠0 且错误文本含 “timed out” → `nvidia_smi_failed` 而非 `ssh_timeout`；错误分类按 §5.4 + 优先级记录。
- **PRS-21 错误分类矩阵** — 逐一制造 `timed out`、`Connection refused`、`No route to host`、`Could not resolve hostname`、`Permission denied`、`Host key verification failed`、GPU rc≠0、分节损坏；kind 与 `retryable` 精确匹配 §5.4；未知错误为 `unknown`。

### 3.5 状态判定（§6，R2/R5）

- **STA-01 free 精确边界** — used=512、util=10、无进程 → `free`（阈值是 `>` 才 busy）。
- **STA-02 busy 显存** — used=513、util=0、无进程 → `busy`。
- **STA-03 busy 利用率** — used=0、util=11、无进程 → `busy`。
- **STA-04 进程优先** — used=1、util=0、存在 idle `python`/notebook → `busy`。
- **STA-05 Exclusive 模式** — compute_mode=Exclusive_Process/Exclusive_Thread 且存在该模式进程 → `busy`。
- **STA-06 processes=null 低负载** — used≤512、util≤10、`processes=null` → `unknown`、`reason=no_process_data`。
- **STA-07 processes=null 高负载** — used>512 或 util>10、`processes=null` → `busy`（阈值可独立确认）。
- **STA-08 MIG** — 任意显存/利用率/进程组合，MIG=1 → 所有 GPU `unknown`/`mig`。
- **STA-09 Prohibited** — compute_mode=Prohibited、无进程、低负载 → `unknown`、`reason=compute_prohibited`。
- **STA-10 Prohibited + 进程（歧义）** — Prohibited 且有进程：§6:361 的 unknown 列在 §6:363 的 busy 之前，预期 `unknown`；若实现给 `busy`，记 AMBIGUOUS 并引用两行，不直接判错。
- **STA-11 关键字段缺失** — 分别缺 index/uuid/memory_total/memory_used/memory_free/utilization；各自 → `unknown`；不得用 0 补缺。
- **STA-12 tier-2 缺失** — 分别缺 temp/compute_mode/driver；`fields_complete=false`，但仍按显存/util/进程判 free/busy。
- **STA-13 两次采样最差** — 仅一项越界也必须 busy（util 20/5、used 600/100 两种组合）；同降级/进程数据组合验证优先级。
- **STA-14 计数口径** — `free` 只计 state=free；unknown/MIG 计入 gpus_total 不计 free；error host 不贡献 GPU 计数。

### 3.6 JSON 契约（§7，R2）

- **JSN-01 基本可用** — `$GF --json` 输出可被 `jq -e .` 解析；`schema=="gpufree/v1"`；stdout 无表格/日志。
- **JSN-02 顶层键** — 始终含 `schema/generated_at/preset/transport/duration_ms/summary/hosts`；`--host` 无 preset 时 `preset=null`（§7:442 的“键始终存在”），记录实际。
- **JSN-03 summary 键与数学** — 含 `hosts_total/hosts_ok/hosts_error/hosts_shown/gpus_total/gpus_free/gpus_busy/gpus_unknown`；`hosts_total=hosts_ok+hosts_error`；`gpus_total=gpus_free+gpus_busy+gpus_unknown`；`hosts_shown ≤ hosts_ok`（`--free-only` 时更小）。
- **JSN-04 host 顺序** — preset/参数顺序稳定，与并发完成顺序无关；重复运行顺序一致。
- **JSN-05 ok host 键集** — 含 `host/resolved_alias/transport_used/status/probed_at/duration_ms/hostname/load1/cpus/fields_complete/processes_complete/gpus`；可空字段为 `null` 而非缺键（与 §7:442 对照）。
- **JSN-06 error host 键集** — 含 `host/resolved_alias/transport_used/status/probed_at/duration_ms/gpus/error`；`gpus=[]`；`error.kind/message/retryable` 齐全；是否补 hostname/load1/cpus/fields_complete/processes_complete 与 §7 示例冲突（示例缺这些键），记录 AMBIGUOUS。
- **JSN-07 no_gpu 形状** — `status=ok`、`gpus=[]`、`note="no_gpu"`、计入 `hosts_ok`；`fields_complete/processes_complete` 的实际值记录（§7:444 未定义）。
- **JSN-08 processes null vs []** — PROC 失败 → `null`；确认无进程 → `[]`；两者不得互换，TUI 文案也不同。
- **JSN-09 GPU 键集** — 含 `index/uuid/name/driver/mig/compute_mode/memory_total_mb/memory_used_mb/memory_free_mb/util_pct/temp_c/state/processes`；非 free 的 `reason` 可为字符串或 null；free 是否带 `reason:null` 记录（§7:446 与 442 冲突）。
- **JSN-10 driver/tier-2 null** — 降级查询 `driver=null`；缺 temp/compute_mode 时对应字段 null；键仍在。
- **JSN-11 state/mig 枚举** — `state ∈ {free,busy,unknown}`；`mig` 为 bool；无其他字符串。
- **JSN-12 transport 语义** — `transport` 是请求模式；`transport_used` 是每 host 最终选择（direct/remote）；`resolved_alias` remote 时为映射值，direct 时是否等于 host 记录；`--transport direct` 时不出现 remote。
- **JSN-13 时间格式** — `generated_at/probed_at` 为 RFC3339 带偏移；`--utc` 改为 UTC（`Z` 或 `+00:00`）；`probed_at` 不晚于 `generated_at`；`probed_at` 是开始还是结束时间记录（§7 未定义）。
- **JSN-14 duration_ms** — 顶层与 host 均为非负整数；顶层 ≥ 最长 host（允许调度开销）；与 T 观测同量级。
- **JSN-15 --free-only 语义** — `hosts` 数组是否移除 busy/error host、`summary` 是否只统计保留 host、`hosts_total` 是否仍为原始总数；以 §3:126 + §7:449 判定，记录实际并标 AMBIGUOUS（两行未说清 `hosts` 数组）。
- **JSN-16 stdout 纯净** — 警告/进度/调试信息只在 stderr；stdout 从第一个 `{` 到最后一个 `}` 可解析；`--json` 下 TTY 也不得混入面板控制序列。
- **JSN-17 null 与 0/false** — util=0、mem=0、temp=0 与 null 可区分；不得把 null 序列化为 0；`retryable` 为 bool 而非字符串。
- **JSN-18 summary 排除 error/no_gpu** — error host 的 `gpus=[]` 不贡献 gpus_*；no_gpu 贡献 0；`gpus_unknown` 包含 MIG/字段缺失/进程未知。
- **JSN-19 JSON 下的退出码** — CLI-11..18 在 `--json` 下退出码不变；E 与 JSON `status`/`summary` 一致。
- **JSN-20 数字合法性/键唯一** — 无 `NaN`/`Infinity`/重复键；大整数不溢出；`jq` 可 round-trip。

### 3.7 TUI 列表、检索、刷新、取消（§8.1–§8.4，R3/R4/R5）

- **TUI-01 非 TTY 退化** — stdout 为 pipe 且无 `--json`；输出纯文本表格，无 alternate screen/ANSI；E=0。
- **TUI-02 TTY 进入与流式** — tmux 启动；出现 header/表头/底栏；慢 host 先显示 spinner，随后原位替换为数据；host 行顺序固定为 preset 顺序；200ms 内首帧。
- **TUI-03 TTY + --json** — 不进入 alternate screen；stdout 为 JSON；S 中无面板边框。
- **TUI-04 TTY + --format table** — 记录实现：应输出表格而非 TUI（§3:125 明确 format 选择）；若仍进 TUI，记 AMBIGUOUS（§8:118 只说 TTY 默认面板）。
- **TUI-05 header 汇总** — `ok/total hosts` 与 `free` GPU 数正确；随结果到达更新；error host 计入 total、不计 ok。
- **TUI-06 每机 free/total** — 3 卡（free,busy,unknown）显示 `a/3`；仅 1 卡显示 `1/1`；unknown 计入分母不计分子；计数出现在该 host 连续组首行，组内后续行 HOST 为空。
- **TUI-07 计数不受过滤影响** — `/` 只匹配到 busy 卡、`f=free` 时，该 host 仍显示完整分母与完整 free 数（§8.1:477）。
- **TUI-08 排序 s** — 在 host 顺序与“空闲显存降序”间切换；组可能不连续，每次 host 变化重新标注计数；并列值顺序稳定；选择行不丢。
- **TUI-09 检索字段** — `/` 对 host、GPU name、user、process name 做大小写不敏感子序列匹配；分别用 `H20`、`gpu0`、`root`、`train` 验证；不匹配行隐藏。
- **TUI-10 多关键词** — `/` 输入 `gpu train`：两个关键词都命中才显示；顺序无关；空格分隔；计数栏 `matched/total` 实时更新。
- **TUI-11 f 循环/交集** — `f` 顺序 all→free→busy→unknown→error→all；状态过滤与文本过滤取交集；error 只留 host 级错误行；MIG 行在 unknown 下可见。
- **TUI-12 计数栏** — 文本/状态过滤后 `matched/total` 正确，total 为当前列表总行（error 行计入），不因滚动变化。
- **TUI-13 enter 可进入性** — GPU 行 enter 进详情并高亮来源卡；host error 行 enter 进错误详情；spinner 行 enter 无操作；不 panic/不额外 ssh。
- **TUI-14 r 刷新/generation** — 按 `r` 后旧 generation 结果被丢弃（延迟返回的旧数据不得覆盖新数据）；从 Detail 按 r 返回列表；探测中的行 spinner 重置；无重复行。
- **TUI-15 q 取消/恢复** — q 后进程 ≤2s 退出；fake/子进程全消失；`alternate_on=0`；`stty` 恢复 `icanon echo`；无 panic/backtrace。
- **TUI-16 ctrl+c** — 与 TUI-15 相同；Filter/Detail 态下也退出。
- **TUI-17 Filter 键语义** — `esc` 第一次清空查询、第二次退出 Filter；`enter` 提交并回 Normal 且保留过滤；字符追加实时过滤；`q` 在 Filter 中是输入字符而非退出。
- **TUI-18 Help 浮层** — `?` 打开帮助；关闭键（`?`/`esc`/`q` 的实际行为记录）；关闭后回到原 Normal 状态且不丢选择/过滤。§8.2 未定义 Help 的关闭键，记 AMBIGUOUS。
- **TUI-19 Detail 返回保留状态** — Detail 中 `esc`/`q` 返回列表，过滤串、状态过滤、排序、选中 host 保留；`r` 例外（刷新全部并回列表）。
- **TUI-20 小终端** — 80x24、40x10 下不 panic；列截断而非换行破坏布局；内容不足一屏不显示滚动条。
- **TUI-21 panic/终端恢复** — 构造渲染期 panic 或发送致命信号；panic hook/Drop 保证离开 alternate screen 并恢复 raw mode；R 可有 panic 信息但终端必须可用。

### 3.8 详情页（§8.5，R5）

- **DET-01 主机抬头** — ok host 显示 alias、resolved_alias、transport_used、duration_ms、probed_at；第二行 hostname、load1、nproc。
- **DET-02 完整性提示** — `fields_complete=false` 显示 `! 部分 GPU 字段缺失`；`processes_complete=false` 显示 `! 进程列表不完整`；两者 true 时不显示。
- **DET-03 GPU 表** — 列出全部 GPU（不受列表过滤影响），含 IDX/MODEL/FREE-TOTAL/UTIL/TEMP/STATE；来源卡高亮；unknown 附带 reason（mig/compute_prohibited/no_process_data）。
- **DET-04 SELECTED GPU** — UUID、compute_mode、driver、memory used/total；tier-2 为 null 时显示 `—`；不得显示 “null”/0 代替。
- **DET-05 进程表** — 该机全部 compute 进程，含 GPU/PID/USER/PROCESS/MEM；GPU 列能对应到卡；多卡进程都列出；长名称截断。
- **DET-06 processes=null 文案** — 显示“进程查询失败（不代表无进程）”；不得显示“无计算进程”。
- **DET-07 processes=[] 文案** — 显示“无计算进程”。
- **DET-08 error 变体** — 隐藏 GPU/SELECTED/进程区；显示 error.kind、error.message、retryable；不得显示空 GPU 表。
- **DET-09 no_gpu 变体** — 显示“未检测到 GPU”；不显示 SELECTED/进程表；host 仍为 ok 样式。
- **DET-10 MIG 变体** — GPU 表标 `unknown (MIG)`/reason=mig；不虚构实例；进程区按实际数据/空处理。
- **DET-11 详情不额外 ssh** — 进入详情前后 L 的调用次数不变；滚动/切卡也不新增。
- **DET-12 滚动与截断** — ↑↓/j/k 单行，PgUp/PgDn 翻页；长字段截断不横向滚动；内容不足一屏无滚动条。

### 3.9 doctor / init / print-config（§9，R1/R3）

- **DOC-01 健康配置** — `doctor` E=0；输出含配置解析、preset 校验、ssh 版本、transport 判定四类信息；不探测。
- **DOC-02 坏配置** — 语法/类型错误：E 非 0，R/O 指明文件与键，无 panic。
- **DOC-03 别名/映射校验** — `ssh -G` 失败或 remote_aliases 缺失：E 非 0，明确指出 preset+host；不得只报 “invalid config”。
- **DOC-04 ssh 二进制** — `ssh_binary` 不存在/不可执行：E 非 0，可读提示；`ssh -V` 失败单独提示；不得 panic。
- **DOC-05 ssh -G 解析** — 显示每 host 的最终 hostname:port；`proxyjump/proxycommand` 分类与 §5.3 一致。
- **DOC-06 每 preset transport** — direct/remote/auto 的判定与最终别名/host:port 可见；auto 显示 TCP 探测结果与缓存。
- **DOC-07 --probe** — 真连并报告 nvidia-smi 是否存在、驱动版本、compute-apps 权限、MIG 状态；失败 host 有分类错误；E 规则记录（§9 未定义 doctor 退出码）。
- **DOC-08 BatchMode 提示** — fake `auth`（模拟需要密码或 2FA 的主机）时，doctor 明确提示 BatchMode 下无法通过，而不是普通 timeout。
- **DOC-09 doctor 只读** — 运行前后配置文件 mtime/hash 不变；不写 `~/.ssh`；不创建 ControlMaster（或至少不改配置）。
- **DOC-10 --print-config 脱敏** — 输出生效配置（含 transport 判定）；不得含私钥、口令、token、`IdentityFile` 内容；`ssh_binary`/路径可显示。
- **DOC-11 --print-config + auto** — 显示 auto 的 direct/remote 判定、最终 host:port、缓存状态；与 CFG-10/11 一致。
- **DOC-12 init 创建** — 空 `XDG_CONFIG_HOME` 下 `init` 创建目录与 config.toml；E=0；文件可被 `presets validate` 解析；权限合理（目录/文件不 world-writable）。
- **DOC-13 init 不覆盖** — 已存在自定义内容时再 `init`：文件字节不变（hash 相同）；E 与提示记录（§4:201 只说不覆盖，未定义退出码，记 AMBIGUOUS）。
- **DOC-14 init 路径语义** — `XDG_CONFIG_HOME` 生效；`--config` 是否影响 init 记录；不得写到真实 HOME。
- **DOC-15 presets list/validate** — list 输出全部 preset 名称；validate 对合法/坏配置给出正确退出码与可读信息；不探测。

### 3.10 安全与稳健（§10，R1/R2）

- **SEC-01 远端只读** — 检查 fake 收到的 stdin：只含 `nvidia-smi/hostname/cat /proc/loadavg/nproc/ps`；不含 `sudo`、重定向写文件、`rm`、`curl`。
- **SEC-02 参数不经本地 shell** — `--host '$(touch /tmp/gpufree-review/pwn)'`、`--host 'a; touch ...'`、反引号、`|`；本机不得创建文件；host 作为一个 argv 传给 ssh（或被明确拒绝）。
- **SEC-03 前导 dash 防 option 注入** — `--host '-oProxyCommand=touch /tmp/gpufree-review/pwn'` 与 `--host=-oProxyCommand=touch /tmp/gpufree-review/pwn` 必须被拒绝（E=2）或以非选项方式传递；不得让 ssh 解释为选项。§5.1:220 说不用 `--`，因此实现必须自行校验。
- **SEC-04 BatchMode** — 所有 ssh 调用含 `BatchMode=yes`；伪造需要密码的场景不出现本地密码提示/挂起；快速返回 `ssh_auth`。
- **SEC-05 host key** — 不出现 `StrictHostKeyChecking=no`/`UserKnownHostsFile=/dev/null`；`Host key verification failed` → `ssh_hostkey`，不静默接受。
- **SEC-06 密钥/敏感信息** — `doctor`、`--print-config`、错误 message、TUI 均不打印私钥/口令/密钥；R 中无 `BEGIN ... PRIVATE KEY`。
- **SEC-07 输出上限** — 引用 SSH-22；超 8 MiB 必须 kill，不得把巨量内容写入 JSON/TUI。
- **SEC-08 终端控制字符** — fake 返回 hostname/GPU/进程名含 ESC/OSC/换行；TUI 和表格不得让远端内容执行终端控制序列（记录实际；§10 未明确要求，但属稳健风险）。
- **SEC-09 broken pipe** — `$GF --json | head -c 64`；不得 panic/打印 backtrace；退出码可非 0，但进程应正常收尾且无孤儿。
- **SEC-10 对抗输入不 panic** — 超大 host 列表、超长 alias、二进制 hosts-file、畸形 CSV/配置；不得 panic/abort/死循环；错误可读。

### 3.11 表格/颜色/输出约束（§3.2，R2）

- **TBL-01 非 TTY 纯文本** — pipe 下无 ANSI/alternate screen；表格含 host/GPU/state；E 按 CLI 规则。
- **TBL-02 颜色模式** — `--color always` 在 pipe 下产生 ANSI；`never` 无 ANSI；`auto` 在 pipe 无 ANSI、TTY 有（或文字状态列）。
- **TBL-03 NO_COLOR** — 设 `NO_COLOR=1` 时 `auto` 无 ANSI；与 `--color always` 同时存在时记录优先级（§3.2:155 只写“尊重”，记 AMBIGUOUS）。
- **TBL-04 表格变体** — error host 行、no_gpu 行、`--free-only` 过滤后的表格均正确，不显示空 GPU 行；TUI 不适用。
- **TBL-05 宽字符/截断** — 中文/emoji GPU 名、超长 host 名不破坏列对齐/不 panic；按显示宽度或字节截断均可，但不得产生非法 UTF-8。

---

## 4. 规格歧义与矛盾（需协调者/实现者确认）

以下按“若实现选择任一合理解释，不单独判 OBJECT；但必须记录并对照”的方式处理。`DESIGN.md` 行号为准。

| ID | 位置 | 问题 | 本计划的判定口径 |
|---|---|---|---|
| A1 | `DESIGN.md:442` vs `392-435`、`393-420`、`405-419` | “可空数值用 null，键始终存在”与示例冲突：error host 示例缺 `hostname/load1/cpus/fields_complete/processes_complete`，ok host 示例无 `note`，GPU 示例 free 无 `reason`。 | 以 442 为契约断言“文档化键始终存在、N/A 为 null”；示例缺键记为文档矛盾，不要求实现同时满足两者。 |
| A2 | `DESIGN.md:412-414` vs `371` | 示例 `memory_total=143771, used=1, free=143167`，`total-used=143770`，free 不是简单差值；两次采样只规定 `max(used)`，未定义 `memory_free_mb` 取哪个样本。 | 要求 `0≤free≤total` 且与实现声明的样本来源自洽；记录 free 是 min(free) 还是 total-max(used)。 |
| A3 | `DESIGN.md:361` vs `363-365` | `compute_mode=Prohibited` 列在 unknown，但“存在进程/超阈值”列在 busy；两者同时成立时优先级未定义。 | 按列表顺序预期 unknown；若实现 busy，记 AMBIGUOUS。MIG 同理（unknown 优先）。 |
| A4 | `DESIGN.md:126` vs `449` | `--free-only` 说“输出里只保留有空卡的 host”，但只定义 `hosts_shown` 计数；未说 `hosts` 数组是否过滤、error host 是否保留、summary 是否重算、退出码是否受影响。 | 断言数组只留 free host、errors 移除、`hosts_shown`=数组长度；退出码仍按 3.1 规则。记录实现。 |
| A5 | `DESIGN.md:579` vs `259` | “单 host 输出上限 8 MiB”与“stdout/stderr 各一个读取线程，累计到 8 MiB”未说清是每流 8 MiB 还是两流合计；超限错误 kind 未定义。 | 至少断言“不会无界读取/不会背压死锁”，并记录是每流还是合计；超限 kind 可为 `parse_error`/`unknown`/专用错误，记录。 |
| A6 | `DESIGN.md:561-570`、`201` vs `140-145` | §3.1 退出码只覆盖探测；doctor 不健康、init 已存在文件、presets validate 失败的退出码未定义。 | 断言 doctor 健康=0、不健康非 0；init 不覆盖且不破坏文件；具体码记 AMBIGUOUS。 |
| A7 | `DESIGN.md:482-497` | 声明 `Help` 状态，但没有 Help 态的按键表（如何关闭、q 是否退出）。 | 断言 `?` 可开；关闭键接受 `?`/`esc`/`q` 中至少一种且不改变过滤/排序；记录。 |
| A8 | `DESIGN.md:200` vs `143-144` | auto 直连失败且 remote 映射缺失时“同样报错”，但未说这是配置错误 E=2 还是该 host error E=1/0。 | 预检可发现时应 E=2；运行期发现时记录实际（2 或 host error），记 AMBIGUOUS。 |
| A9 | `DESIGN.md:118`、`125` | `--format table` 在 TTY 下是否强制表格、覆盖 TUI 未定义。 | 预期表格；若进 TUI 记 AMBIGUOUS。 |
| A10 | `DESIGN.md:118`、`522` | “TTY 下”未定义看 stdin、stdout 还是两者；stdin 为 pipe、stdout 为 TTY 时行为不明。 | 断言只有 stdin+stdout 都是 TTY 才进 TUI，避免读 /dev/tty 挂死；记录。 |
| A11 | `DESIGN.md:418`、`445`、`554` | JSON 中 `processes` 元素的对象键/类型从未定义（示例只有 `[]`；详情页只给列名）。 | 只要求键集稳定、可文档化、与详情列对应；具体键名记录，不据未定义字段判 OBJECT。 |
| A12 | `DESIGN.md:397-398` | `probed_at` 是探测开始还是完成时间未定义；与 `duration_ms` 的关系不明。 | 断言 RFC3339、不晚于 generated_at；记录 start/end。 |
| A13 | `DESIGN.md:171-172` | `sample_interval` “不低于 NVML 刷新窗口”没有具体下限；`samples=0`、`interval=0` 未定义。 | 断言 0/负值被拒或钳制且不死循环；最小间隔数值记录。 |
| A14 | `DESIGN.md:331` | “硬上限 16”未说超限是配置错误还是钳制；`parallel=0` 未定义。 | 断言不建 >16 线程、不死锁；具体错误/钳制记 AMBIGUOUS。 |
| A15 | `DESIGN.md:155`、`501` | `NO_COLOR` 与 `--color always` 同时存在时优先级未定义。 | 记录实际；若 `always` 覆盖 `NO_COLOR`，记 AMBIGUOUS 而不是缺陷。 |
| A16 | `DESIGN.md:477` vs `507-511` | 每机计数“不受 / 和 f 影响”，但过滤是行级还是 host 级未定义；若某 host 只有部分 GPU 命中，header/其他行是否显示不明。 | 断言计数用完整 GPU 列表；行级过滤行为记录。 |
| A17 | `DESIGN.md:451` | “按 preset/参数顺序输出”未说重复 host 是否去重、hosts-file 重复如何处理。 | 断言顺序稳定、结果可解释；去重与否记 AMBIGUOUS。 |
| A18 | `DESIGN.md:444` | no_gpu 只说 `status/gpus/note`，未定义 `fields_complete/processes_complete` 及 host 级其他空值。 | 记录实际；不得因此判错。 |
| A19 | `DESIGN.md:448` vs `199` | `transport_used` 是“最终实际选择”，但 `transport=remote` 的映射别名若本身没有 ProxyJump，实际链路是 direct；该报 remote 还是 direct 未定义。 | 记录实现（请求模式 vs 实际链路）；两种都可解释。 |
| A20 | `DESIGN.md:403`、`445` vs `362` | `processes_complete=false` 是否必然导致 GPU unknown？§6 只说“进程查询失败且低于 busy 阈值”才 unknown；PS 用户名失败是否算“进程查询失败”未定义。 | PROC rc≠0 → null + 低负载 unknown；仅 PS 失败 → 列表保留、user=null，状态按阈值；记录。 |
| A21 | `DESIGN.md:123`、`121` | hosts-file 的 `#` 是否支持行尾注释、逗号串是否 trim、CRLF 是否处理未定义。 | 至少支持整行注释/空行/前后空格；行尾注释与 CRLF 记录。 |
| A22 | `DESIGN.md:220`、`578` | 明确不使用 `--`，但没有要求拒绝以 `-` 开头的 host；存在 ssh 选项注入面。 | 安全断言：前导 `-` 必须 E=2 或被安全传递；若实现接受并注入，判 OBJECT（安全缺陷，非歧义）。 |
| A23 | `DESIGN.md:358` vs `346` | §6 说“nvidia-smi 失败 → GPU unknown”，§5.4 把 GPU rc≠0 列为 host 级 `nvidia_smi_failed` 错误；两者如何映射到 host status/gpus 未定义。 | 断言失败 host 为 `status=error`/`gpus=[]`（无 GPU 行可标 unknown）；若实现造 unknown 行，记录并对照。 |
| A24 | `config.example.toml:18` vs `DESIGN.md:202` | 示例在 `[settings]` 下注释 `# prefix`，设计稿明确 `settings` 里没有 prefix、只在 preset 级。 | 以 DESIGN 为准：settings.prefix 应被拒绝/忽略；示例注释是文档矛盾。 |
| A25 | `DESIGN.md:171` vs `291-302` | 设计说次数与间隔由 Rust 模板写入，但 §5.2 示例脚本写死 `while ... -lt 2` 与 `sleep 1`。 | 断言实际脚本随配置变化；示例视为伪代码。 |

---

## 5. 最高风险区域（执行时优先）

1. **ssh 进程生命周期与取消**（SSH-14..25）：孤儿/僵尸、pgid 复用误杀、D 状态 reaper、8 MiB 上限、SIGINT/TUI q。安全与挂死风险最高，且最难黑盒复现。
2. **JSON 契约**（JSN-01..20）：键始终存在 vs 示例缺键、null/[]、`memory_free_mb` 不自洽、`--free-only` 数组语义。脚本消费者会直接踩坑。
3. **状态判定优先级**（STA-01..14）：MIG/Prohibited/关键字段缺失/processes=null/两次采样最差，以及 unknown 与 busy 的先后。
4. **远端数据解析**（PRS-05..19）：降级查询、RC、`[N/A]`/单位、逗号进程名、两次采样合并、畸形 CSV。远端数据不可控，解析器最易 panic/误判。
5. **transport/mux/auto**（CFG-08..14、SSH-03..10）：`ControlPersist=0` 陷阱、代理分类、双池限流、auto 缓存与映射缺失。
6. **TUI 终端恢复与详情**（TUI-15..21、DET-01..12）：raw mode/alternate screen 残留、spinner 永久、每机计数与详情变体。
7. **安全面**（SEC-01..10）：前导 `-` host 的 ssh 选项注入、终端控制字符注入、`--print-config` 脱敏。

---

## 6. 后续执行流程（实现交付后）

1. 等待协调者确认实现完成；按 §1 冻结工作树并构建。
2. 先跑冒烟：CLI-01/02/11、JSN-01、STA-01、PRS-03；若冒烟失败，先报告阻塞，不把半成品铺开判 180 项。
3. 按风险顺序执行：SSH → JSN → STA/PRS → CFG → TUI/DET → DOC → SEC/TBL。每项记录 PASS/OBJECT/AMBIGUOUS/NOT-RUN。
4. 对每个 OBJECT：
   - 最小复现命令（可直接复制）；
   - 期望 vs 实际（stdout/stderr/exit/时间/进程）；
   - `file:line` 证据（源码定位，不改代码）；
   - 影响面（R1–R5、安全、脚本契约）。
5. 写 `docs/reviews/codex-m1-m3-review.md`：概览、按 ID 的 PASS/OBJECT 表、OBJECT 详情、歧义处理、未覆盖/无法验证项（如真实 MIG、真实 D 状态、真实跳板）、最终结论。
6. 结论规则：
   - **PASS**：所有可执行检查通过，或剩余项仅为文档歧义且实现选择合理。
   - **OBJECT**：任一硬契约失败（退出码、JSON schema、状态判定、进程泄漏、安全注入、终端不恢复）。
   - **BLOCKED**：实现未完成/工作树持续变化，无法形成稳定结论。
7. 不在执行阶段修改实现；发现的问题只报告，不代修。

---

## 7. 检查数量

本计划共 **182** 条编号检查（CLI 20、CFG 19、SSH 25、PRS 21、STA 14、JSN 20、TUI 21、DET 12、DOC 15、SEC 10、TBL 5），其中多条为 3–10 个子用例的矩阵；执行时按子用例逐项记录，任何子用例失败即使该条 CHECK 为 OBJECT。
