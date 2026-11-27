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

- `dev`：开发主干，日常提交都落这里，CI 在这上面跑校验。
- `main`：发布分支，只接受 `git merge --ff-only dev` 和打 tag，不直接写代码。它的历史就是发布现场。
- `release/x.y`：只有需要同时维护旧版本线时才建，从对应 tag 切出；不是常规流程。顺手切一个「冻结分支」不解决问题，只会多一处要同步的分叉。
- 试探用 `feat/*`、`docs/*`、`spike/*`，合回 `dev`。

`main` 从不产生自己的提交，所以 tag 范围（上一个 tag..这个 tag）就是这一版发布内容的精确定义。

## 版本与发布

发布身份是 annotated tag，不是分支 HEAD：

1. 在 `dev` 上把 `app/pubspec.yaml` 的 `version` 改到目标版本（Flutter 构建读它）。
2. `main` 上 `git merge --ff-only dev`。
3. `git tag -a v0.2.0 -m "…" && git push origin main v0.2.0`。
4. `release` workflow 校验 tag 是 annotated、提交在 `main` 上、版本与 pubspec 一致，然后构建、用 `cliff.toml` 从提交生成 notes、发 GitHub Release。
5. 预发布用 `v0.2.0-rc.1`，自动标记为 prerelease。

不要手改 Release 正文：notes 来自提交，改它等于下次生成时丢掉。要让 note 变，就改对应的提交。

## 文档

产品概念写在 `docs/000` 起的编号文档里，按顺序读。还没拍板的写在对应文档里，不要写进 README 当已决定的事。

不把聊天记录、长谈导出原文放进仓库。
