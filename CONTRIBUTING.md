# 贡献约定

从旧 Synap 的 `CONTRIBUTING.md` 留下还能用的部分。平台 tag、分端发版、`core → ffi → 各端` 的提交顺序都绑在旧 monorepo 上，不搬过来。旧仓库的 ADR、桌面设计稿、构建说明留在 [旧 Synap 仓库](https://github.com/VilinkoStudio/Synap)。

这个仓库的提交历史就是发布说明：`dev` 上每个 PR 压缩成一条功能级提交，GitHub Release 的正文由这些提交生成。所以提交信息按「将来会出现在发布说明里」的标准写。

## 提交

格式沿用 Conventional Commits：

```text
<type>(<scope>): <summary>

[正文]
```

`type` 用 `feat`、`fix`、`refactor`、`perf`、`docs`、`test`、`build`、`ci`、`chore`。

`scope` 取改动所在的模块，只从这张表里选：

| scope | 范围 |
|---|---|
| `core` | `crates/synap-core`：块、编解码、ID、密钥 |
| `store` | `crates/synap-store`：落盘、帧管道、索引 |
| `runtime` | `crates/synap-runtime`：实体、投影、合并 |
| `sync` | `crates/synap-sync`：对账、Merkle、传输 |
| `node` | `crates/synap-node`：个人节点二进制 |
| `relay` | `crates/synap-relay`：中继 |
| `http` | `crates/synap-http`：HTTP/MCP 网关 |
| `bridge` | `crates/synap-bridge`：Flutter Rust Bridge 边界 |
| `app` | `app/`：Flutter 应用 |
| `repo` | 工作区配置、`justfile`、脚本 |
| `ci` | `.github/` |
| `deps` | 依赖升级 |

一次提交只写一个 scope。跨模块的原子改动取主要归属，其余在正文里说明：

```text
feat(core): 帧格式改为按 SyncId 分片

涉及 store 的帧表与 sync 的对账边界，必须同时升级。
```

拆提交会破坏可构建性时不要拆，把范围写清楚即可。不要用业务词当 scope（`editor`、`notes`），发布说明按 scope 分组，词表一漂移分组就失效。

文档提交的 scope 写文档主题（`philosophy`、`readme`、`004`），不受上表限制。

`summary` 写具体做了什么，不要写「更新文档」「修了一些问题」。破坏性变更加 `!`（`feat(core)!: …`）或在正文写 `BREAKING CHANGE:`，发布说明会单独标出来。

一个 PR 做一件事；确实带了多个功能时，在合并时按「合并 PR」一节拆开。文档和无关代码不要捆在一起。

## 分支

| 分支 | 用途 | 规则 |
|---|---|---|
| `feat/*`、`fix/*`、`spike/*` | 个人开发 | 怎么提交都行，这些提交不会进 `dev` 的历史 |
| `dev` | 集成 | 只通过合并 PR 前进；每个 PR 压成一条提交 |
| `main` | 发布 | 只由 `dev` fast-forward 过来，tag 打在这里 |
| `release/x.y` | 旧版本维护 | 需要同时支持两条已发布版本线时才建 |

`dev` 只允许 fast-forward 前进，所以没有 merge commit，历史是一条直线。不要在 `dev` 上直接提交绕过 PR；`main` 不接收直接提交。

## 开发

从 `dev` 开分支：

```bash
git switch dev && git pull
git switch -c feat/store-syncid-index
```

按上面的格式提交。分支上的提交不用整理——合并时会压成一条，原样保留的只有 PR 标题和正文。

推送并开 PR 到 `dev`：

```bash
git push -u origin HEAD
gh pr create --base dev
```

PR 标题必须符合提交格式，因为 squash 后它就是 `dev` 上的提交标题，也是发布说明的一行。标题不合规、scope 不在词表里，`pr-title` 检查会直接失败；改了标题会自动重跑。

CI 另外跑：

- `cargo fmt --check`、`cargo clippy -- -D warnings`、`cargo test`（`Cargo.toml` 存在时）；
- `flutter analyze`、`flutter test`（`app/pubspec.yaml` 存在时）。

纯文档改动不跑构建。

## 合并 PR

维护者操作。一个 PR 对应一件事时，直接在 GitHub 上 squash，标题和正文就是最终提交：

```bash
gh pr merge 12 --squash -t "feat(store): 块表加 SyncId 索引" -b "块表新增 SyncId 索引，读取路径改走索引。"
```

一个 PR 里有多个能分开的功能时，在本地分组，一条功能一条提交（GitHub 不能只 squash 其中一部分）：

```bash
git fetch origin pull/12/head:pr-12
git switch -c merge/12 dev

git cherry-pick -n <shaA> <shaB>          # 同一功能的若干提交，只暂存改动
git commit -m "feat(store): 块表加 SyncId 索引" -m "Refs #12"

# 其余功能重复上面两步
git switch dev && git merge --ff-only merge/12 && git push origin dev
gh pr close 12 --comment "已按功能拆成 <sha…> 合入 dev"
```

拆不开的多提交（改一处必须连带另一处）就整个 PR squash 成一条，标题写主功能。

`cherry-pick` 会产生新的 sha，作者原分支的提交不会出现在 `dev` 上。合并后让作者 `git fetch` 并从 `dev` 重开分支，旧分支不要再继续用。

## 发布

发布由 annotated tag 定义：tag 打在 `main` 上，推送后 `release` workflow 负责构建和发布。

先定版本。`app/pubspec.yaml` 的 `version` 是应用版本的唯一来源（Flutter 构建读它），在 `dev` 上改好并合入。`0.x` 阶段加功能提 minor、修问题提 patch，不兼容变更也走 minor 并在提交里标 `!`；预发布用 `v0.2.0-rc.1`。

然后发布：

```bash
git switch main && git pull
git merge --ff-only dev
git tag -a v0.2.0 -m "v0.2.0"
git push origin main v0.2.0
```

workflow 先校验三件事，任一不满足直接失败：

- tag 是 annotated（分支 HEAD 会动，发布身份只能是 tag）；
- tag 指向的提交在 `main` 上；
- tag 版本与 `app/pubspec.yaml` 一致。

校验通过后构建 `synap-node` 和 `synap-relay`，用 `cliff.toml` 取上一个 tag 到当前 tag 的提交生成发布说明，创建 GitHub Release；带 `-` 的版本（如 `v0.2.0-rc.1`）自动标为 prerelease。

打完 tag 后本地预览发布说明：

```bash
git cliff --latest --strip header
```

已发布的 tag 和 Release 正文不改。发布说明来自提交，手改会在下次生成时丢掉；需要更正就在下一版用一条提交写清楚。

## 文档

产品概念写在 `docs/000` 起的编号文档里，按顺序读。还没拍板的写在对应文档里，不要写进 README 当已决定的事。

不把聊天记录、长谈导出原文放进仓库。
