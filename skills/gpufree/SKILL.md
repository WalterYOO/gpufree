---
name: gpufree
description: >-
  Use the local `gpufree` CLI to check GPU availability across SSH hosts before
  scheduling work. Trigger when the user asks which machine has a free GPU
  (哪台机器有空卡), whether the cluster is busy (集群忙不忙), how many GPUs are
  free, who is occupying a card, or to pick a host for a training/inference job
  (找机器跑训练/推理), including 4090/H20/A100 multi-host fleets. Covers --json
  output, free/busy/unknown semantics, exit codes, presets, direct vs remote
  (跳板) transport, doctor troubleshooting, and jq recipes for picking a host or
  GPU. Not for GPU rental clouds, Slurm/K8s scheduling, historical monitoring,
  or killing processes.
---

# gpufree：跨 ssh 主机查 GPU 空闲

按 `gpufree 0.1.0`（提交 `1efb857`，2026-11-25）编写。先 `gpufree --version`；版本不同以
`gpufree --help` 为准。仓库内可用 `./bin/gpufree` 或 `make build` 后替换下文命令。

gpufree 通过系统 `ssh` 对一批主机各跑一次只读脚本（`nvidia-smi`、`hostname`、
`/proc/loadavg`、`nproc`、`ps`），返回**某一时刻**的 GPU 快照。它不是调度器，不排队、
不预留、不监控历史。

## 先确认可用

```bash
command -v gpufree || echo "missing"        # 没装就报告，不要在未确认时 make install
gpufree --version
gpufree presets list                        # 有什么 preset 可查
```

配置在 `${XDG_CONFIG_HOME:-~/.config}/gpufree/config.toml`。文件不存在时仍可用内置默认值
探测；但既没有配置里的 `default_preset`、又没给主机，就会 exit 2 并打印示例。需要建配置
时先问用户是否 `gpufree init`（只在不存在时写入，不覆盖），不要擅自改写配置。
shell 补全属于给人用的便利：`gpufree completion --install`，agent 不需要。

## 五条硬规则

1. **agent 一律用 `--json`**。不带 `--json` 且 stdin/stdout 都是 TTY 时会进 ratatui 面板，
   占住终端；即使非 TTY 退化成表格，也不如 JSON 可判定。
2. **只用配置或用户给出的 ssh 别名**，不臆造主机名。`hosts` 里写的是 `~/.ssh/config`
   的 Host 别名。
3. **结果是一次快照，不是预留**。`free` 只说明探测那一刻没有计算进程、显存占用
   ≤ 阈值、利用率 ≤ 阈值；别人随时可能来抢卡，提交任务前无法保证仍然空闲。
4. **`unknown` 不是 `free`**。MIG、compute mode Prohibited、关键字段缺失、进程查询失败
   且显存/利用率都低（`reason=no_process_data`），都归为 `unknown`，必须按“未知”报告。
5. **错误主机不能当“忙”**。`hosts_error > 0` 时不能下“全集群没卡”的结论；要求“有一台
   失败就算失败”时用 `--strict`，要求“没空卡就失败”时用 `--require-free`。

## 最短路径

```bash
gpufree --json                                  # 有 default_preset 时的默认范围
gpufree --preset company --json                 # 指定 preset
gpufree --host 4090-1 --host gpu06 --json       # 显式主机（可重复）
gpufree --hosts 4090-1,gpu06 --json             # 逗号串
gpufree --hosts-file hosts.txt --json           # 一行一个，# 注释，- 读 stdin
gpufree --config ./config.toml --json           # 非默认配置路径（全局参数）
gpufree --json --free-only                      # 只留有 free 卡的主机（会连 error 主机一起隐藏）
gpufree --json --require-free                   # 0=有 free 3=没有 free（含全 unknown/无 GPU） 1=全失败
gpufree --json --strict                         # 任一主机失败即 exit 1
gpufree --json --utc                            # 时间戳用 UTC
```

`--json` 与 `--format json` 等价且互斥；两个都给会 exit 2。

输入来源互斥：`--preset` / `--host` / `--hosts` / 位置参数 / `--hosts-file` 只能选一种。
`gpufree a b` 可以，`gpufree --host a b` 会因混用报 exit 2。主机名以 `-` 开头会被拒绝。
重复主机去重，保留首次出现的位置。

