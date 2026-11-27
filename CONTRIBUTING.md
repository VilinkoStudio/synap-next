# 贡献约定

从旧 Synap 的 `CONTRIBUTING.md` 留下还能用的部分。平台 tag、分端发版、`core → ffi → 各端` 的提交顺序都绑在旧 monorepo 上，不搬过来。旧仓库的 ADR、桌面设计稿、构建说明同样留在 [旧 Synap 仓库](https://github.com/VilinkoStudio/Synap)。

## 提交

Conventional Commits：

```text
<type>(<scope>): <summary>
```

`type` 沿用原来的：`feat`、`fix`、`refactor`、`perf`、`docs`、`test`、`build`、`ci`、`chore`。

`scope` 写主要改动落在哪。现在仓库几乎只有文档，用 `docs` 或具体文件主题即可，例如 `docs(philosophy)`。以后有代码，再按模块起 scope，不要提前照抄旧项目的 `android/net`、`coreffi` 这类名字。

`summary` 写这次改了什么，短、具体。不要写「更新文档」「修了一些问题」。

一次提交做一件事。文档和无关代码不要捆在一起。

## 分支

`main` 保持别人拉下来就能读、后来有代码时也能构建。试探用 `feat/*`、`docs/*`、`spike/*`。

## 文档

产品概念写在 `docs/000` 起的编号文档里，按顺序读。还没拍板的写在对应文档里，不要写进 README 当已决定的事。

不把聊天记录、长谈导出原文放进仓库。
