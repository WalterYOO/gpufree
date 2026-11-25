# gpufree 设计稿 v1（Rust）

> 一条命令看清一组服务器的 GPU 空闲情况：`--json` 给脚本，TTY 下开一个可检索的终端面板。

- 版本：v1.2
- 日期：2026-11-25
- 状态：设计定稿，待实现
- 变更：用户指定 Rust；进程管理、TUI、依赖选型随之调整，SSH 协议、空闲判定、JSON schema、v1 范围不变
- v1.2 变更：用户评审补充 TUI 每机空闲小计与单机详情页（R5 / §8.1 / §8.5 / D20）；探测层与 JSON 不变

---

## 0. 需求与验收

| # | 需求 | 验收方式 |
|---|---|---|
| R1 | 按**预设列表**或**指定服务器列表**查看 GPU 空闲状态 | `gpufree --preset company` / `gpufree --host a --host b` 都能出结果 |
| R2 | 直接输出 **JSON** | `gpufree --json \| jq` 可用，schema 稳定 |
| R3 | **终端面板**模式 | TTY 下 `gpufree` 进入全屏面板，逐台流式刷新 |
| R4 | 面板内**检索** | `/` 输入即过滤 host / GPU / 用户 / 进程；`f` 按状态过滤 |
| R5 | 面板内**单机空闲数与详情** | 列表按机器显示 `空闲/总数`；`enter` 进入详情，查看该机 GPU 全字段与计算进程 |

非目标：任务调度/排队、benchmark、远端常驻 agent、Web UI、告警推送、MIG 实例级容量、历史趋势、Windows。

---

## 1. 结论摘要

1. **语言**：Rust（edition 2024），ratatui + crossterm，单二进制。
2. **传输**：`std::process::Command` 调系统 `ssh`，复用 `~/.ssh/config` 的别名/跳板/密钥；远端只跑一段只读 `sh` 脚本，**一次往返**拿全 GPU + 进程 + 负载。
3. **并发**：固定 worker 线程 + `mpsc` 结果流；不用 tokio。ssh 是阻塞子进程，异步运行时不带来吞吐收益，只增加复杂度。
4. **两模一源**：探测产出统一的 `Snapshot`，JSON 与 TUI 共用，避免两套判定逻辑漂移。
5. **故障是一等公民**：每台 host 独立超时/取消，错误分类进 JSON；坏机器不拖垮整批。
6. **v1 只做五条需求**：cache / watch / ndjson / partial / 面板切 preset / 复制 推迟到 v2。

---

## 2. 架构

```
                 ┌──────────────┐
  clap flags ───▶│  config      │  TOML: presets / settings / transport
                 └──────┬───────┘
                        ▼
                 ┌──────────────┐      ┌──────────────────────────┐
                 │  scheduler   │─────▶│ probe workers (threads)  │
                 │  队列/取消    │      │  ssh → sh -s → 分节输出   │
                 └──────┬───────┘      └────────────┬─────────────┘
                        │  mpsc<HostResult>          │
                  ┌─────┴─────┐                     │
                  ▼           ▼                     │
              json/table     TUI ◀──────────────────┘
                        （crossterm 事件轮询）
```

目录：

```
gpufree/
├── Cargo.toml
├── src/
│   ├── main.rs              # clap 解析、模式选择、退出码
│   ├── cli.rs               # clap derive 定义（输入来源互斥）
│   ├── config.rs            # TOML + preset + transport 解析
│   ├── model.rs             # Host/Gpu/Snapshot、状态判定、summary
│   ├── probe/
│   │   ├── mod.rs           # 调度器、worker、结果通道
│   │   ├── ssh.rs           # Command、进程组、超时、kill、限额读取
│   │   ├── script.rs        # 远端脚本模板
│   │   └── parse.rs         # 分节 + CSV 解析
│   ├── output/
│   │   ├── mod.rs
│   │   ├── json.rs
│   │   └── table.rs
│   ├── tui/
│   │   ├── mod.rs           # 事件循环、App 状态机
│   │   ├── ui.rs            # ratatui 渲染
│   │   └── filter.rs        # 子序列匹配
│   └── doctor.rs
├── tests/
│   └── cli.rs               # assert_cmd + fake-ssh
├── testdata/fake-ssh
├── docs/
├── config.example.toml
├── Makefile
└── README.md
```

依赖（主版本锁定，Cargo.lock 入库）：

```toml
[dependencies]
anyhow      = "1"          # 应用层错误
chrono      = { version = "0.4", default-features = false, features = ["clock"] }
clap        = { version = "4", features = ["derive"] }
ctrlc       = { version = "3", features = ["termination"] }  # SIGINT/SIGTERM 清理
dirs        = "7"          # 配置/家目录
libc        = "0.2"        # kill(-pgid, SIGTERM/SIGKILL)
ratatui     = "0.30"       # 事件/后端用 ratatui::crossterm 重导出
serde       = { version = "1", features = ["derive"] }
serde_json  = "1"
thiserror   = "2"          # probe 错误枚举
toml        = "1"

[dev-dependencies]
assert_cmd  = "2"
predicates  = "3"
```

不引 tokio / async-std；不引模糊搜索库（子序列匹配自实现约 40 行）。
**不单独依赖 `crossterm`**：统一用 `ratatui::crossterm`，避免 crossterm 版本与 ratatui 后端不一致。