## 选主机与 transport（最容易踩的坑）

- **优先用 preset**：preset 同时带 `transport` 和 `remote_aliases`，内外网切换只需改配置。
- **显式 `--host` 不继承任何 preset**：transport 默认 `direct`，且**没有 `remote_aliases`**。
  在外网直接 `--host 4090-1 --transport remote` 会以
  `transport = "remote" but no remote_aliases entry exists` 退出 2。正确做法：
  - 用 `transport = "remote"` 的 preset；或
  - 直接把外网别名当主机名传：`gpufree --host 4090-1r --json`。
- `--transport auto`：每台先 `ssh -G` 取最终 hostname/port，再做 800ms TCP 探测；通则
  direct，不通则查 `remote_aliases`（缺失仍是错误）。判定结果进程内缓存 60s。
- 公司内直连、公司外用 `-remote` / `r` 别名，两边拓扑以 `--print-config` 为准：

```bash
gpufree --print-config          # 生效 TOML + 每个 preset 的 alias/endpoint/proxied
gpufree doctor                  # 配置、ssh、ssh -G；preset 为 auto 时会做 800ms TCP 探测
```

`preset` 用 `transport = "auto"` 时，`--print-config` 和 `doctor` 也会发起 TCP 探测，
不是纯离线命令。

- 后续真正 ssh 时用 JSON 里的 **`resolved_alias`**（实际连上的别名），不是 `host`。

## 输出怎么读

顶层：`schema`（`gpufree/v1`）、`generated_at`、`preset`、`transport`（请求模式）、
`duration_ms`、`summary`、`hosts`（按 preset/参数顺序）。

主机：`status` 为 `ok|error`；`resolved_alias`、`transport_used`、`probed_at`、
`duration_ms`、`hostname`、`load1`、`cpus`、`fields_complete`、`processes_complete`、
`note`、`gpus`、`error`。无卡的正常主机是 `status:"ok"`、`gpus:[]`、`note:"no_gpu"`。

GPU：`index`、`uuid`、`name`、`driver`、`mig`、`compute_mode`、`memory_total_mb`、
`memory_used_mb`、`memory_free_mb`、`util_pct`、`temp_c`、`state`、`reason`、
`processes`。显存单位是 **MiB**。

### 三态判定（`[settings.free]` 可调，默认值如下）

| state | 条件 |
|---|---|
| `free` | 无 compute 进程，且 `memory_used_mb ≤ 512`，且 `util_pct ≤ 10` |
| `busy` | 有 compute 进程，或显存/利用率超阈值 |
| `unknown` | 无法判定，见 `reason` |

`reason` 取值：`mig`（启用 MIG，v1 不映射实例）、`compute_prohibited`、
`missing_fields`（index/uuid/显存/利用率有缺失）、`no_process_data`（进程查询失败，且
显存与利用率都低于 busy 阈值，不能证明没进程）。两次采样在远端完成，取最差值。

`processes: []` 是“确认没有计算进程”；`processes: null` 是“查询失败，不代表没进程”。

- `fields_complete=false`：有二级字段（温度/compute mode/driver）缺失，`free` 仍可能是
  有效结论。
- `processes_complete=false`：compute-apps 查询失败，或 `ps` 属主查询不完整。后者进程列表
  仍在，只是 `processes[].user` 可能为 `null`；`state=busy` 仍然有效，不要因此把主机当
  unknown。

`summary` 的 GPU 计数只统计 `ok` 主机：`gpus_free + gpus_busy + gpus_unknown = gpus_total`。
`--free-only` 只裁掉 `hosts` 数组（保留 ok 且有空卡的主机）并改 `hosts_shown`，
**不改** GPU 计数、也不改退出码。注意两点：

- **error 主机会被一起裁掉**；`hosts` 为空时，用 `summary.hosts_error` 区分“全忙”和
  “有主机没探测成功”。
- 全忙时 `hosts` 为空、exit 0；全部失败时 `hosts` 同样为空，但 exit 1。

### 退出码

