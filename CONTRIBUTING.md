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

- `dev`：工作线。提交按改动落点写，粒度小、可以随性；完整历史只在这里，追溯、`bisect`、找引入问题的提交都查它。
- `main`：发布线。只有功能级的策展提交：发布时从 `dev` 挑改动，按功能分组压成一条条 `feat(scope): …`。tag 打在这里，release notes 也从这里生成。
- `release/x.y`：策展用的准备分支，从 `main` 切出、发布后删。需要同时维护两条已发布版本线时，它才长期保留。
- 试探用 `feat/*`、`docs/*`、`spike/*`，合回 `dev`。

`main` 只靠 fast-forward 前进（策展提交先在 `release/*` 上做好，再 ff 进 `main`），所以 `main` 上没有 merge commit，tag 范围（上一个 tag..这个 tag）恰好就是这一版的功能清单。

## 版本与发布

发布身份是 annotated tag，不是分支 HEAD。notes 由 `main` 的策展提交生成，`dev` 的碎提交不进 Release。

1. 在 `dev` 上把 `app/pubspec.yaml` 的 `version` 改到目标版本（Flutter 构建读它），记下基准：`BASE=$(git rev-parse dev)`。
2. `git switch -c release/0.2 main`，按功能把 `dev` 的改动分组搬过来，每组压成一条：

   ```bash
   git cherry-pick -n <shaA> <shaB>     # 同一功能的若干提交，只暂存改动
   git commit -m "feat(store): 块表加 SyncId 索引" -m "Dev-Base: $BASE"
   ```

   一个功能只有一条提交时，直接 `git cherry-pick <sha>`，再把 subject 改写成功能级。

3. 搬完确认没漏：`git diff --stat HEAD "$BASE"` 应该为空；不为空就是还差改动，补完再继续。
4. `git switch main && git merge --ff-only release/0.2`，然后 `git tag -a v0.2.0 -m "…"`，`git push origin main v0.2.0`。
5. `release` workflow 校验 tag 是 annotated、提交在 `main` 上、版本与 pubspec 一致，然后构建、用 `cliff.toml` 从提交生成 notes、发 GitHub Release。
6. 预发布用 `v0.2.0-rc.1`，自动标记为 prerelease。

不需要按功能拆的时候，`git merge --squash dev` 得到一条提交，改好 subject 照样发布——代价是一版只有一行 note。

`Dev-Base: <sha>` 是出问题时的入口：从它 `git log` 能回到每条改动在 `dev` 上的原始提交，`bisect` 在 `dev` 上做。

不要手改 Release 正文：notes 来自提交，改它等于下次生成时丢掉。要让 note 变，就改 `main` 上的策展提交。

## 文档

产品概念写在 `docs/000` 起的编号文档里，按顺序读。还没拍板的写在对应文档里，不要写进 README 当已决定的事。

不把聊天记录、长谈导出原文放进仓库。