---

## 3. CLI 表面

```bash
gpufree                                  # TTY: 面板；非 TTY: 表格
gpufree --preset company                 # 预设
gpufree --host 4090-1 --host gpu06       # 可重复
gpufree --hosts 4090-1,gpu06,h20-5       # 逗号串
gpufree 4090-1 gpu06                     # 位置参数
gpufree --hosts-file -                   # 从 stdin 读，一行一个，# 注释
gpufree --json                           # --format json 的别名
gpufree --format table|json
gpufree --free-only                      # 输出里只保留有空卡的 host
gpufree --require-free                   # 无空闲卡 → 退出码 3
gpufree --strict                         # 任一 host 失败 → 退出码 1
gpufree --transport direct|remote|auto   # 覆盖配置
gpufree doctor [--probe]                 # 自检；--probe 真连
gpufree presets list | validate
gpufree --print-config                   # 生效配置（含 transport 判定）
gpufree init                             # 首次生成配置（不覆盖已有文件）
```

输入来源互斥：`--preset` / `--host` / `--hosts` / 位置参数 / `--hosts-file` 只能选一种，冲突报用法错误（clap `ArgGroup`）。没有任何来源且无 `default_preset` 时，打印示例并退出 2。

### 3.1 退出码

| code | 含义 |
|---|---|
| 0 | 至少一台 host 探测成功 |
| 1 | 全部 host 失败；或 `--strict` 下出现任一失败 |
| 2 | 用法 / 配置错误 |
| 3 | `--require-free` 且没有空闲 GPU |

判定优先级：全部失败 → `1`（优先于 `--require-free` 的 3）；`--strict` 下任一失败 → `1`；至少一台成功且 `--require-free` 无空闲 → `3`；其余成功场景 → `0`。

> 与审核意见的差异：审核建议「有结果但无空闲卡」一律返回 3。最终只在 `--require-free` 下返回 3——否则 `gpufree --json | jq` 在集群全忙时会莫名其妙失败。

### 3.2 输出约束

- 非 TTY 且未指定格式 → 纯文本表格（无 ANSI）。
- JSON 写 stdout；日志/警告一律 stderr。
- `--color auto|always|never`，尊重 `NO_COLOR`。

---

## 4. 配置

路径：`${XDG_CONFIG_HOME:-~/.config}/gpufree/config.toml`；`--config` 覆盖。完整示例见 `config.example.toml`。

```toml
default_preset = "company"

[settings]
parallel        = 8        # 直连并发
parallel_remote = 4        # 经跳板并发
connect_timeout = "6s"     # 每一跳建连超时
total_timeout   = "15s"    # 单 host 总超时
samples         = 2        # 采样次数，取最差；由 Rust 模板写入远端脚本
sample_interval = "1s"     # 不低于 NVML 的刷新窗口
ssh_binary      = "ssh"
mux             = true
control_persist = "60s"
color           = "auto"

[settings.free]
used_mem_max_mb = 512
util_max_pct    = 10

[presets.company]
description = "公司内网"
transport   = "direct"     # direct | remote | auto
hosts       = ["4090-1", "3090-0", "h20-5"]
# prefix    = "module load cuda/12.4 &&"   # 仅 preset 级

[presets.company.remote_aliases]
4090-1  = "4090-1r"
3090-0  = "3090-0-remote"
h20-5   = "h20-5-remote"
```

规则：

- `hosts` 写 **ssh config 的 Host 别名**；不重复写 IP/端口/用户/ProxyJump。
- `transport`：
  - `direct`（默认）：原样使用别名。
  - `remote`：必须能从 `remote_aliases` 找到映射，缺失即报配置错误——不静默回退到直连。
  - `auto`（opt-in）：`ssh -G <alias>` 取最终 `hostname`/`port`，用 `std::net::TcpStream::connect_timeout` 做 800ms 探测；通则 direct，不通则 remote 映射（映射缺失同样报错）；结果缓存 60s，`doctor`/`--print-config` 可见。
- 配置只读；`init` 仅在文件不存在时写入模板，不覆盖。
- `prefix` **只存在于 preset 级**（`settings` 里没有），拼在远端脚本前；同一 preset 的主机共用。

---

## 5. 探测协议

### 5.1 ssh 调用与进程管理

```
ssh -T -o BatchMode=yes \
    -o ConnectTimeout=6 -o ConnectionAttempts=1 \
    -o ControlMaster=auto -o ControlPath=<home>/.ssh/gf-%C -o ControlPersist=60s \
    <alias> sh -s
```

要点与依据：

- **绝不用 `-n`**：`-n` 把 stdin 指向 `/dev/null`，远端 `sh -s` 收不到脚本。脚本经 `Command::stdin(Stdio::piped())` 写入，只保留 `-T`。
- **不写 `--`**：ssh 在 host 之后不保证支持分隔符，直接 `ssh <alias> sh -s`。
- `BatchMode=yes`：不弹密码；失败即返回，归类 `ssh_auth`。
- `ControlPath` 用 `dirs::home_dir()` 拼绝对路径（ssh 不展开 `$HOME`）；`%C` 为 40 位 hash，路径远低于 macOS `sun_path` 104B 上限。
- **mux 默认开**：实测 `gpu06` 冷连 9.0s、复用 0.30s。
- `--no-mux` 或 `control_persist=0` 时，**不要传 `ControlPersist=0`**（ssh 里 0 表示永久驻留），而是改为 `-o ControlMaster=no -o ControlPath=none`。