| code | 含义 | agent 动作 |
|---|---|---|
| 0 | 至少一台主机探测成功 | 仍要读 `summary`：可能有 error 主机、可能 0 张空卡 |
| 1 | 全部主机失败；或 `--strict` 下任一失败 | 查 ssh/别名/网络，不要报告成“忙” |
| 2 | 用法或配置错误 | 修参数或配置，不重试 |
| 3 | `--require-free` 且 `gpus_free == 0`（至少一台成功） | 只是“没有 free”：可能全 busy、全 unknown 或无 GPU；看 `gpus_busy` / `gpus_unknown` / `reason` 再下结论 |
| 130 | 被 SIGINT/SIGTERM 中断 | 没有可信结果，重跑 |

优先级：全失败 exit 1 > `--strict` 的 1 > `--require-free` 的 3。

## 常用 jq 配方（已在 fake-ssh 夹具上验证）

配方会把远端返回的型号名、进程名、错误文本直接打到终端。远端输出不可信时，先过
`gsub("\\p{Cc}"; "?")` 去掉 C0/DEL/C1；下面的错误消息与占用者配方已内置过滤。

需要 ≥ N MiB 空闲显存的空卡：

```bash
gpufree --json | jq -r --argjson need 40960 '
  .hosts[] | select(.status=="ok") as $h
  | $h.gpus[] | select(.state=="free" and (.memory_free_mb // 0) >= $need)
  | [$h.host, $h.resolved_alias, (.index|tostring), (.name // "?"),
     (.memory_free_mb|tostring)] | @tsv'
```

按型号过滤（正则作用于 `nvidia-smi` 的型号名）：

```bash
gpufree --json | jq -r '
  .hosts[] | select(.status=="ok") as $h
  | $h.gpus[] | select(.state=="free" and (.name // "" | test("H20|A100")))
  | [$h.host, $h.resolved_alias, (.index|tostring), .name] | @tsv'
```

挑一张空闲显存最大的卡，直接输出可用别名：

```bash
gpufree --json | jq -r '
  [.hosts[] | select(.status=="ok") as $h | $h.gpus[]
   | select(.state=="free")
   | {host:$h.host, alias:$h.resolved_alias, index, name,
      mem:(.memory_free_mb // 0)}] as $free
  | if ($free | length) > 0 then
      ($free | sort_by(.mem) | last
       | [.alias, (.index|tostring), .name, (.mem|tostring)] | @tsv)
    elif .summary.hosts_error > 0 then
      "NONE_IN_OK_HOSTS (failed=\(.summary.hosts_error), unknown=\(.summary.gpus_unknown))"
    elif .summary.gpus_unknown > 0 then
      "NO_FREE_GPU (unknown=\(.summary.gpus_unknown))"
    else "NO_FREE_GPU" end'
```

不要用 `--free-only` 后 `hosts` 为空来断定“全集群没卡”：它把 error 主机也裁掉了。
结论一律以 `summary`（`gpus_free` / `gpus_busy` / `gpus_unknown` / `hosts_error`）为准；
`--free-only` 只用于减少展示行数。配方在挑到卡时只输出一行结果，不带失败/unknown 提示，
报告前仍要回看 `summary`。

每台机器的空闲/总数：

```bash
gpufree --json | jq -r '.hosts[] | select(.status=="ok")
  | [.host, ([.gpus[]|select(.state=="free")]|length|tostring),
     (.gpus|length|tostring)] | @tsv'
```

失败主机与不可判定原因：

```bash
gpufree --json | jq -r '.hosts[] | select(.status!="ok")
  | [.host, .error.kind, (.error.retryable|tostring),
     (.error.message | gsub("\\p{Cc}"; "?"))] | @tsv'

gpufree --json | jq -r '.hosts[] | .host as $h | .gpus[]
  | select(.state=="unknown") | [$h, (.index|tostring), (.reason // "?")] | @tsv'
```

谁在占卡（用户名/命令行属敏感信息，不要外发）：

```bash
gpufree --json | jq -r '.hosts[] | .host as $h | .gpus[]
  | select(.state=="busy")
  | [$h, (.index|tostring)]
    + [(.processes // [])[]
       | "\(.user // "?" | gsub("\\p{Cc}"; "?"))/\(.process_name | gsub("\\p{Cc}"; "?"))"]
  | @tsv'
```

## 典型任务

