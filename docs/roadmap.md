# aicoder 路线图

状态：S0–S6 已完成当前 Linux 范围实现；S7 正在推进评测、最终验收与 Pi 对照。本文替代旧 TODO 的排期。
基线和验证依据见 [S0 开发基线](development-baseline.md)。
S1 的统一模型契约已贯穿执行器、工具、会话、CLI/TUI 和 bench，wire types 已移至
providers；部分结果、任务预算和 provider 配置均已接通。S1 验收见 [验收记录](s1-acceptance.md)。见 [迁移说明](migrations.md) 和 [模型 API](model-api.md)。

## 1. 产品目标

构建轻量、可嵌入、可扩展的 Rust 编码 Agent：日常能力对齐 Pi，同一个执行内核
服务 CLI、TUI、Web 和 bench。优先保证契约与完整闭环，再增加 provider 数量。

四个约束：

1. **轻量**：默认安装不要求 Node、Python、Docker、数据库或常驻服务；
   标准本地工作流仍依赖 Git、Bash、ripgrep。评测 harness 和可选扩展可有额外依赖。
2. **完整**：覆盖项目上下文、编码工具、长会话、交互控制、扩展、机器接口，
   不能只完成一次模型调用和工具循环。
3. **provider 可扩展**：先交付一个完善的 OpenAI-compatible adapter；
   公共 API 不绑定该协议。新增 provider 不应修改执行循环。
4. **宿主无关**：核心不读取终端输入，不加载进程环境配置，不依赖 UI 或 Web 框架；
   应用显式注入 provider、工具、存储、策略和事件消费者。

轻量不只看代码行数。记录冷启动时间、空闲及运行峰值内存、release 二进制体积、
编译时间、依赖树、每轮事件和消息复制量。S0 建立基线后给出可复现的预算，
后续依赖引入及性能回退需要解释，不提前承诺未经测量的数字。

## 2. S0 起点与主要约束

S0 起点有四个 crate：core、cli、tui、eval。已有模型与工具循环、流式内容、工具审批、
取消、线性 JSONL 会话、确定性裁剪、可插拔 completion verifier 和评测链路。
保留可复用实现，按阶段迁移，不重写整个项目。

当时识别的主要约束如下。S1 已解除协议/HTTP 耦合，S3 已解除默认工具及存储装配的耦合：

- `ChatCompletionProvider` 仍暴露 OpenAI 风格 wire types，抽象尚未跨协议。
- core 同时包含 HTTP adapter、内置工具和运行逻辑；嵌入方难以只选必要能力。
- S0 会话是线性消息列表；S2 已增加分支和模型/压缩/扩展记录，语义摘要仍按 S4 推进。
- 已有事件回调适合本地观察，但尚未定义版本化的跨进程协议、背压及断线语义。
- 高层结果已区分完成、取消和失败，但底层限制退出及中断进度仍需统一契约。
- CLI、TUI 的配置、项目上下文加载和会话控制需要共享应用层。

## 3. 目标分层与 API 边界

按真实依赖拆包，避免每个 trait 都独立成 crate。计划先增加两个 crate：

| crate | 职责 | 依赖约束 |
| --- | --- | --- |
| `aicoder-core` | 协议无关模型类型、执行循环、会话模型、事件、控制与策略接口 | 不依赖其他本仓库 crate、HTTP/UI 框架 |
| `aicoder-providers`（新增） | OpenAI-compatible wire 转换、HTTP、重试、认证来源接口 | 依赖 core；具体协议不进入 core |
| `aicoder-runtime`（S3 已新增） | 配置加载、项目资源、默认工具与 JSONL 存储装配、会话服务、扩展发现 | 依赖 core/providers；不依赖终端或 Web UI |
| `aicoder-cli` | 单次输出、REPL、JSON/RPC 入口 | 使用 runtime/core |
| `aicoder-tui` | 终端输入、渲染、交互适配 | 使用 runtime/core |
| `aicoder-eval` | case、工作区隔离、评测器、报告、SWE-bench | 使用 core；按需使用 providers/runtime |