Rust 侧实现：

```rust
use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};

let child = Command::new(&cfg.ssh_binary)
    .args(["-T", "-o", "BatchMode=yes", /* ... */])
    .arg(alias)
    .args(["sh", "-s"])
    .stdin(Stdio::piped())
    .stdout(Stdio::piped())
    .stderr(Stdio::piped())
    .process_group(0)          // 关键：自成进程组，代理跳板的子 ssh 也在组内
    .spawn()?;

// 立即交给 RAII guard：登记 pgid（锁内复检 cancel），此后任何提前返回/panic
// 都由 Drop 负责 kill + 有界 reap，不会漏 ssh 或留僵尸。
let mut guard = ChildGuard::new(child, &registry);
if cancel.load(Ordering::SeqCst) {
    return Err(ProbeError::Cancelled); // Drop 收尾
}

// ... 写 stdin、读输出、轮询 try_wait ...

// 正常收尾：try_wait 返回 Some 后立即 disarm，
// 禁止 Drop 再按 pgid kill —— 该 pgid 可能已被系统复用。
guard.disarm();
```

- **进程组**：`process_group(0)`（`std::os::unix::process::CommandExt`）让 ssh 及其 ProxyJump 子进程同组；超时/取消用 `libc::kill(-pgid, SIGTERM)`，200ms 后 `SIGKILL`。
- **超时与回收**：std 没有 `wait_timeout`；每 20ms `child.try_wait()`，到期走 SIGTERM→200ms→SIGKILL。SIGKILL 后再做**有界** `try_wait` 轮询（最多 2s）；若进程处于 D 状态仍不退出，记录错误并放弃该 Child，绝不裸 `wait()` 阻塞 worker。
- **stdin**：单独线程写入脚本后 `drop`，避免与 stdout 读取互相阻塞。
- **stdout/stderr**：各一个读取线程，`poll(fd, 200ms)` 循环 + 每轮检查 per-probe cancel；累计到 8 MiB 后标记超限并触发 kill，之后继续读到 EOF，避免子进程因管道写满而卡死。用 poll 而非阻塞 `read`，保证进程卡死/被放弃时线程也能自行退出。
- **取消**：`Arc<AtomicBool>` + `Arc<Mutex<HashSet<i32>>>` 记录活跃 pgid；TUI 按 `q` 时置位并对所有 pgid 发 SIGTERM/SIGKILL，worker 在取下一个 host 前检查标志。registry / 并发许可的 Mutex 要处理 poison（`unwrap_or_else(|e| e.into_inner())`），否则一次 panic 会让所有 worker 连锁崩溃。
- **spawn 与取消的竞态**：spawn 后在 registry 的同一把锁内复检 cancel；已取消则立刻 kill 且不插入，避免“刚起就被漏掉”。
- **Condvar 唤醒**：等待并发许可的 `wait` 谓词必须包含 `cancelled`；cancel 时 `notify_all`，否则队列空时 worker 永久阻塞、`q` 退不出去。
- **非 TUI 的 Ctrl-C**：子进程已 `process_group(0)`，默认 SIGINT 打不到它们。用 `ctrlc`（`termination` feature）安装 SIGINT/SIGTERM 处理器：置 cancel 标志、kill 全部 pgid，并 `notify_all` 唤醒等待许可的 worker；**不在信号线程里退出**，由主线程恢复终端（TUI）并正常退出，避免终端停在 raw mode。
- **mux master 不在 pgid 内**：它是独立后台进程，`q` 杀不到，也不应主动 `ssh -O exit`（会误关并发实例正在用的通道）。交给 `ControlPersist=60s` 自然过期；`q` 的语义是清掉**会话进程**，不是清 master。
  - 备选方案是「每进程唯一 ControlPath + 退出时 `-O exit`」，清理彻底，但会丧失跨调用的复用（`gpu06` 每次重新冷连 9s）。v1 选择共享路径 + 60s 过期；若将来需要强清理，再加实例引用计数的 `gpufree mux close`。
- **收尾顺序**：`try_wait()` 返回 `Some` 时**立即**从 registry 摘除 pgid（此时已 reap，pgid 可能被复用，晚摘会误杀新进程），再 join stdin/stdout/stderr 线程（正常路径读线程会读到 EOF；走 D 状态兜底时靠 poll + cancel 自行退出，不做无界 join）。任何路径都不遗留 ssh 会话进程。
- **D 状态兜底**：若 SIGKILL 后 2s 仍不退，先从 registry 摘除 pgid，再把 `Child` 交给 detached reaper 线程 `wait()`（避免僵尸）；读线程已在 cancel 后自行退出，**不做无界 join**。极端情况下泄漏的是 reaper 线程而非 worker，也不会挂死 JSON/TUI。
- **RAII ChildGuard**：spawn 后立刻包进 `ChildGuard { child, pgid, registry }`；登记 pgid 时在同一把锁内复检 cancel。`Drop` 负责 kill 进程组、有界 reap（必要时 detached reaper）、摘除 pgid。这样 catch_unwind、早返回、解析 panic 都不会漏 ssh 或留僵尸。
- **disarm**：正常收尾且 `try_wait` 返回 `Some` 后必须 `guard.disarm()`，否则 Drop 会再按 pgid kill，而该 pgid 可能已被系统复用，误杀无关进程。
- **单一收尾路径**：正常退出、超时、早取消三条分支都由 `ChildGuard` 收尾（`bounded try_wait -> 必要时 detached reaper`）；任何分支不允许在 kill 后裸 `return`。
- **完成判定**：不依赖“收齐 N 个结果”，而是等 `mpsc` 的 sender 全部 drop、`for msg in rx` 自然结束；worker panic 也不会让 JSON 模式永久挂死。