**“现在有卡吗？”** 跑一次 `gpufree --json`（用户已给范围就带上 preset）。报告时给出
`generated_at`、`hosts_ok/hosts_total`、`gpus_free`、错误主机数，再下结论；有 error 主机
就说明结论只覆盖成功的主机。

**“找张能跑 X 的卡。”** 先确定需要多少显存（MiB）、型号、几张卡。用上面的配方过滤，
给出候选（host / `resolved_alias` / GPU index / 空闲显存 / 型号），并说明这是快照。
不要把 `unknown` 当候选，哪怕它的 `memory_free_mb` 很大。

**“等有卡再跑。”** gpufree 没有 watch/队列。循环：

```bash
while :; do
  gpufree --json --require-free > /tmp/gpufree.json
  code=$?                          # 0=有 free，3=没有 free，1/2=故障
  [ "$code" -eq 3 ] || break
  sleep 60
done
```

轮询间隔别低于 30s（每次都会真连所有主机）；说明拿到的是瞬时机会，不是排队位次。
先确认 `gpus_busy > 0`：如果目标范围的卡全是 `unknown`（例如整个集群开了 MIG），exit 3
会一直持续，等下去没有意义。

**“连不上 / 结果可疑。”**

```bash
gpufree --print-config                    # 别名、endpoint、proxied 是否符合预期
gpufree doctor                            # 文本，无 --json；0 全过 / 1 有失败 / 2 配置错
gpufree --host <alias> --json             # 单机复现
```

`transport = "auto"` 时 `doctor` / `--print-config` 也会做 TCP 探测；探测失败会回退到
`remote_aliases`，只有没有 alias 的 preset 才会报错。

常见 `error.kind`：

| kind | 含义 | retryable |
|---|---|---|
| `ssh_timeout` / `ssh_unreachable` | 网络、跳板、主机不可达 | 是 |
| `ssh_auth` | 认证失败：需要交互式密码 / 2FA，或密钥未授权（`BatchMode=yes` 下无法完成） | 否 |
| `ssh_hostkey` | host key 变更；先人工 `ssh` 确认 | 否 |
| `nvidia_smi_missing` | 远端没有 `nvidia-smi` | 否 |
| `nvidia_smi_failed` | 驱动/权限/GPU 异常 | 是 |
| `parse_error` | 远端输出不符合预期 | 否 |
| `output_limit` | 远端输出超过 8 MiB 被终止 | 否 |
| `internal_error` | gpufree 内部错误 | 是 |

同一台不可达主机可能因 ssh 的错误文本不同而报 `ssh_timeout` 或 `ssh_unreachable`
（`timed out` 先匹配前者）；两者都按可重试处理。

`gpufree doctor --probe` 会真连**所有 preset 的所有主机**跑 `nvidia-smi`，主机多时很慢；
单机排障优先用 `--host <alias>`，不要随手 `--probe`。

**“加一台新机器。”** 在 preset 的 `hosts` 里写 ssh 别名（不写 IP/端口/用户）；外网访问
再补 `[presets.<name>.remote_aliases]`，或把 preset 的 `transport` 设为 `remote`；
可选 `prefix`（如 `module load cuda/12.4 &&`，仅 preset 级）。改完
`gpufree presets validate`（0 通过 / 1 校验失败 / 2 解析或 IO 失败），再用 `--print-config`
确认解析。

## 边界

- 只读：远端不写文件、不 sudo、不杀进程。看到占用者只报告，不动手。
- 不做调度、排队、预留、历史曲线、告警（watch 在 v2 才有）。
- 不用于云 GPU 租赁比价，也不替代 `nvidia-smi` 的逐卡细节。
- 主机多时注意耗时：每台一次 ssh，默认单跳建连 6s、单机总超时 15s，直连并发 8、
  跳板并发 4；`settings.parallel` / `parallel_remote` / `connect_timeout` /
  `total_timeout` 可调。`ControlMaster` 默认复用 60s；探测与 `--print-config` 可用
  `--no-mux` 关闭复用排障（`doctor` 不接受该效果）。

字段、错误码与语义的完整清单见 [`references/json-contract.md`](references/json-contract.md)；
设计与判定依据见仓库 `docs/DESIGN.md` §3、§6、§7、§9。
