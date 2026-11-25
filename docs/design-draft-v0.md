# gpufree — 设计稿 v0（待 codex 补充 / agy 审核）

> **历史文档**：这是最初的 Go 版初稿，已被 [`DESIGN.md`](DESIGN.md) v1.1（Rust）取代；保留用于追溯评审过程。

> 一句话：一条命令看清一组服务器的 GPU 空闲情况，既能吐 JSON 给脚本，也能开一个可搜索的终端面板。

- 日期：2026-11-25
- 状态：DRAFT v0（我的初稿，尚未合并 codex / agy 意见）
- 作者：Claude（初稿）

---

## 1. 背景与目标

现有资产：`~/.ssh/config` 里约 171 个 Host，覆盖公司内网（4090-1、nfs-204-5、h20-5/6、3090-0、gpu01–gpu10 等）、跳板机链路（jump-a / jump-b / 4090-1-remote）和多套集群。现在想知道「哪台机器还有空卡」只能逐台 ssh + nvidia-smi，慢且不可脚本化。

目标：

1. **一条命令查一批机器**：按预设（preset）或临时指定的 host 列表，并行探测，几秒内出结果。
2. **双模输出**：`--json` 给脚本/监控；默认 TTY 下进交互面板。
3. **交互面板可检索**：按 host / GPU 型号 / 用户 / 进程名模糊过滤，支持只看空闲卡。
4. **复用现有 SSH 配置**：不重复实现跳板、密钥、ProxyJump；不引入远端常驻 agent。
5. **坏机器不拖垮整体**：不可达/无 nvidia-smi/驱动卡死都要有明确状态，单独超时。

非目标（明确不做）：

- 不做任务调度 / 排队 / 抢卡。
- 不做 GPU benchmark（那是 `_gpubench` 的事）。
- 不做远端常驻 daemon，不往服务器装东西。
- v1 不处理 MIG 实例级容量（只标记 `mig: true`）。
- 不做 Web UI / 告警推送。

---

## 2. 使用方式（预期 CLI 表面）

```bash
# 默认：TTY 下开交互面板（用默认 preset）
gpufree

# 指定预设 / 临时 host 列表
gpufree --preset company
gpufree --preset hk-h20
gpufree --hosts 4090-1,gpu06,h20-5

# 脚本模式
gpufree --json                      # 单个 JSON 文档
gpufree --json --preset company | jq '.hosts[] | select(.status=="ok")'
gpufree --format ndjson --watch 5s  # 每 5s 一行快照，便于流式消费

# 只关心有空卡的机器
gpufree --free-only --json
gpufree --require-free || echo "no free GPU"   # 无空闲卡时退出码非 0

# 诊断与缓存
gpufree doctor                      # 检查 ssh 连通性 / nvidia-smi / 配置
gpufree presets list
gpufree --cached                    # 展示上次快照（离线可用）
```

退出码：

| code | 含义 |
|---|---|
| 0 | 全部 host 探测成功（空闲卡数可以为 0） |
| 1 | 部分 host 失败（结果里对应 host `status=error`） |
| 2 | 用法 / 配置错误 |
| 3 | 全部 host 失败 |
| 4 | 指定了 `--require-free` 但没有空闲卡 |

---

## 3. 语言与依赖选型（初稿建议，待评审）

**建议 Go 1.27 + Bubble Tea**：

- 单文件静态二进制，`make install` 丢到 `~/.local/bin`，无运行时依赖。
- `bubbletea` + `lipgloss` + `bubbles/textinput` 做 TUI 成熟；并发模型适合「一堆 host 同时探测、边到边渲染」。
- 解析 CSV / TOML / JSON 都是标准库或一个轻依赖。

备选 Rust + ratatui：性能不是瓶颈，但编译/迭代慢一些；Go 的 goroutine + channel 对「逐 host 流式更新面板」更顺手。

依赖上限（希望 ≤ 5 个直接依赖）：

```
github.com/charmbracelet/bubbletea
github.com/charmbracelet/lipgloss
github.com/charmbracelet/bubbles
github.com/pelletier/go-toml/v2
```