### 5.2 远端脚本（只读，一次往返）

```sh
echo '@@HOST'
hostname 2>/dev/null
cat /proc/loadavg 2>/dev/null
nproc 2>/dev/null
echo '@@NOGPU'
if ! command -v nvidia-smi >/dev/null 2>&1; then
  if [ -e /dev/nvidia0 ]; then echo 2; else echo 1; fi   # 2=有卡无工具，1=确实无卡
  exit 0
fi
echo 0
echo '@@MIG'
nvidia-smi -L 2>/dev/null | grep -q 'MIG' && echo 1 || echo 0
echo '@@GPU'
q='index,uuid,name,memory.total,memory.used,memory.free,utilization.gpu,temperature.gpu,compute_mode,driver_version'
fb=0; i=0
while [ "$i" -lt 2 ]; do          # 次数与间隔由 Rust 模板写入（默认 2 / 1s）
  i=$((i+1))
  out=$(LC_ALL=C nvidia-smi --query-gpu="$q" --format=csv,noheader,nounits 2>&1); rc=$?
  if [ "$rc" -ne 0 ] && [ "$fb" -eq 0 ]; then
    echo '@@FALLBACK=1'
    fb=1; i=0
    q='index,uuid,name,memory.total,memory.used,memory.free,utilization.gpu'
    continue
  fi
  printf '%s\n' "$out"; echo "@@GPU_RC=$rc"
  [ "$rc" -ne 0 ] && break
  [ "$i" -lt 2 ] && sleep 1
done
echo '@@PROC'
out=$(LC_ALL=C nvidia-smi --query-compute-apps=gpu_uuid,pid,process_name,used_memory \
      --format=csv,noheader,nounits 2>&1); rc=$?
printf '%s\n' "$out"; echo "@@PROC_RC=$rc"
echo '@@PS'
pids=$(nvidia-smi --query-compute-apps=pid --format=csv,noheader,nounits 2>/dev/null \
       | grep -E '^[0-9]+$' | paste -sd, -)
[ -n "$pids" ] && ps -o pid=,user= -p "$pids" 2>/dev/null
echo '@@END'
```

设计理由：

- **一次往返**：跳板链路上建连 5–9s，省一次就是省几秒。
- **采样合并**：`samples=2` 在远端循环，两次结果都带回，客户端取 `max(util)`、`max(used)`；不增加 ssh 次数。
- **RC 随段输出**：`2>&1` 混入的错误文本不会冒充数据行；客户端按 rc 分类，而不是「解析失败再试」。
- **MIG 单独探测**：核心 query 不含 `mig.mode.current`（老驱动会整条失败）；`nvidia-smi -L` 只判断是否启用 MIG。启用 MIG 的机器 v1 一律标 `unknown`（reason=`mig`）。
- **核心字段** 10 列；老驱动不认识某列导致整条失败时，远端降级为 7 列并输出 `@@FALLBACK=1`；`fields_complete=false`，tier-2 字段为 `null`。
- **tier-2 字段缺失不降级**：`temperature.gpu` / `compute_mode` / `driver_version` 为 `null` 只置 `fields_complete=false`，仍可正常判 free/busy；只有显存或利用率缺失才判 `unknown`。
- **PID 过滤非数字**：`--query-compute-apps=pid` 可能返回 `[N/A]` 或提示文本，`grep -E '^[0-9]+$'` 后再拼 `ps`。
- `@@NOGPU=1` 时脚本提前退出，缺失的 GPU/PROC/PS 段按空处理，不算解析错误。
- HPC 非交互 shell 无 nvidia-smi 时，用 preset 的 `prefix`（如 `module load cuda/12.4 &&`）解决。

### 5.3 并发与调度

- 先用 `ssh -G <alias>` 判定：`proxyjump` 或 `proxycommand` 有值且不为 `none` 才算 **proxied**，否则 **direct**（不看 transport 模式）。
- 两组各有**独立队列和 worker**：direct `parallel` 个、proxied `parallel_remote` 个。不能共用一个队列，否则 direct worker 会抢走远程 host，`parallel_remote` 形同虚设。
- 默认 8 + 4 = 12 个线程；硬上限 16。结果统一经 `std::sync::mpsc` 回传。
- 同一首跳（如 `hk-h20-05-remote`）用 `Mutex<HashMap<String, usize>> + Condvar` 实现每跳 6 个的软限（约 40 行，不引 semaphore crate）；许可用 RAII guard，drop 时递减并 `notify_one`（只减值不唤醒会让等待者睡死），panic/早退也能释放。
- 结果流式回传：TUI 每帧 drain 一次 channel；JSON 一直读到 channel 关闭（所有 worker drop sender）再输出，不依赖固定条数。
- **panic 兜底**：每个 host 的探测包在 `catch_unwind` 里；panic 转成该 host 的 `internal_error` 结果照常发送，worker 不退出，JSON 不会缺 host、TUI 不会留永久 spinner。
- `total_timeout` 默认 15s：实测经跳板成功建连 6.8–9.0s，6s 总超时会误杀。

