# codex 独立审核意见（第二轮，2026-11-25）

> 角色：独立审核者。输入：`docs/design-draft-v0.md`。以下为原始回复，未删改。

**覆盖**
四项齐全，未跑偏；但被 cache/watch/auto/partial 稀释。缺口：列表仅逗号串、无配置首用失败、默认 company 可能长期 error、MIG 检索需降级。

**过度设计**
1. partial 及过滤：需求只要空闲/非空闲。
2. cache/--cached：缓存“空闲”会误导。
3. --watch/ndjson：流式生命周期成本高。
4. TUI 复制/切 preset/多排序：非验收项。
5. auto 网段推断：VPN/多网卡下不可靠。

**真实缺失**
1. SSH 参数冲突：`-n` 置空 stdin、`--` 分隔不可靠，探测跑不通。
2. JSON 缺 null 契约、summary 分母、实际 transport；`--free-only` 未定义。
3. 列表/配置：缺文件、重复参数、位置参数；`--hosts` 与 `--preset` 冲突未定义；无配置缺 init。
4. TUI q/r 缺取消去重；171 host 缺 viewport。
5. 默认 company 可能长期 error；MIG 下用户/进程检索失效。

**九问**
1. Go+bubbletea 合适，Rust 无收益。
2. 方案对；修 `-n`/`--`；ControlPath 绝对路径，Persist 60–120s；`-O check` 只查不预热；direct/remote 实为别名选择，建议改名。
3. 字段够；加 compute_mode，砍 power/clocks/ecc；MIG 独立查询。
4. 1024MB/5%/单样本偏松；改 512MB、10%、2 次取最差且无 compute 进程；MIG 无映射/Prohibited 不判 free；砍 partial。
5. 补 transport_used、duration_ms、null；`processes:null` 表查询失败；错误 host 保留 `gpus:[]`；0=至少一台 ok，`--strict` 才非 0。
6. `/`+`f` 够；不做多选刷新，先做取消/去重和 viewport。
7. 只读 TOML 可接受；remote 缺映射报错（h20-6/nfs-204-5/hk-h20 补 gpuNNr）；auto 仅 opt-in。
8. 砍 cache/watch/ndjson/partial/切 preset/复制；补 hosts-file、取消、JSON 契约。
9. gpufree 可保留。

**结论：PASS-WITH-CHANGES**

**必须修改**
1. SSH 改 `ssh -T ... "$host" sh -s`，删 `-n`/`--`；ControlPath 0700 绝对路径；fake-ssh 测 stdin/超时/ProxyJump。
2. nvidia-smi 核心与 MIG/进程分开查，含 compute_mode；`[N/A]`→null；探测路径；PID 过滤非数字；超时杀进程组。
3. 空闲默认无 compute 进程且 mem≤512MB、util≤10%（2 次取最差）；MIG 无映射/Prohibited 标 unknown，UI 明示不可归属；删 partial。
4. JSON 补 nullable/transport_used/duration_ms；错误 host 保留 `gpus:[]`；日志走 stderr；退出码 0=有 ok、1=全失败、2=用法、3=有结果但无空闲卡，`--strict` 可选；统一 `--format table|json`，`--json` 别名；`--free-only` 只筛 host。
5. 加 `--host` 可重复、位置参数、`--hosts-file -`；`--hosts`/`--preset` 二选一；无配置提示 `gpufree init`；默认显式 transport，auto opt-in，`--transport` 覆盖，remote 缺映射报错；默认 company 改可连通示例或去掉 default_preset。
6. TUI q/r 取消 context，generation id 丢旧结果；viewport；删复制/切 preset/多排序；v1 删 cache/watch/ndjson。
