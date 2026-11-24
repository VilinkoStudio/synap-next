因为 `synap-store` 和 `synap-runtime` 都是纯 Rust 无头库，后续扩展能力全部作为独立 Crate 挂在外部，不塞进核心，也不走 `synap-bridge`：

### 1. `synap-net`（网络同步层）

* **职责：** 包装 `synap-store` 的 Sans-IO 同步状态机，引入 `tokio` 和具体传输通道。
* **传输解耦：** 同一套同步状态机可以按需接入多种后端——局域网直连（mDNS + WebSocket/QUIC）、自建轻中继服务器（Relay）、或纯静态对象存储（S3 / WebDAV，将块集合打包同步）。
* **运行方式：** 在桌面/服务端可作为后台常驻任务运行；在移动端由宿主按系统保活策略触发增量同步。

### 2. `synap-http`（统一外部接口与 MCP 网关）

* **职责：** 直接依赖 `synap-runtime`，在本地（或受控内网）暴露标准的 HTTP / WebSocket / SSE 服务，让外部世界与官方 Flutter 客户端处于**完全平等的地位**。
* **两类核心协议：**
* **通用结构化 REST / Stream API：** 供 CLI 工具、本地脚本、浏览器插件以及未来的本地 Markdown 文件夹双向桥接器使用。
* **原生 MCP（Model Context Protocol）端点：** 面向外部 AI Agent。不把大模型代码写进本项目，而是通过 MCP 把内核的完整上下文暴露出去——包括 Schema/Trait 自省、实时 Projection 查询、以及受沙箱和 CoW 不可变块保护的安全 Action 执行。外部 AI 既能精准读懂个人数据，也能安全写回结构化内容，甚至直接注册新的 Data Plugin。