### 5.4 错误分类

| kind | 触发 | retryable |
|---|---|---|
| `ssh_timeout` | stderr 含 `timed out` / 超时被杀 | true |
| `ssh_unreachable` | refused / no route / DNS 失败 | true |
| `ssh_auth` | `Permission denied` / 需要密码或 2FA | false |
| `ssh_hostkey` | `Host key verification failed` | false |
| `nvidia_smi_missing` | `@@NOGPU=2`（有 /dev/nvidia0 但无工具） | false |
| `nvidia_smi_failed` | GPU rc≠0 | true |
| `parse_error` | 分节结构不可解析 | false |
| `internal_error` | 探测线程 panic（catch_unwind 兜底） | true |
| `unknown` | 其他 | false |

---

## 6. 状态判定

只保留三态，与用户「空闲 / 非空闲」的直觉对齐：

```
unknown : nvidia-smi 失败、
          关键字段缺失（index/uuid/memory.*/utilization.gpu 任一）、
          启用 MIG（v1 不映射实例）、
          compute_mode == Prohibited（字段存在时，reason=compute_prohibited）、
          或进程查询失败且显存/util 未达到 busy 阈值（无法确认无进程）
busy    : 存在 compute 进程（含 Exclusive_Process / Exclusive_Thread 下的任意进程）
          或 used_mem > used_mem_max_mb (默认 512)
          或 util > util_max_pct (默认 10)
free    : 其余
```

- **必须无 compute 进程**才叫 free：一个 idle 的 `python`/notebook 进程仍占着卡，下一秒就可能被用。
- `--query-compute-apps` 只列计算进程，桌面 `Xorg` 之类的图形进程不会误伤。
- 两次采样取**最差**：`max(util)`、`max(used_mem)`；远端完成，不增加往返。
- 启用 MIG：`state=unknown`，`reason="mig"`；面板明确显示「MIG，无法判定」，不假装空闲。
- 输出始终带 `memory_free_mb`，调用方可用 `--free-mem-gte`（v1 可选）进一步收紧。
- **不设 partial**：需求只要「空闲/非空闲」，剩余显存已由 `memory_free_mb` 表达。

---

## 7. JSON（schema `gpufree/v1`）

```json
{
  "schema": "gpufree/v1",
  "generated_at": "2026-11-25T14:03:11+08:00",
  "preset": "company",
  "transport": "auto",
  "duration_ms": 7120,
  "summary": {
    "hosts_total": 5, "hosts_ok": 4, "hosts_error": 1, "hosts_shown": 4,
    "gpus_total": 17, "gpus_free": 8, "gpus_busy": 8, "gpus_unknown": 1
  },
  "hosts": [
    {
      "host": "gpu06",
      "resolved_alias": "gpu06r",
      "transport_used": "remote",
      "status": "ok",
      "probed_at": "2026-11-25T14:03:06+08:00",
      "duration_ms": 7060,
      "hostname": "gpu06",
      "load1": 4.0,
      "cpus": 192,
      "fields_complete": true,
      "processes_complete": true,
      "gpus": [
        {
          "index": 7,
          "uuid": "GPU-e7bc76b2-7685-0ec9-7ed9-c03061826c7c",
          "name": "NVIDIA H20-3e",
          "driver": "570.86.15",
          "mig": false,
          "compute_mode": "Default",
          "memory_total_mb": 143771,
          "memory_used_mb": 1,
          "memory_free_mb": 143167,
          "util_pct": 0,
          "temp_c": 29,
          "state": "free",
          "processes": []
        }
      ]
    },
    {
      "host": "3090-0",
      "resolved_alias": "3090-0-remote",
      "transport_used": "remote",
      "status": "error",
      "probed_at": "2026-11-25T14:03:05+08:00",
      "duration_ms": 6100,
      "gpus": [],
      "error": {
        "kind": "ssh_timeout",
        "message": "connect to host 192.0.2.23 port 22: Operation timed out",
        "retryable": true
      }
    }
  ]
}
```

约定：

- 可空数值用 `null`，键始终存在（`Option<T>`，不用 `skip_serializing_if`）。
- 错误 host 保留 `"gpus": []`，下游无需判空分支。
- 确实无卡的机器：`status="ok"`、`gpus=[]`、`note="no_gpu"`，计入 `hosts_ok`，不产生错误。
- `processes: null` 表示查询失败；`[]` 表示确认没有进程。
- `state` ∈ `free | busy | unknown`；非 free 的 GPU 可带 `reason`（如 `mig`、`no_process_data`）。
- `driver` 来自核心查询的 `driver_version` 列；降级查询时为 `null`。
- `transport` 是请求的模式；每个 host 的 `transport_used` 是最终实际选择。
- `summary` 只统计 ok host 上的 GPU；`hosts_shown` 是 `--free-only` 过滤后的条数。
- 字段只增不删；`schema` 主版本变化才 breaking。
- `hosts` 按 preset/参数顺序输出，便于 diff。
- `--utc` 切换时间戳时区。