CLI 解析用标准库 `flag` + 手写子命令，不引 cobra。

---

## 4. 项目结构（拟）

```
gpufree/
├── cmd/gpufree/main.go          # 入口：参数解析、模式选择
├── internal/config/             # 配置文件加载、preset 解析、默认值
├── internal/probe/              # ssh 调用 + 远端脚本 + nvidia-smi 解析
│   ├── ssh.go                   # ControlMaster / 超时 / context kill
│   ├── script.go                # 远端只读探测脚本
│   └── parse.go
├── internal/model/              # Host/GPU/Snapshot 类型、空闲判定
├── internal/output/             # json.go / table.go / ndjson.go
├── internal/tui/                # bubbletea model / view / keys / filter
├── internal/cache/              # ~/.cache/gpufree/last.json
├── docs/DESIGN.md               # 最终设计稿
├── config.example.toml
├── Makefile
└── README.md
```

---

## 5. 配置模型

路径：`${XDG_CONFIG_HOME:-~/.config}/gpufree/config.toml`，`--config` 可覆盖。

```toml
default_preset = "company"

[settings]
parallel     = 8            # 同时在跑的 ssh 数
timeout      = "6s"         # 单 host 总超时（含建连）
connect_timeout = "4s"
ssh_binary   = "ssh"
control_persist = "5m"      # ControlMaster 复用窗口
color        = "auto"       # auto|always|never

[settings.free]
used_mem_max_mb = 1024      # 已用显存 ≤ 此值才算闲
util_max_pct    = 5         # 瞬时利用率 ≤ 此值才算闲
samples         = 1         # 远端连续采样次数（>1 时取最大值判定）

[presets.company]
description = "公司内网 4090/H20/3090"
transport   = "auto"        # auto|direct|remote，见 §6.3
hosts       = ["4090-1", "h20-5", "h20-6", "3090-0", "nfs-204-5"]

[presets.hk-h20]
description = "香港 H20 十台"
transport   = "remote"
hosts       = ["gpu01", "gpu02", "gpu03", "gpu04", "gpu05",
               "gpu06", "gpu07", "gpu08", "gpu09", "gpu10"]

[presets.h100]
description = "H100 集群"
transport   = "direct"
hosts       = ["h100-0", "h100-1-root", "h100-2-root", "h100-3-root",
               "h100-4-root", "h100-5-root", "h100-6-root", "h100-7-root"]

# 可选：给 auto 模式用的直连↔远程别名映射
[presets.company.remote_aliases]
4090-1 = "4090-1r"
h20-5  = "h20-5-remote"
3090-0 = "3090-0-remote"
```

约束/取舍：

- preset 的值就是 **ssh config 里的 Host 别名**，不在这里重复写 IP/端口/用户/跳板。
- 不提供 `gpufree preset add` 之类的写配置命令（v1）；直接编辑 TOML，`gpufree presets list` 只读展示。
- 没有配置文件时：`--hosts` 必填；若 `~/.ssh/config` 存在，`gpufree doctor` 提示可用 host，但不自动探测全部 171 台。

---

## 6. 探测协议

### 6.1 传输：shell out 到 `ssh`

不自研 SSH 客户端。理由：ProxyJump、ProxyCommand、密钥、known_hosts、ControlMaster 全部免费复用，行为与用户手工登录完全一致。

基础参数：

```
ssh -T -n -o BatchMode=yes \
    -o ConnectTimeout=<connect_timeout> \
    -o ControlMaster=auto \
    -o ControlPath=~/.ssh/cm-gpufree-%C \
    -o ControlPersist=<control_persist> \
    <host> -- sh -s
```

要点：

- `BatchMode=yes`：绝不弹密码/口令交互，失败即返回。
- `-n` + `-T`：不占用本地 stdin、不申请远端 TTY（TUI 运行时尤其重要）。
- 远端脚本通过 **stdin** 传入（`cmd.Stdin = strings.NewReader(script)`），避免多层引号地狱，也兼容 zsh/bash/sh。
- 总超时用 `context.WithTimeout` + `exec.CommandContext`，到点 kill 整个进程组。
- `%C` 是连接四元组的 hash，ControlPath 短且不暴露主机名。

