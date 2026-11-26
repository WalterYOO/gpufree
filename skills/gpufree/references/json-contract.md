# gpufree JSON contract（schema `gpufree/v1`）

`gpufree --json` / `--format json` 输出单个 JSON 文档到 stdout；日志与警告走 stderr。
键始终存在，可空值用 `null`（不省略）。字段只增不删；`schema` 主版本变化才算 breaking。
输出顺序：`hosts` 按 preset / 参数顺序。

## 顶层

| 字段 | 类型 | 说明 |
|---|---|---|
| `schema` | string | 固定 `"gpufree/v1"` |
| `generated_at` | string | 本次输出时间，RFC 3339 带时区；`--utc` 改为 UTC |
| `preset` | string \| null | 使用的 preset 名；显式主机时为 `null` |
| `transport` | `"direct"` \| `"remote"` \| `"auto"` | **请求**的 transport 模式 |
| `duration_ms` | number | 整批探测耗时 |
| `summary` | object | 见下 |
| `hosts` | array | 每台一个对象 |

## `summary`

| 字段 | 说明 |
|---|---|
| `hosts_total` | 请求的主机数 |
| `hosts_ok` | `status == "ok"` 的数量（含 `no_gpu` 主机） |
| `hosts_error` | `status == "error"` 的数量 |
| `hosts_shown` | 实际出现在 `hosts` 数组里的数量；`--free-only` 时变小 |
| `gpus_total` / `gpus_free` / `gpus_busy` / `gpus_unknown` | 只统计 `ok` 主机上的 GPU |

`--free-only` 只过滤 `hosts` 数组（保留 `ok` 且至少一张 `free` 的主机），**不改** GPU
计数，也不改退出码。全忙时 `hosts: []`、`hosts_shown: 0`、exit 0。注意 `error` 主机也会被
一起移除；判断“是真没卡还是有主机没探到”要看 `hosts_error`，不能只看 `hosts` 是否为空。

## `hosts[]`

| 字段 | 类型 | 说明 |
|---|---|---|
| `host` | string | 配置 / 参数里的逻辑主机名 |
| `resolved_alias` | string | 实际传给 `ssh` 的别名；后续手工 ssh 用它 |
| `transport_used` | `"direct"` \| `"remote"` | 该主机最终采用的方式（auto 会逐机解析） |
| `status` | `"ok"` \| `"error"` | 主机级结果 |
| `probed_at` | string | 该主机探测**开始**时间（发起 ssh 前取时间戳），RFC 3339 |
| `duration_ms` | number | 该主机耗时 |
| `hostname` | string \| null | 远端 `hostname` 输出 |
| `load1` | number \| null | `/proc/loadavg` 的 1 分钟负载 |
| `cpus` | number \| null | `nproc` |
| `fields_complete` | bool | 所有 GPU 的关键字段与二级字段是否齐全 |
| `processes_complete` | bool | 所有 GPU 的 compute-apps 查询成功，且每个 PID 都在 `ps` 里找到了属主 |
| `note` | string \| null | 目前只有 `"no_gpu"`：主机正常但未检测到 GPU |
| `gpus` | array | 错误主机为 `[]`，下游无需判空分支 |
| `error` | object \| null | `ok` 时为 `null` |

`status == "error"` 时 `gpus: []`、`hostname/load1/cpus` 为 `null`、
`fields_complete == processes_complete == false`。

`processes_complete=false` 不等于“进程情况不可信”：`ps` 属主查询失败时进程列表仍保留，
只是 `user` 为 `null`，`state=busy` 依然有效；compute-apps 整体失败且显存/利用率都低时
才是 `unknown`（`reason=no_process_data`）。

### `error`

| 字段 | 说明 |
|---|---|
| `kind` | 见下表 |
| `message` | 面向人的错误文本。**JSON 只做转义，不剥离控制字符**（表格 / TUI / doctor 才会清理）；拼进日志或终端前先自行过滤 `\x00-\x1f\x7f-\x9f`（实现里的 C0 / DEL / C1 范围） |
| `retryable` | 是否值得原样重试 |