---

## 8. 终端面板（TUI）

### 8.1 布局

```
┌ gpufree  company  remote ─────────── 12/14 hosts · 48 free ── 14:03:11 ┐
│ / python                                    f:free  s:sort  r:refresh  │
├──────────────────────────────────────────────────────────────────────────┤
│ HOST            IDX  MODEL                FREE/TOTAL   UTIL  TEMP  STATE  USERS│
│ ── company ───────────────────────────────────────────────────────────── │
│ 4090-1 1/1        0  RTX 4090             23.3/24.0G     0%   33°  free   —    │
│ gpu06 2/8         3  H20-3e               11.3/140.4G    0%   33°  busy   root │
│                   7  H20-3e              139.8/140.4G    0%   29°  free   —    │
│                  11  H20-3e               40.1/140.4G    0%   31°  free   —    │
│                   ⋮                                                          │
│ 3090-0            !  ssh_timeout (6.1s)                                       │
├──────────────────────────────────────────────────────────────────────────┤
│ q quit  / search  enter detail  r refresh  ? help                       │
└──────────────────────────────────────────────────────────────────────────┘
```

- **每机空闲数**：HOST 列只在当前排序下该主机连续组的首行写 `host 空闲/总数`（如 `gpu06 2/8`），组内后续行留空；`s` 切到显存排序后组可能不连续，每次 host 变化重新标注。计数基于该机完整 GPU 列表，不受 `/` 文本过滤或 `f` 状态过滤影响；只算 `state=free`，`unknown`（含 MIG）计入总数、不计入空闲。
- 主机级错误占一整行；仍在探测的行显示 spinner，且不可进入详情。

### 8.2 状态与按键

显式状态机：`Normal | Filter | Help | Detail`。

| 键 | Normal | Filter | Detail |
|---|---|---|---|
| `/` | 进入 Filter | — | — |
| 字符 | — | 追加查询串，实时过滤 | — |
| `esc` | 无操作 | 先清空，再退出 Filter | 返回列表 |
| `enter` | 打开选中行 Detail | 提交并回 Normal（保留过滤） | — |
| `↑` `↓` / `j` `k` | 移动选中行 | — | 滚动详情 |
| `PgUp` `PgDn` | — | — | 翻页 |
| `q` | 取消探测并退出 | 输入 `q` | 返回列表 |
| `ctrl+c` | 取消探测并退出 | 同左 | 同左 |
| `f` | 循环状态过滤 all→free→busy→unknown→error | — | — |
| `s` | 排序 host ↔ 空闲显存降序 | — | — |
| `r` | 重新探测 | — | 重新探测全部并返回列表 |
| `?` | 帮助浮层 | — | — |

`enter` 的可进入性：GPU 行进入该主机详情并高亮来源卡；host 级 error 行进入错误详情；仍在探测（spinner）的行不可进入。Detail 是上一轮探测的快照，不额外发起 ssh；`r` 会让旧 generation 作废，因此统一回列表。

配色：`free` 绿、`busy` 灰、`unknown` 黄、host 级 `error` 红；`NO_COLOR` / `--color never` 时改用文字状态列。

v1 明确不做：面板内切 preset、复制到剪贴板、多选刷新。

### 8.3 检索语义

- 过滤串对 `host / GPU 名称 / 用户名 / 进程名` 做大小写不敏感的子序列匹配（自实现）。
- 空格分隔多个关键词，全部命中才算匹配。
- 文本过滤与状态过滤取交集；计数栏实时显示 `matched/total`。
- 启用 MIG 的卡标注「MIG，无法判定」，检索仍可按 host 命中。

### 8.4 事件循环与渲染

- `ratatui::init()` 建终端；`Drop` guard + panic hook 保证 raw mode / alternate screen 一定恢复。
- 每帧：`ratatui::crossterm::event::poll(50ms)` → 用 `poll(0)` 循环排空已排队事件 → `try_recv` 批量 drain 结果 → 渲染。
- 没有 async runtime；worker 线程与 UI 线程只通过 `mpsc` 交互。
- 列表用 `ratatui::widgets::Table` + `TableState`；按可见高度切片行，只构造可见行，避免上千行全量布局。
- 启动即探测；未完成行显示 spinner（100ms 帧），行位置按 preset 顺序固定。
- `r`：`generation += 1`，cancel 旧一轮（置位 + kill 活跃 pgid），旧 generation 的结果直接丢弃。
- `q`：cancel 全部探测、kill 进程组、恢复终端后退出。
- 无自动刷新（`--watch` 推迟到 v2）。
- 非 TTY 自动退化为表格，不进入 TUI。

### 8.5 详情页（Detail）

