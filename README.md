<div align="center">

# Synap

按类型记录，按情景回看。

[![status](https://img.shields.io/badge/status-prototype-orange)](#状态)
[![license](https://img.shields.io/github/license/VilinkoStudio/synap-next)](LICENSE)
[![last commit](https://img.shields.io/github/last-commit/VilinkoStudio/synap-next)](https://github.com/VilinkoStudio/synap-next/commits)
![stack](https://img.shields.io/badge/stack-Rust%20%2B%20Flutter-informational)

</div>

笔记会被拿去做完全不同的事：随手记、待办、AI 知识库、个人记录。这些事不该共用同一种组织结构。旧 Synap 用 DAG 去装全部，这条路不再走。

> [!NOTE]
> Synap 仍处于原型实验阶段。本仓库目前只公开设计文档，下文描述的是设计目标，不是已经提供的功能。

## 为什么做 Synap

一个人的记录通常散落在好几个 App 里：待办、记账、日记、读书笔记各用一个。每个 App 都好用，但数据彼此隔开。想回顾一次旅行，就得在几个 App 之间来回翻找。

Notion、Tana、Anytype 这类工具把数据放到了一起，但结构要用户自己搭：建库、配关系、写查询。

Synap 的出发点是：人记录时按类型来记，一笔账、一条待办、一篇日记；回看时却按情景来看，这次旅行、这个项目、这学期。专门的 App 只照顾了前一半，Synap 想把两半都做好，并且不需要用户自己搭建任何结构。

## 设计目标

### 专注的工具

待办、记账、日记各自是完整、好用的工具，有自己的页面和交互。Synap 不会为了"打通"而把不相关的数据混进同一个列表。

### 共用的入口

几样东西由所有工具共用：

- **一个输入框**：输入"午饭 35，和小王"，记账工具会认出这是一笔支出，不用先想该打开哪个 App。
- **一份待处理清单**：记账里待核实的账目、学习里待复习的内容、读书里待整理的摘录，会汇总到同一个地方，无需配置。
- **一个搜索框和完整的历史**：所有记录都能搜到，能看到它被哪些内容引用，也能回到任意一个历史版本。

### 情景

一次旅行、一个项目、一门课，往往同时包含多种记录。Synap 把它们归到同一个情景里：

```mermaid
flowchart LR
  T[待办<br/>订酒店 · 办签证] --> C
  L[记账<br/>机票 · 餐饮 · 交通] --> C
  D[日记<br/>每天的见闻] --> C
  N[笔记<br/>行程攻略] --> C
  C(("情景<br/>日本旅行"))
  C --> P1[按时间排开的经过]
  C --> P2[各工具给出的摘要<br/>总花费 · 未完成事项]
  C --> P3[你自己写的回顾]
```

- 进入某个情景后，这段时间里新记下的内容会自动归入其中。
- 情景页是一份可以阅读的记录：按时间顺序排列的经过，加上各个工具提供的摘要。它不是一块堆满数字的看板。
- 做过的情景可以拿来当下一次的起点，比如"像上次旅行那样准备"。

### 为小需求做小工具

常见的 App 只覆盖最普遍的需求。记吃药、记健身组数、管理收藏，这类小需求很少有趁手的工具。

Synap 计划支持让 AI 按你的描述生成这样的小工具。生成出来的工具和内置工具一样，能同步、搜索，也能出现在待处理清单和情景里。

### 与 AI 协作

Synap 不内置聊天窗口，而是通过标准接口连接你自己选择的 AI。AI 读到的是带结构的记录，而不是一堆零散的文本。一个情景就是一份现成的上下文；你也可以只授权 AI 读取某个情景，不开放整个库。

## 与现有工具的区别

| | 结构由谁搭建 | 数据是否在一处 | 使用前的配置 |
| --- | --- | --- | --- |
| 专门的 App | 开发者 | 否 | 几乎不需要 |
| Notion / Tana / Anytype | 用户 | 是 | 较多 |
| Synap | 工具作者或 AI | 是 | 选一套预设即可 |

## 数据由你掌握

- 数据保存在本地并加密。
- 同步方式由你选择：局域网、自建服务器或对象存储。
- 可以在自己的服务器上运行一个个人节点：用 Docker 部署，作为一直在线的同步对端，也能在任何浏览器里打开你的库。
- 工具的定义和数据存放在一起。某个工具不再维护，数据也依然可读。
- 随时能把整个库导出成一个文件一份记录的明文，改完还能导回来，历史不会丢。

无头节点现在可以从源码安装，或打成较小的 musl 镜像部署。安装需要 Rust 工具链和 C 编译器（LuaJIT 随源码编译）：

```bash
cargo install --locked --git https://github.com/VilinkoStudio/synap-next.git synap-node
synap-node init --data-dir ./data
synap-node serve --data-dir ./data
```

在仓库里也可以 `cargo install --locked --path crates/synap-node`，或 `just install-node`。默认听 `127.0.0.1:8790`。

```bash
docker compose up --build
```

或不用 compose：

```bash
docker build -t synap-node .
docker run --rm -p 8790:8790 -v "$PWD/data:/data" synap-node
```

镜像默认听 `0.0.0.0:8790`，数据写在 `/data`；`serve` 会在空目录里建库。若 `docker build` 提示缺少 buildx，前面加上 `DOCKER_BUILDKIT=0`，或先安装 docker-buildx。

## 状态

Synap 是 [旧 Synap](https://github.com/VilinkoStudio/Synap) 之后的一次重新设计。旧版以碎片记录为核心，在扩展到待办、知识库等用途时遇到结构上的限制，所以从更底层重新开始。

目前已经完成底层与产品形态的设计。下一步先不做界面，而是做一个可以自己用起来的无头数据库：通过命令行和 MCP，让 AI 往里整理真实的待办、成绩和记忆，看看 AI 写起数据插件来是什么感觉，再边用边改设计。之后的客户端要验证两件事：情景能不能让人留下来，小工具能不能让人愿意来用。

- [x] 底层与产品形态设计
- [ ] 第一阶段：无头数据库、命令行与 MCP，自己先用起来
- [ ] 明文导出与导入
- [ ] 个人节点：HTTP 接口与同步
- [ ] 网页版客户端（由个人节点提供）
- [ ] 桌面与手机客户端
- [ ] 首批工具：随手记、待办，以及记账或读书之一
- [ ] 统一输入、待处理、情景
- [ ] 通过 AI 生成小工具

## 设计文档

架构与实现细节见 [`docs/`](docs/)，按编号顺序阅读。原型阶段以文档为规格。

| 文档 | 内容 |
| --- | --- |
| [000](docs/000-from-synap.md) | 与旧 Synap 的关系 |
| [001](docs/001-philosophy.md) | 设计理念、术语，以及与 CouchDB 等前人方案的对照 |
| [002](docs/002-architecture.md) | 整体架构 |
| [003](docs/003-product-shape.md) | 产品形态 |
| [004](docs/004-synap-core.md) – [007](docs/007-synap-app.md) | 各模块规格 |
| [008](docs/008-synap-node.md) | 个人节点 |

## 参与

现阶段最需要的是对设计的反馈：你会用它记什么，你想到的情景是什么，文档中哪里讲不通。欢迎在 Issue 中讨论。提交约定见 [CONTRIBUTING.md](CONTRIBUTING.md)。

## License

[Apache-2.0](LICENSE)