| `kind` | 含义 | retryable |
|---|---|---|
| `ssh_timeout` | 建连 / 探测超时 | true |
| `ssh_unreachable` | 网络不可达 | true |
| `ssh_auth` | 认证失败（BatchMode 下无法交互输入密码） | false |
| `ssh_hostkey` | host key 校验失败 / 变更 | false |
| `nvidia_smi_missing` | 远端没有可用的 `nvidia-smi` | false |
| `nvidia_smi_failed` | `nvidia-smi` 非零退出（驱动、权限、GPU 异常） | true |
| `parse_error` | 远端输出不完整 / 不符合协议 | false |
| `output_limit` | 单机输出超过 8 MiB 被终止 | false |
| `internal_error` | gpufree 内部错误 | true |
| `unknown` | 未分类 | false |

## `hosts[].gpus[]`

| 字段 | 类型 | 说明 |
|---|---|---|
| `index` | number \| null | `nvidia-smi` 的 GPU 序号 |
| `uuid` | string \| null | GPU UUID |
| `name` | string \| null | 型号，如 `NVIDIA H20-3e` |
| `driver` | string \| null | 驱动版本；降级查询可能为 `null` |
| `mig` | bool | 是否启用 MIG |
| `compute_mode` | string \| null | 如 `Default`、`Prohibited` |
| `memory_total_mb` | number \| null | MiB |
| `memory_used_mb` | number \| null | MiB |
| `memory_free_mb` | number \| null | MiB；筛选任务显存需求时用它 |
| `util_pct` | number \| null | 0–100，两次采样取最大 |
| `temp_c` | number \| null | 摄氏度 |
| `state` | `"free"` \| `"busy"` \| `"unknown"` | 三态判定 |
| `reason` | string \| null | `unknown` 的原因；`free` / `busy` 通常为 `null` |
| `processes` | array \| null | `[]` = 确认无计算进程；`null` = 查询失败 |

`reason` 取值：

| reason | 含义 |
|---|---|
| `mig` | 启用 MIG，v1 不映射实例，无法判定 |
| `compute_prohibited` | compute mode 为 Prohibited |
| `missing_fields` | 关键字段（index/uuid/显存/利用率）缺失 |
| `no_process_data` | 进程查询失败且显存/利用率未达 busy 阈值，不能证明空闲 |

判定顺序：MIG → Prohibited → 关键字段缺失 → 进程查询失败且显存/利用率未达 busy 阈值
（以上皆 `unknown`）→ 有进程 / 显存超 `used_mem_max_mb` / 利用率超 `util_max_pct`
（`busy`）→ 其余 `free`。
阈值在 `[settings.free]`，默认 `used_mem_max_mb = 512`、`util_max_pct = 10`；两次采样
（默认 2 次、间隔 1s）在远端一次会话内完成，取最差。

`fields_complete == false` 不必然导致 `unknown`：二级字段（温度、compute mode、driver）
缺失只降低置信度；关键字段缺失才会把该卡判为 `unknown`。

## `gpus[].processes[]`

| 字段 | 类型 | 说明 |
|---|---|---|
| `gpu_uuid` | string | 所属 GPU UUID |
| `pid` | number \| null | 进程号 |
| `user` | string \| null | 属主（来自 `ps`） |
| `process_name` | string | 命令行（已处理引号与逗号） |
| `used_memory_mb` | number \| null | 该进程显存占用 |

只列 compute 进程；桌面合成器等图形进程不算。用户名与命令行属于敏感信息，不要外发。

## 示例（节选）

```json
{
  "schema": "gpufree/v1",
  "generated_at": "2026-11-26T16:06:57+08:00",
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
      "probed_at": "2026-11-26T16:06:50+08:00",
      "duration_ms": 7060,
      "hostname": "gpu06",
      "load1": 4.0,
      "cpus": 192,
      "fields_complete": true,
      "processes_complete": true,
      "note": null,
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
          "reason": null,
          "processes": []
        }
      ],
      "error": null
    }
  ]
}
```