### 6.2 远端脚本（只读，一次往返）

```sh
echo '@@HOST'
hostname 2>/dev/null
cat /proc/loadavg 2>/dev/null
nproc 2>/dev/null
echo '@@GPU'
nvidia-smi --query-gpu=index,uuid,name,memory.total,memory.used,memory.free,utilization.gpu,temperature.gpu,driver_version,mig.mode.current \
           --format=csv,noheader,nounits 2>&1
echo '@@PROC'
nvidia-smi --query-compute-apps=gpu_uuid,pid,process_name,used_memory \
           --format=csv,noheader,nounits 2>&1
echo '@@PS'
pids=$(nvidia-smi --query-compute-apps=pid --format=csv,noheader,nounits 2>/dev/null | paste -sd, -)
[ -n "$pids" ] && ps -o pid=,user= -p "$pids" 2>/dev/null
echo '@@END'
```

设计理由：

- **一次 ssh 往返**拿全：GPU 列表、进程、进程属主、机器负载。ProxyJump 链路上每次建连 1–3s，必须省。
- `--query-compute-apps` 在无权限看别人进程时会返回空/报错，属于正常降级，不影响 GPU 列表。
- `@@SECTION` 分隔符解析简单；远端不需要 python/jq。
- 若完整 query 因老驱动不认识 `mig.mode.current` 而整条失败，**自动回退**到最小字段集 `index,uuid,name,memory.total,memory.used,memory.free,utilization.gpu` 重试一次。
- 采样 `samples > 1` 时在远端循环：`for i in $(seq N); do nvidia-smi ...; sleep 0.5; done`，仍只占一次连接。

### 6.3 传输模式（direct / remote / auto）

对应 CLAUDE.md §4 的现状：在公司直连 `4090-1`，在外面走 `4090-1r`（ProxyJump jump-a）。

- `direct`：直接使用 host 别名。
- `remote`：使用 preset 的 `remote_aliases` 映射（缺映射则原样使用）。
- `auto`：读本机网卡地址，命中 `settings.lan_cidrs`（默认 `192.0.2.0/24`、`198.51.100.0/24`）则 direct，否则 remote。

`auto` 只是选别名，不做「先试直连再回落」的双倍延迟探测。`gpufree doctor` 会打印判定结果，便于排查。

### 6.4 并发与限流

- 默认 8 并发（可配），`--parallel` 覆盖。
- 结果通过 channel 流式返回：TUI 每台到货即刷新；JSON 模式等全部结束后一次输出。
- ControlMaster 复用让同一批的第二次查询（刷新）通常 < 1s。

---

## 7. 空闲判定

单卡状态：

```go
type GPUState string // "free" | "partial" | "busy" | "unknown"
```

判定（默认值可配）：

```
free    : util <= util_max_pct  &&  used_mem <= used_mem_max_mb
partial : 既不是 free，也没有「满载」——仍有 ≥ 50% 空闲显存（给「勉强能跑小任务」用）
busy    : 其余
unknown : 字段缺失 / 解析失败
```

- 默认 `used_mem_max_mb = 1024`、`util_max_pct = 5`：H20 143G 上 1G 占用视为空闲；24G 4090 上也留出余量。
- 输出里始终带 `memory_free_mb`，调用方可用 `--free-mem-gte` 自行收紧。
- `samples > 1` 时取各次采样的**最大** util 与 **最大** used_mem 判定，降低「采样瞬间恰好 0%」的误判。
- Host 级附加信息（load1 / nproc）用于面板里判断「卡空但机器被 CPU 任务占满」。

---

## 8. JSON 输出（schema v1）