```
┌ gpufree  company / gpu06 ───────────────────────────────────── 14:03:11 ┐
│ alias gpu06r · transport remote · 7.1s · probed 14:03:06               │
│ hostname gpu06 · load 4.0 / 192 cpus                                    │
├──────────────────────────────────────────────────────────────────────────┤
│ GPU                                                                      │
│  IDX  MODEL      FREE/TOTAL     UTIL  TEMP  STATE                        │
│ > 3   H20-3e      11.3/140.4G     0%   33°  busy                         │
│   7   H20-3e     139.8/140.4G     0%   29°  free                         │
├──────────────────────────────────────────────────────────────────────────┤
│ SELECTED GPU 3                                                           │
│  uuid GPU-3f0a…c91 · compute mode Default · driver 570.86.15             │
│  memory 131072 / 143771 MiB                                              │
├──────────────────────────────────────────────────────────────────────────┤
│ COMPUTE PROCESSES                                                        │
│  GPU  PID    USER   PROCESS                       MEM                    │
│   3   12345  root   python train.py               128.0G                 │
├──────────────────────────────────────────────────────────────────────────┤
│ ↑↓ scroll  PgUp/PgDn  esc back  r refresh                               │
└──────────────────────────────────────────────────────────────────────────┘
```

内容与规则：

1. **主机抬头**：`alias` / `resolved_alias` / `transport_used` / `duration_ms` / `probed_at`；第二行 `hostname` / `load1` / `nproc`。`fields_complete=false` 时追加 `! 部分 GPU 字段缺失`，`processes_complete=false` 时追加 `! 进程列表不完整`。
2. **主机级错误**：隐藏 GPU / SELECTED / 进程区，改为错误块，显示 `error.kind`、`error.message`、`retryable`；`note=no_gpu` 显示「未检测到 GPU」。
3. **GPU 表**：IDX / MODEL / FREE-TOTAL / UTIL / TEMP / STATE；`unknown` 附带 `reason`（如 `mig`、`compute_prohibited`、`no_process_data`）。从列表进入时高亮来源卡。
4. **SELECTED GPU**：UUID、compute_mode、driver、memory used/total；tier-2 字段为 `null` 时显示 `—`。
5. **计算进程**：列出该机全部 compute 进程，`GPU` 列标出所属卡，列 PID / USER / PROCESS / MEM。`processes=null` 显示「进程查询失败（不代表无进程）」，`[]` 显示「无计算进程」。进程名过长截断。
6. **滚动**：`↑↓` / `j k` 单行，`PgUp/PgDn` 翻页；不横向滚动，长字段截断；内容不足一屏时不显示滚动条。
7. **数据来源**：只渲染同一轮 `Snapshot`，不额外发起 ssh。`r` 重新探测全部并返回列表；`esc` / `q` 返回列表，保留过滤与排序状态。
8. MIG 主机照常在 GPU 表标 `unknown (MIG)`，详情页不虚构实例级信息。

---

## 9. doctor

`gpufree doctor`：

1. 配置解析、preset 校验（别名是否存在、`remote_aliases` 是否齐全）。
2. `ssh -V`、`ssh_binary` 可执行性、`ssh -G` 解析结果。
3. 每个 preset 的 transport 判定（direct/remote、最终 host:port）。
4. `--probe` 时真连：nvidia-smi、驱动版本、compute-apps 权限、MIG 状态。
5. 明确提示 `BatchMode` 下无法通过的主机（需要密码或 2FA）。
6. `--print-config` 输出生效配置（脱敏，无密钥）。

---

## 10. 安全与稳健

- 远端只执行只读命令：`nvidia-smi`、`hostname`、`cat /proc/loadavg`、`nproc`、`ps`；不写文件、不 sudo。
- `BatchMode=yes`；不静默接受 host key 变更。
- 参数经 `Command` 直传，不拼本地 shell 字符串；远端脚本固定，无用户输入插值。
- 单 host 输出上限 8 MiB；超限 kill。
- 不读取、不打印任何密钥内容。
- 取消/超时/退出都 kill 进程组，避免遗留 ssh 子进程。

---

## 11. 决策记录（三方分歧与裁决）

| # | 议题 | Claude 初稿 | codex 补充 | codex 审核 | 最终裁决 |
|---|---|---|---|---|---|
| D1 | 语言 | Go | Go | Go | **Rust（用户指定，2026-11-25 变更）**：ratatui + crossterm，其余评审结论继续适用 |
| D2 | `-n` / `--` | 使用 | 删 `-n` | 均删 | **删两者，stdin 传脚本** |
| D3 | mux | 默认开 | 同批无收益 | 60–120s | **默认开 60s；`--no-mux` 映射为 ControlMaster=no** |
| D4 | 超时 | total 6s | 偏紧 | 偏紧 | **connect 6s/跳，total 15s；try_wait 轮询** |
| D5 | 字段回退 | 客户端重试 | 远端回退 + RC | 分开查 MIG/进程 | **远端分节 + RC；MIG 单独探测** |
| D6 | free 判定 | 显存+util | 还需无进程 | 无进程 + 512MB + 10% + 两次取最差 | **三条件 + compute_mode 例外 + 两次采样（间隔 ≥1s）** |
| D7 | partial | 引入 | 可留可删 | 删 | **删** |
| D8 | 退出码 | 1/3 重叠 | 0=有 ok | 3=无空闲 | **0/1/2 + `--require-free` 时 3；全失败优先 1** |
| D9 | transport auto | 网段判断 | TCP 探测 | auto 仅 opt-in | **默认 direct；auto 显式开启** |
| D10 | 面板功能 | 切 preset/复制/多排序 | 砍切 preset | 全砍 | **v1 全砍** |
| D11 | watch/ndjson/cache | 有 | 限制 | 全砍 | **推迟 v2** |
| D12 | JSON | 基础字段 | 补 null/retryable | 补 transport/duration/空 gpus | **合并采纳** |
| D13 | 命名 | gpufree | 保留 | 保留 | **gpufree** |
| D14 | Rust 取消/回收 | — | 指出 CLI 无 SIGINT 清理 | 指出 spawn 竞态、D 状态卡死 | **ctrlc 清理；spawn 后锁内复检；SIGKILL 后有界 try_wait** |
| D15 | pgid 生命周期 | — | 指出 wait 后摘除会误杀复用 pgid | — | **try_wait 返回 Some 时立刻摘除** |
| D16 | 完成/panic | 固定条数 | 指出 worker panic 会挂死 | 指出 panic 会缺 host / 卡 spinner | **读到 mpsc 关闭 + catch_unwind 转 internal_error** |
| D17 | crossterm 依赖 | 直接依赖 | 指出版本冲突 | — | **用 ratatui::crossterm 重导出** |
| D18 | mux master | 未区分 | 指出 `-O exit` 会误关并发实例 | 建议每进程唯一路径 + `-O exit` | **共享路径 + ControlPersist 60s 过期，不主动关**（保留跨调用复用；强清理留 v2） |
| D19 | 线程池队列 | 共用队列 | — | 指出远程限流失效 | **direct/proxied 独立队列** |
| D20 | 面板单机信息 | 只有卡片行 | — | — | **HOST 组首行标 `空闲/总数`；`enter` 进入单机详情（GPU 全字段 + 进程），数据复用同一轮 Snapshot，不额外 ssh**（用户评审补充，2026-11-25） |