S3 已将内置工具、JSONL 文件存储和 Git 完成检查迁入 runtime，
core 保留接口、数据类型与内存实现；默认 builder 不隐式注册工具。MCP、Web adapter 和扩展运行时在功能出现后按需拆包，
不预建空 crate。运行不涉及配置与文件系统的 mock Agent 不应需要 runtime。

核心契约按能力设计，以下名称是设计草案，S1 确定后补 API 文档：

- `ModelProvider`：协议无关请求、统一流事件、取消与结构化错误；模型能力可查询。
- `ModelSpec`：provider/model 标识、上下文窗口、输入/输出能力、可选价格与 tokenizer。
  不支持的参数明确报错或返回诊断，不默默忽略。
- `Message` / `ContentBlock`：文本、工具调用/结果、图片与可扩展元数据；
  reasoning 区分展示内容与 provider 私有状态，不伪装成普通文本跨 provider 传递。
- `ExecutableTool` / `ToolRegistry`：schema、执行、能力标签、取消、进度、结构化输出；
  工具定义不依赖某一家模型的 function 格式。
- `TurnExecutor`：一次用户 turn 内的模型/工具循环；不负责配置发现和 UI。
- `Agent` / session service：消息提交、分支上下文、持久化与运行控制；
  同一 session 写操作串行，不同 session 可由宿主并行调度。
- `RunHandle`：取消、steer、follow-up、审批响应；所有控制带 run/session 标识。
- `RunOutcome`：完成、失败、取消、达到预算，均提供已产生的消息、usage、
  轮次和停止原因；provider 未返回 usage 时标记未知，不填成精确的零。
- `SessionStore`、`ContextPolicy`、`ApprovalPolicy`、`CompletionVerifier`：
  明确扩展边界，保留内存实现与确定性测试替身。

事件是观察接口，策略/hook 是行为修改接口，避免通过异步事件监听间接改变执行顺序。
先定义契约再增加快捷入口，CLI/TUI/bench 不能各自实现一套循环。

## 4. Pi 功能对齐范围