```json
{
  "schema": "gpufree/v1",
  "generated_at": "2026-11-25T14:03:11+08:00",
  "preset": "company",
  "transport": "remote",
  "summary": {
    "hosts_total": 5,
    "hosts_ok": 4,
    "hosts_error": 1,
    "gpus_total": 17,
    "gpus_free": 9,
    "gpus_partial": 2,
    "gpus_busy": 6
  },
  "hosts": [
    {
      "host": "4090-1",
      "status": "ok",
      "latency_ms": 812,
      "hostname": "4090-1",
      "load1": 0.42,
      "cpus": 32,
      "gpus": [
        {
          "index": 0,
          "uuid": "GPU-xxxx",
          "name": "NVIDIA GeForce RTX 4090",
          "driver": "550.54.15",
          "mig": false,
          "memory_total_mb": 24564,
          "memory_used_mb": 199,
          "memory_free_mb": 24365,
          "util_pct": 0,
          "temp_c": 35,
          "state": "free",
          "processes": [
            { "pid": 12345, "user": "walter", "name": "python", "used_mb": 18000 }
          ]
        }
      ]
    },
    {
      "host": "3090-0",
      "status": "error",
      "latency_ms": 4001,
      "error": {
        "kind": "ssh_timeout",
        "message": "connect to host 192.0.2.23 port 22: Operation timed out"
      }
    }
  ]
}
```

错误 `kind` 枚举：`ssh_timeout` / `ssh_unreachable` / `ssh_auth` / `ssh_hostkey` / `nvidia_smi_missing` / `nvidia_smi_failed` / `parse_error` / `unknown`。

约定：

- 字段一旦发布不删不改名；新增只加字段，`schema` 主版本才 breaking。
- `hosts` 按 preset 声明顺序输出，稳定可比对。
- 时间戳带本地时区；`--utc` 可切换。
- `summary` 是便利字段，可由 `hosts` 重算，方便 jq 一行取用。

---

## 9. 终端交互面板（TUI）

### 9.1 布局

```
┌ gpufree  company (remote) ─────────── 12/14 hosts · 48 free ── 14:03:11 ┐
│ / [filter: python            ]   f: all→free→busy→error   s: sort       │
├──────────────────────────────────────────────────────────────────────────┤
│ HOST         GPU                        FREE/TOTAL   UTIL  TEMP  USERS   │
│ ── company ───────────────────────────────────────────────────────────── │
│ 4090-1       0 RTX 4090                 23.8/24.0G    0%   35°  walter   │
│ 4090-1       1 RTX 4090                 12.0/24.0G   88%   61°  bob      │
│ h20-5        0 H20-3e                  140.4/140.4G   0%   32°  —        │
│ 3090-0       ! ssh_timeout (4.0s)                                       │
├──────────────────────────────────────────────────────────────────────────┤
│ q quit  / search  enter detail  r refresh  p preset  y copy  ? help      │
└──────────────────────────────────────────────────────────────────────────┘
```

### 9.2 交互

| 键 | 行为 |
|---|---|
| `/` | 进入模糊搜索（host / GPU 型号 / 进程名 / 用户名），逐字符实时过滤 |
| `esc` | 清空搜索 / 退出当前层 |
| `f` | 循环过滤：all → free → partial → busy → error |
| `s` | 循环排序：host / 空闲显存降序 / 利用率升序 / 名称 |
| `r` | 重新探测（保留过滤与滚动位置） |
| `enter` | 展开该 host 详情（进程 PID/用户/占用、load、驱动版本、错误原文） |
| `p` | 切换 preset（面板内直接换一组机器） |
| `y` | 复制当前 host 别名到剪贴板（osc52，失败则提示） |
| `?` | 帮助浮层 |
| `q` / `ctrl+c` | 退出 |

### 9.3 刷新与缓存

- 启动：先读 `~/.cache/gpufree/last.json` 立即画一帧（标注 `cached 3m ago`），同时后台探测，结果到达后原地替换。
- 手动 `r` 刷新；可选 `--watch 10s` 自动刷新（默认关，避免默默占连接）。
- 探测中显示 per-host 的 spinner，已完成的 host 先渲染，不阻塞整屏。
- 非 TTY（管道/CI）自动退化为表格输出，除非显式 `--json`；避免把 ANSI 写进文件。

### 9.4 颜色

- 空闲=绿、partial=黄、busy=默认灰、error=红。
- `--color never` / `NO_COLOR` 生效；lipgloss 自适应终端背景。

---

## 10. 缓存

