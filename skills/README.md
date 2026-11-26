# skills

仓库自带的 agent skills，纯 Markdown，无构建步骤。

| skill | 用途 |
|---|---|
| [`gpufree/`](gpufree/SKILL.md) | 用 `gpufree` 通过 ssh 查一批机器的 GPU 空闲情况：`--json` 字段、free/busy/unknown 判定、退出码、preset 与 direct/remote transport、doctor 排障、选机器的 jq 配方。 |

## 安装到 agent

Claude Code 或其它读取 Agent Skills 目录的工具，把 skill 目录链接进去即可：

```bash
# 从仓库根目录执行；已存在同名目录时先自行确认
ln -s "$PWD/skills/gpufree" ~/.claude/skills/gpufree
# 或你的 skills 真实目录（例如 ~/.agents/skills）
ln -s "$PWD/skills/gpufree" ~/.agents/skills/gpufree
```

确认：

```bash
ls -l ~/.claude/skills/gpufree/SKILL.md
```

skill 内的命令假定 `gpufree` 已在 `PATH`（`make install` 装到 `~/.local/bin`）；未安装时
用仓库里的 `./bin/gpufree` 代替。