对标基线：[Pi 官方功能页](https://pi.dev/)，核对日期：2026-10-28。
S2 起固定为 Pi v1.4.0 / `4a080252f06caf63a525d7873c5d94d3c23887e0`；见 [对照基线](pi-baseline.md)。
后续阶段沿用该版本，避免目标无限追随上游变化。
目标是能力对等，不要求兼容其 TypeScript 插件 ABI 或复制所有快捷键。

| 能力 | aicoder 交付位置 | 完成标准 |
| --- | --- | --- |
| SDK、流式事件、可取消执行 | S1 | CLI/TUI/bench 复用同一契约 |
| 自定义模型/provider、会话内切换 | S1、S4 | adapter 注入与能力校验；先支持一个协议 |
| 模型思考级别控制 | S5 | 声明支持级别，界面可设置，不支持时明确诊断 |
| 文件读写编辑、搜索、shell | S3 | 有界输出、错误可恢复、变更可检查 |
| 项目指令、系统提示、skills、模板 | S3 | 发现规则、来源和优先级可见，可禁用 |
| 自动与手动压缩 | S4 | 摘要可持久化、可追溯、失败可恢复 |
| 树形会话、分支、标签、恢复 | S2、S4 | 任意有效节点可继续，分支互不污染 |
| 运行中 steer 与排队 follow-up | S2、S5 | 明确投递点、顺序及取消语义 |
| TUI、print/JSON、RPC、SDK | S1、S5 | 四种使用方式共用 runtime/core |
| 扩展工具、hook、命令、UI 能力 | S6 | Rust 扩展和进程外扩展均有示例 |
| 扩展资源包、发现、安装、重载 | S6 | 安装可固定版本，可移除，失败不破坏会话 |
| 主题、导出、分享 | S5、S6 | 本地 HTML/JSON 导出；发布由可选 adapter 完成 |
| MCP 与 Code Mode | S6 | MCP 工具可用；组合调用可选且受相同策略约束 |
| 多模态 | S1 类型、S5 交互 | 支持图片输入的 adapter 可完整收发 |

Pi 核心刻意留给扩展的子 Agent、plan mode 等，aicoder 同样优先通过扩展示例提供。
多租户平台、RAG 数据库、内置浏览器、LSP、自动规划系统不作为首轮功能对齐门槛。
Web 目标是可接入且有可运行示例，不在此路线图中建设完整账户/部署平台。

## 5. 阶段与验收

按 S0 → S1 → S2 → S3 → S4 → S5 → S6 顺序推进。S7 是最终验收，
评测与性能记录从 S0 开始持续执行，不等功能全部完成才补。
每个阶段独立可用、可检查，不同时推进多个大重构。

### S0：固定基线与工程约束

- [x] 记录现有 API、持久化格式、依赖树、二进制和运行性能基线。
- [x] 选择经过验证的 Rust 版本与 MSRV，统一 workspace 元数据、依赖和 lint 策略。
- [x] CI 覆盖 fmt、Clippy、测试、文档构建；锁文件固定依赖。
- [x] 定义 Linux 为首个完整验证平台；明确 macOS/Windows 的 shell 与工具差异。
- [x] 整理离线 smoke cases：编辑、搜索、工具失败、取消、会话恢复。

验收：现有测试通过，基线可复现，文档明确哪些功能已实现、哪些仍在计划。

### S1：协议无关的 core 与 provider adapter

已交付统一模型主路径：core 无 OpenAI wire types，所有入口使用同一 `ModelProvider`
循环。新会话存储内容块，旧 v1 会话兼容读写，usage 未知值贯穿 UI 和评测。
结构化运行结果与轮次/工具/token/时长/输出预算已接通所有入口，验证说明见
[运行结果与预算](run-outcomes.md)。S1 已通过 [验收检查](s1-acceptance.md)，下一阶段是 S2。

- [x] 确定内部 Message、ContentBlock、ModelSpec、ProviderError、Usage 类型。
- [x] 将 OpenAI wire types、HTTP、重试和认证装配从执行循环移出。
- [x] 新增 providers crate，先完善 OpenAI-compatible adapter；配置 endpoint、
  代理、超时及 API key，明确服务兼容差异。
- [x] 支持模型注册、provider 注入与 capability 查询；预留 Responses/Anthropic 等 adapter。
- [x] 统一流与非流结果，覆盖碎片工具参数、异常 finish、空回复及连接中断。
- [x] 统一有类型的运行错误与完整部分结果；设置轮次、工具、token、时长、输出预算。
  token 上限缺少精确计量时标明估计和请求中可能超出的边界。
- [x] 提供纯内存 mock provider/tool/store 的 SDK 示例和公开 API 契约测试。

验收：无网络 Agent 可只依赖 core；mock 与真实 adapter 使用相同循环；新增 provider
只需实现 adapter。现有入口保持可运行；必要 API 变更附迁移说明，不长期保留两套模型。

### S2：会话、控制与事件契约

会话树、v3 迁移、独占运行与中断恢复已落地；见 [会话契约](sessions.md)。
RunHandle 的有界输入、steer/follow-up 和异步审批已接入核心与 TUI；见 [运行控制](run-control.md)。
S2 的版本化事件和有界订阅已落地，见 [事件契约](events.md)。[会话验收](s2-sessions-acceptance.md)
保留了前一增量的 202 个测试与轻量指标；[运行控制验收](s2-controls-acceptance.md)
保留了该增量的 219 个测试和性能采样；[S2 最终验收](s2-events-acceptance.md)
覆盖当前 246 个测试、协议样例和三次轻量采样。

- [x] 将会话扩展为追加式树：entry ID、parent ID、当前叶节点、模型/压缩/扩展记录。
- [x] 定义版本、原子写入、并发冲突、崩溃恢复及旧 JSONL 的迁移和备份路径。
- [x] 将中断前已完成的交互持久化；不把未配对的工具调用直接发回模型。
- [x] 引入 RunHandle，支持 cancel、steer、follow-up 和异步审批响应。
  在安全执行点投递 steer；正在执行的变更工具不被强行回滚。
- [x] 定义事件 schema、序号、session/run/round 标识、生命周期和终止事件。
- [x] 定义背压与断连：消费者慢时限制缓冲，重要记录不能静默丢弃；
  UI 订阅断开与运行取消分离，核心不承诺未实现的断点续传。

验收：内存和 JSONL store 跑同组契约测试；取消、审批、steer、崩溃恢复与分支
有确定性测试，终止后无悬挂审批和继续写入。

### S3：共享 runtime、项目上下文与可靠工具

共享配置/默认装配已接入 CLI、TUI、SWE-bench，见 [配置契约](runtime-configuration.md)。
项目资源加载与快照重载已接通，见 [资源契约](project-resources.md)。
S3 已整体通过当前 Linux 验收，见 [授权与阶段验收](s3-approvals-acceptance.md)；后续按 S4 推进。
拆包契约见 [runtime 后端](runtime-backends.md)，验证见 [验收记录](s3-backends-acceptance.md)。
[配置增量验收](s3-runtime-acceptance.md) 保留 268 个测试的历史记录；
[资源增量验收](s3-resources-acceptance.md) 保留 293 个测试及三次轻量采样的历史记录；
拆包增量保留 294 个测试的历史记录；[文件工具增量](s3-file-tools-acceptance.md) 保留 308 个测试的历史记录；
[编辑增量](s3-edits-acceptance.md) 保留 323 个测试的历史记录；[shell 增量](s3-shell-acceptance.md) 保留 339 个测试的历史记录；当前授权增量覆盖 355 个测试。

- [x] 新增 runtime，统一显式参数 > 环境变量 > 项目配置 > 用户配置 > 默认值；
  凭证使用独立来源，不混淆 provider 与默认 endpoint。
- [x] 加载 AGENTS.md、SYSTEM.md、skills、提示词模板；确定全局、父目录、
  工作区与嵌套目录规则，去重、大小限制、来源展示、关闭开关和热重载边界。
- [x] 内置文件/搜索/shell 工具和 JSONL store 由 runtime 提供，core 只保留接口及内存实现。
- [x] 补目录枚举与 glob 搜索；read/grep 以有界方式读取；明确截断与结果分页。
  见 [文件工具契约](file-tools.md) 与 [验收记录](s3-file-tools-acceptance.md)。
- [x] 编辑支持精确匹配及并发修改检测；写入采用适当的原子策略；审批可展示 diff。
  见 [修改契约](file-mutations.md) 与 [验收/耗时取舍](s3-edits-acceptance.md)。
- [x] shell 的输出、超时、取消、进程组清理和凭证处理形成统一契约。
  见 [shell 契约](shell-tools.md) 与 [验收/内存采样](s3-shell-acceptance.md)。
- [x] 审批策略支持拒绝、单次、会话级授权；前端只负责展示和提交决定。
  见 [授权契约](approvals.md) 与 [S3 全项验收/累计开销](s3-approvals-acceptance.md)。

验收：CLI/TUI/eval 可共享同一 runtime 配置；项目指令可追溯；大输出、路径逃逸、
并发编辑及取消工具有回归测试。保留已有路径与审批保护，明确 shell 不等于沙箱。

### S4：长会话与上下文管理

- [x] 分离持久化历史和 model-facing context，支持按分支构建上下文。
  见 [视图契约](model-context.md) 与 [验收记录](s4-projection-acceptance.md)。
- [x] 实现自动及手动语义摘要，保留目标、约束、修改与待办；摘要记录可追溯。
  见 [摘要契约](semantic-summary.md) 与 [验收记录](s4-summary-acceptance.md)。
- [x] 摘要失败可重试/回退，避免反复压缩；摘要 usage 计入总量。
- [x] 对超长工具输出裁剪或引用完整产物；不向模型发送不完整的工具调用单元。
  见 [产物契约](output-artifacts.md) 与 [验收记录](s4-artifacts-acceptance.md)。
- [x] 分支、标签、搜索、重命名、恢复和模型切换；切换时验证上下文能力。
  见 [会话契约](session-navigation.md) 与 [验收记录](s4-navigation-acceptance.md)。
- [x] 可选精确 tokenizer，缺少时显式使用估计；支持自定义 ContextPolicy。
  精确性区分文本编码与完整请求，见 [计数/策略契约](context-policy.md) 与 [验收记录](s4-policy-acceptance.md)。
- [x] 为长会话存储/视图增加分页或加载预算，避免为构建有限上下文加载全量归档。
  生产执行使用真实尾部窗口，原始记录通过归档页访问；见 [窗口契约](archive-windows.md) 与 [S4 验收](s4-archive-acceptance.md)。

验收：长会话不会只因历史增长无限占用内存或超出配置窗口；固定任务验证摘要后
仍保留关键约束；模型能力不匹配有清晰诊断。

### S5：四种入口与 Web 接入示例

- [x] CLI：stdin/参数输入、单次 print、JSON 事件、REPL、统一配置和 Ctrl-C。
  见 [CLI 契约](cli-execution.md) 与 [验收记录](s5-cli-acceptance.md)。
- [x] TUI：多行/历史、文件引用、命令面板、模型切换与思考级别、分支、审批 diff、steer/follow-up，
  展示 usage、上下文占用及可选成本；主题配置保持独立。
  见 [交互契约](tui-interaction.md) 与 [验收记录](s5-tui-acceptance.md)；思考级别沿用 [统一 API](reasoning-controls.md)。
- [x] 图片内容通过统一消息 API 进入；TUI/CLI 明确文件限制及 provider 能力。
  见 [图片输入契约](image-input.md) 与 [图片 / S5 全项验收](s5-images-acceptance.md)。
- [x] 版本化 stdin/stdout RPC：request ID、控制命令、响应、事件和错误；
  日志只去 stderr/文件，不能污染协议输出。
  见 [RPC v1 契约](rpc.md) 与 [验收记录](s5-rpc-acceptance.md)。
- [x] 最小 Web 示例：HTTP 提交、SSE 事件、取消和审批；宿主负责认证、工作区授权
  与运行生命周期，浏览器不获得任意文件/命令入口。
  见 [Web 契约](web.md) 与 [验收记录](s5-web-acceptance.md)，四入口有同任务规范化对照。
- [x] JSON/HTML 本地导出；分享功能作为显式调用的发布 adapter。
  导出全部规范分支、保留中断状态；见 [导出契约](session-exports.md) 与 [验收记录](s5-exports-acceptance.md)。

验收：同一 scripted case 已经 SDK、CLI JSON、RPC、Web 示例执行，并产生一致的
规范化结果；TUI 只适配输入展示，不复制执行逻辑。当前 Linux 范围验证通过，
见 [S5 全项验收](s5-images-acceptance.md)；下一阶段是 S6，完整 Pi 对标和最终 bench 仍未完成。

### S6：扩展机制与 Pi 剩余能力

- [x] 稳定 Rust 的工具、provider、策略、hook 和命令注册 API。
  原生宿主显式安装冻结快照；见 [Rust 扩展契约](native-extensions.md) 与 [验收记录](s6-native-acceptance.md)。
- [x] 区分观察与拦截 hook；定义顺序、超时、失败处理、可改字段和权限边界。
  有界上下文追加、审批前拒绝、事件观察和命令宿主投递均有离线契约测试。
- [x] 用版本化进程外协议实现动态工具扩展，避免 Rust 动态库 ABI；支持取消与崩溃诊断。
  可选 stdio adapter 复用原生快照和 core 审批；见 [协议契约](process-extensions.md) 与 [验收记录](s6-process-acceptance.md)。
- [x] 接入 MCP，先 stdio，再按实际需求增加远端传输；复用审批、预算和 trace。
  支持 2026-07-28 与旧握手协议，包含真实官方服务器互通；见 [MCP 契约](mcp.md) 与 [验收记录](s6-mcp-acceptance.md)。
- [x] 提供资源包 manifest、发现/安装/卸载、版本固定、运行时重载与会话状态恢复。
  本地包、CLI 内容锁与 SDK 显式公共状态恢复已验收，声明精确依赖及执行权限；默认不引入脚本运行时。
  见 [资源包契约](packages.md) 与 [验收记录](s6-packages-acceptance.md)；远端获取与 stock UI 的代码激活继续按需推进。
- [x] 宿主暴露 UI capability 接口：通知、选择、输入、状态、命令；
  原生宿主的 TUI/Web 适配与无 UI 回退已验收；通道有界、取消后失效，UI 答案不能赋予工具权限。
  见 [UI 契约](extension-ui.md) 与 [验收记录](s6-ui-acceptance.md)；core 不依赖终端或 Web 类型。
- [x] 提供 plan、子 Agent、保护路径等扩展示例，验证扩展无需 fork 执行循环。
  同一 core 支持规划拦截、已准备目标保护及独立有界只读 worker；见 [示例契约](coding-extension-examples.md) 与 [验收记录](s6-coding-examples-acceptance.md)。
- [x] 可选 Code Mode 通过受限组合执行器调用已注册工具；
  独立可选 QuickJS 组件支持真实 JS、并行只读、过滤、发现及分支存储；叶调用复用审批、取消、父预算和规范记录。
  见 [Code Mode 契约](code-mode.md) 与 [S6 全项验收](s6-code-mode-acceptance.md)，Pi 特有 helper 和最终功能矩阵留给 S7 核验。

验收：扩展作者可新增工具/provider/命令并影响上下文；扩展失败不会静默损坏会话；
默认发行不要求运行可选脚本引擎。类型契约、生命周期和示例与实现同步。

### S7：持续 bench、功能验收与发布

- [ ] 每阶段运行离线契约及小型编码任务集；真实模型 smoke 手动触发并固定参数。
  离线编码任务集与显式 --live 入口已就绪；[首轮 Spark/Pi smoke](pi-comparison.md) 已采集四题，完整评测仍待隔离执行与扩大样本。本机模拟服务不作为真实模型证据。
- [x] bench 直接使用 core/runtime，不解析终端文本；固化 commit、配置、模型、
  指令/资源版本、扩展集合、case、工作区、seed（若支持）和产物。
  aicoder-bench 提供实际执行快照、输入指纹和独立归档；见 [契约](coding-benchmarks.md) 与 [验收](s7-bench-acceptance.md)。
- [x] 记录成功率、停止原因、工具失败、usage、延迟、内存和成本（价格未知则不估算）。
  网络/provider 的非确定性作为实验限制记录。
- [x] 保存事件与工作区 diff；可重放 scripted provider/tool 验证回归。
  事件回放仅验证记录与展示，不宣称能还原外部命令或精确复现模型输出。
- [x] SWE-bench 保持生成与官方 grading 隔离；失败样本计入总体结果。
  schema 4 续跑绑定全量输入与产物，官方 5.0.2 / 本地 Docker 夹具已互通；见 [契约](swebench-contract.md) 与 [验收](s7-swe-acceptance.md)。
  夹具比例不作为实际 SWE-bench 编码成绩；真实 SWE-bench 与最终 Pi 对照仍待完成。
- [ ] 在相同任务、模型和资源预算下对比 Pi；分别报告功能覆盖与编码成功率。
  首轮 Spark / DeepSeek-V4.1-Flash-1017 同题 smoke：aicoder 完成并通过 3/4，Pi 4/4；修复测试均 4/4。
  保留未完成结果；首轮非沙箱 /tmp 副作用与 Pi pin 不完整限制不追溯修改。隔离执行/完整包快照已通过离线契约，下一步扩大隔离后的成对样本。
- [ ] 对照第 4 节逐项验收，完成公开 API 文档、迁移说明、运行示例及发布说明。
  [进行中的功能矩阵](pi-parity-audit.md)保留生命周期 hook 及最终评测等实际差异；stock 进程/MCP/包内可执行扩展、包主题、远端原生依赖闭包、[工具图片投影](s7-tool-images-acceptance.md)、[内置图片读取/BMP](s7-native-images-acceptance.md)、[MIME 兼容](s7-mime-acceptance.md)、[Code Mode 显式图片](s7-code-images-acceptance.md)、[only 模式](s7-code-only-acceptance.md)、[原生分类器](s7-classifiers-acceptance.md)、[图片生成模型](s7-image-generation-acceptance.md)、[工具调用/结果拦截](s7-tool-interception-acceptance.md)、[请求视图](s7-request-views-acceptance.md)、[兼容 HTTP hooks](s7-transport-hooks-acceptance.md)、[原生 transport 绑定](s7-aux-transport-acceptance.md)、[轮次/收尾边界动作](s7-boundary-hooks-acceptance.md)、[启动阶段准备](s7-startup-hooks-acceptance.md)、[原始输入拦截](s7-input-hooks-acceptance.md)、[空闲自定义消息](s7-custom-messages-acceptance.md)、[运行中自定义投递](s7-custom-delivery-acceptance.md)、[助手回复定稿](s7-assistant-finalization-acceptance.md)、[各角色新消息定稿](s7-message-finalization-acceptance.md)、[受控会话采用/导航](s7-session-lifecycle-acceptance.md)及[独立会话 fork/clone](s7-session-fork-acceptance.md)已补齐，不能以 S6 清单替代完整 Pi 对齐。

验收：功能矩阵每项有实现/示例/测试依据；轻量指标有前后对照；能说明与 Pi 的
剩余差异。不能用接口预留代替已实现功能，也不以一次 smoke 代表 benchmark 成绩。

S7 当前增量：[持久化上下文编辑](context-edits.md)和[投影感知的摘要准备](summary-projection.md)
已接入原生会话 API。摘要按稳定 entry ID 映射原始保留边界，覆盖编辑/省略、拆分工具结果、
自定义消息和原生上下文；v1 检查点保留原计数规则，新写入 v2 按全部真实消息计数。
[运行边界草稿](boundary-drafts.md)现已支持自定义消息、上下文编辑和原生压缩，预览与提交沿用
固定 ID 与同一事务；回调模型工作计入父预算。接下来接压缩拦截、树摘要与隔离真实评测，
最终性能与编码质量验收仍开放，不能用离线接口契约代替。

## 6. 执行规则

- 每阶段先补契约与必要设计记录，再实现一条可运行路径，最后扩展错误和并发边界。
- 重用已有 115 个测试作为起点；只有涉及行为、契约或已发现风险时增加测试，
  不为机械迁移重复测试实现细节。
- 跨 crate 改动通过 fmt、Clippy、workspace tests；公共文档和示例同步更新。
- 新依赖说明用途与默认构建成本；宿主可选能力不进入 core 的强制依赖。
- API/会话格式变更写明迁移，旧入口只在迁移期保留；不维护两套独立执行循环。
- S0–S6 已完成当前 Linux 范围实现；下一阶段 **S7** 最终验收与对照；按实际证据报告 Pi 剩余差异。

最近的稳定性/性能补查见 [终止事件退出窗口与成对 smoke 对照](s7-terminal-exit-followup.md)：
TUI 单次 Ctrl-C 收尾已补回归；对照未复现此前 22% 的幅度，仍保留原始采样与分析限制。
