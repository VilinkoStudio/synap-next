# 贡献约定

从旧 Synap 的 `CONTRIBUTING.md` 留下还能用的部分。平台 tag、分端发版、`core → ffi → 各端` 的提交顺序都绑在旧 monorepo 上，不搬过来。旧仓库的 ADR、桌面设计稿、构建说明同样留在 [旧 Synap 仓库](https://github.com/VilinkoStudio/Synap)。

## 提交

Conventional Commits：

```text
<type>(<scope>): <summary>
```

`type` 沿用原来的：`feat`、`fix`、`refactor`、`perf`、`docs`、`test`、`build`、`ci`、`chore`。

`scope` 写主要改动落在哪个模块，取固定词表：

```text
core  store  runtime  node  relay  http  sync  bridge  app  repo  ci  deps
```

一次提交只写一个 scope。跨模块的原子改动取主要归属的模块，其余在正文里列出，例如 `feat(core): 帧格式改为 …（涉及 store、sync）`——拆开提交会破坏可构建性，所以拆的是说明，不是提交。不要发明业务词（`editor`、`notes`）当 scope：release notes 按它聚类，词表一漂移，聚类就失效。

文档提交的 scope 写文档主题（`philosophy`、`readme`、`004`），不受上面词表限制。

`summary` 写这次改了什么，短、具体。不要写「更新文档」「修了一些问题」。

一次提交做一件事。文档和无关代码不要捆在一起。

## 分支

- `feat/*`、`fix/*`、`spike/*`：个人分支。怎么提交都行，这些提交不进 `dev` 的历史。
- `dev`：集成分支。每个 PR 以一条功能级提交进入，PR 标题就是这条提交的 subject。
- `main`：发布线，只接受 `git merge --ff-only dev` 和 tag；release notes 从这里生成。
- `release/x.y`：需要同时维护两条已发布版本线时才建，从对应 tag 切出。

## 合并 PR

一个 PR 一件事，直接在 GitHub 上 squash，标题和正文写成最终提交：

```bash
gh pr merge 12 --squash -t "feat(store): 块表加 SyncId 索引" -b "…"
```

一个 PR 里有多个能分开的功能，就在本地分组搬，一条功能一条提交：

```bash
git fetch origin pull/12/head:pr-12
git switch -c merge/12 dev
git cherry-pick -n <shaA> <shaB>          # 同一功能的若干提交，只暂存改动
git commit -m "feat(store): 块表加 SyncId 索引" -m "Refs #12"
# …按功能重复
git switch dev && git merge --ff-only merge/12 && git push origin dev
gh pr close 12 --comment "已按功能拆成 <sha…> 合入 dev"
```

拆不开的多提交（改一处必须连带另一处）就整个 PR squash 成一条，标题写主功能。

`cherry-pick` 产生新的 sha，作者原来的提交不会出现在 `dev` 上；合完让作者 `git fetch` 后从 `dev` 重开分支，不要在旧分支上继续堆。

`dev` 只允许 fast-forward 前进，所以 `dev` 上没有 merge commit；`main` 从 `dev` ff 过来，tag 范围（上一个 tag..这个 tag）就是这一版的功能清单。

## 版本与发布

发布身份是 annotated tag，不是分支 HEAD。notes 由 `dev`（发布时即 `main`）上的功能级提交生成。

1. 在 `dev` 上把 `app/pubspec.yaml` 的 `version` 改到目标版本（Flutter 构建读它）。
2. `git switch main && git merge --ff-only dev`。
3. `git tag -a v0.2.0 -m "…" && git push origin main v0.2.0`。
4. `release` workflow 校验 tag 是 annotated、提交在 `main` 上、版本与 pubspec 一致，然后构建、用 `cliff.toml` 从提交生成 notes、发 GitHub Release。
5. 预发布用 `v0.2.0-rc.1`，自动标记为 prerelease。

不要手改 Release 正文：notes 来自提交。要让 note 变，就改提交；已经发布了才发现的措辞问题，在下一版的提交里改。

## 文档

产品概念写在 `docs/000` 起的编号文档里，按顺序读。还没拍板的写在对应文档里，不要写进 README 当已决定的事。

不把聊天记录、长谈导出原文放进仓库。