---

## 12. 测试

- **单元**：CSV 解析（带/不带单位、`[N/A]`、进程名含逗号、两次采样合并）、状态判定边界、summary、配置校验、transport 选择、子序列匹配。
- **回归**：fake-ssh 断言 stdin 收到的脚本非空（防 `-n` 回归）。
- **集成**：`ssh_binary=testdata/fake-ssh`（POSIX sh，755）模拟正常 / 超时 / 无 nvidia-smi / 驱动报错 / 部分字段缺失 / 海量输出；用 `trap '' TERM; sleep 30` 验证 SIGKILL 升级与总耗时上界。
- **清理**：对 CLI 进程发 SIGINT，断言 fake-ssh 及其子进程全部消失（无孤儿）；worker 提前退出/panic 时 JSON 仍能正常收尾。
- **golden**：注入固定时间，比较 `serde_json::Value`（键集合与顺序由结构体声明固定）。
- **TUI**：测 `App` 状态转移、过滤/排序纯函数、每机 `free/total` 计数；详情页在 ok / error / no_gpu / `processes=null` 四种输入下的文本渲染，不做整屏终端快照。
- **手工验收**：公司直连、外网 remote、断网、跳板超时、无 GPU 的 NAS、MIG 机器、HPC module 环境。

---

## 13. 里程碑

| 阶段 | 内容 | 完成判据 |
|---|---|---|
| M1 | config + probe + JSON + 表格 | `gpufree --host 4090-1 --host gpu06 --json` 正确；坏机器有分类错误 |
| M2 | TUI 列表 + 搜索/过滤/刷新/取消 + 单机详情 | 面板可用；每机显示 `空闲/总数`；`enter` 可看该机 GPU/进程详情；非 TTY 自动退化；`q` 能清掉 ssh 子进程 |
| M3 | doctor + init + transport auto + free-only | 外网可自动走 remote 别名；配置错误有可读提示 |
| v2 | cache / watch / ndjson / partial / 复制 / 切 preset | 按需再评估 |

---

## 14. 已实测环境事实

| 项目 | 结果 |
|---|---|
| `4090-1` 直连 | 0.3s，1×4090，24564 MiB，199 MiB 占用 |
| `gpu06`（经跳板） | 冷连 9.0s / mux 热连 0.30s，8×H20-3e |
| `gpu06` 完整探测（冷连 + 2 次采样） | 9.27s，仍在 total_timeout=15s 内 |
| 探测定稿脚本在 `4090-1` | 1.34s（含 1s 采样间隔），10 列字段齐全 |
| NAS `nfs-204-5` | 0.35s，`@@NOGPU=1`，不报错 |
| `3090-0` / `h20-5` / `gpu01` 当前不可达 | 6.1s 超时（ConnectTimeout=6） |
| `mig.mode.current` | 4090 返回 `[N/A]`，H20 返回 `Disabled` |
| `compute_mode` | 均为 `Default` |
| 跳板连接 | 不额外创建 ControlMaster socket（仅 1 个） |
| OpenSSH | 10.3p1 |
| 工具链 | rustc/cargo 1.100.0（edition 2024）；Go 1.27.3 仅作对照 |

采样间隔取 1s 的原因：NVML 的利用率是窗口平均值，间隔过短两次读数几乎相同；两次采样仍在一次 ssh 会话内完成。

---

## 15. v2 候选（明确不做在 v1）

1. TUI 首帧缓存 / `--cached` 离线查看。
2. `--watch` 自动刷新与 NDJSON 流式输出。
3. `partial` 状态与「剩余显存可用」过滤。
4. 面板内切 preset、复制 host、多选只刷新选中项。
5. MIG 实例级容量与归属。
6. 历史快照 / 「哪台一直空」趋势。
7. Windows 主机（`4090-1win`）。