- 路径：`~/.cache/gpufree/last.json`（`XDG_CACHE_HOME` 优先）。
- 内容即 §8 的 JSON + `cached_at`。
- `--cached`：只读缓存、不探测，缓存不存在则退出码 3。
- 每次成功/部分成功的探测后原子写入（tmp + rename）。

---

## 11. 安全与稳健

- 远端只执行只读命令（`nvidia-smi`、`hostname`、`cat /proc/loadavg`、`nproc`、`ps`），不写文件、不 sudo。
- `BatchMode=yes` 防止卡在密码提示；`StrictHostKeyChecking` 保持默认，host key 变更按 `ssh_hostkey` 报错而不是静默接受。
- 限制单个 host 输出大小（如 8 MiB）防止异常输出撑爆内存。
- 所有 shell 参数用 `exec.Command` 直传，不拼 shell 字符串。
- 不读取/打印任何密钥内容；`doctor` 只报告 ssh 退出码与 stderr 摘要。

---

## 12. 测试

- 单元测试：nvidia-smi CSV 解析（含 `[N/A]`、MIG、空进程、老驱动回退）、状态判定边界、TOML 配置、别名选择。
- 集成测试：用可执行 `fake-ssh` 脚本（`ssh_binary` 可配）模拟 ok / timeout / no-nvidia-smi / 部分 GPU 失败。
- TUI：model 层纯函数测试（过滤/排序/键位），不依赖真实终端。
- 手工验收清单：公司内网直连、外网 remote、断网、跳板机超时、无 GPU 主机。

---

## 13. 里程碑

| 阶段 | 内容 | 验收 |
|---|---|---|
| M1 | config + probe + JSON 输出 + 表格输出 | `gpufree --hosts 4090-1,gpu06 --json` 正确 |
| M2 | TUI 只读列表 + 搜索/过滤/刷新 | 面板可用，非 TTY 退化正常 |
| M3 | doctor / cache / auto transport / watch | 外网 + 缓存 + 诊断可用 |
| M4 | 打磨：排序、详情、复制、帮助、README | 日常替代手工 ssh |

---

## 14. 待评审的具体问题（给 codex / agy）

1. **语言**：Go + bubbletea 是否合适？有没有理由改用 Rust + ratatui？
2. **SSH**：ControlMaster 复用 + `sh -s` 传脚本是否有坑（ProxyJump、ControlPath 长度、macOS/Linux ssh 差异）？要不要改用一次 `ssh -O check` 预热？
3. **探测字段**：`mig.mode.current` 在老驱动上会让整条 query 失败——回退策略够不够？还有哪些字段该进/不该进？
4. **空闲定义**：`used_mem_max_mb=1024` + `util_max_pct=5` 的默认值是否合理？partial 这档有没有必要？
5. **JSON**：schema 字段有没有缺的（例如 GPU 的 `power.draw`、`clocks`、`ecc`）？退出码设计是否好用？
6. **TUI**：`/` 实时过滤 + `f` 状态过滤的组合是否够？要不要支持多选 host 后「只刷这几台」？
7. **配置**：preset 只读、靠手编 TOML 是否可接受？`transport=auto` 的判定方式（本地网段）是否可靠？
8. **范围**：有没有该砍的（partial / cache / watch / p 切 preset），或该补的？
9. **命名**：`gpufree` 是否合适（避免与 `gpustat` 混淆）？

---

## 15. 附：现有环境事实（供评审参考）

- 典型 host：`4090-1`（1×4090，直连 192.0.2.200:2222；外网走 `4090-1r` → jump-a/jump-b）、`gpu01–gpu10`（8×H20-3e，外网走 `gpuNNr` → jump-a）、`h100-0..7`（走 `jump-c` 跳板）。
- 实测 nvidia-smi 输出（gpu06）：
  `0, NVIDIA H20-3e, 1 MiB, 143771 MiB, 0 %`
- 实测不可达：`3090-0`、`h20-5` 在本机当前网络下 6s 超时 → 必须有 per-host timeout 与 error 呈现。
- 本机具备 Go 1.27.3、Rust 1.100、Python 3.14、uv 0.6.6。
