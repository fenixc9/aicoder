# S0 开发基线

> S3 relocation: current coding smoke is `cargo run --locked -p aicoder-runtime --example offline_smoke`;
> concrete backends now live in runtime. Commands/measurements below record this historical increment.
> See [backend migration](runtime-backends.md).

本文件描述 S1 迁移前的行为，供后续比较；未来实现不能把这里的限制当成目标契约。

## 工具链和平台

- 固定 Rust 1.92.0，workspace 的 `rust-version = "1.92"`；MSRV 与固定工具链相同。
  这是已验证的支持下限，不声称是源码理论上所需的最老编译器。
- Linux x86_64 是首个完整验证平台。编译当前 reqwest 默认 TLS 需要系统 OpenSSL
  开发包及 C 工具链（Debian/Ubuntu：`build-essential pkg-config libssl-dev`）。
- 运行工具及测试需要 Git、Bash 和 ripgrep。默认运行不依赖 Python、Node、Docker。
- macOS 尚未进入完整验证矩阵；Bash/Git/rg 和进程组行为需要本机验证。
  Windows 尚未进入完整验证矩阵；shell 仍调用 Bash，Unix 进程组清理不是原生 Windows
  job object，不能声明具备相同的取消保证。
- `scripts/baseline.py` 是开发辅助脚本，额外需要 Python 3、Linux `/proc` 和 GNU time；
  SWE-bench 官方 grading 另需 Python 与 Docker。

## 公开 API 起点

以 `crates/aicoder-core/src/lib.rs` 和 `tests/public_api.rs` 为源码依据。

| 边界 | 当前接口 | 后续需处理的约束 |
| --- | --- | --- |
| 模型 | `ChatCompletionProvider`、`ChatClient` | 暴露 OpenAI request/response；HTTP 在 core |
| 执行 | `TurnExecutor` / builder | 接收完整请求，工具/模型/验证循环，默认 8 轮 |
| 高层入口 | `Agent`、`AgentConfig` | 组装 system/user 消息，支持无状态与持久化运行 |
| 结果 | `AgentTurnOutcome` | 完成/失败/取消；中断结果缺少完整消息，usage 默认零会混淆未知 |
| 观察 | `AgentEventHandler`、raw envelope | run ID、递增序号与 round；无界队列、每 run 一个线程 |
| 控制 | `TurnExecutionContext` | 协作式取消；暂无 steer/follow-up |
| 工具 | `ExecutableTool`、`ToolRegistry`、`ApprovalHandler` | JSON 参数和结构化结果；默认工具依赖本地文件系统 |
| 会话 | `SessionRepository`、内存/JSONL 实现 | 同步调用、线性消息，尚无分支 |
| 上下文 | `ContextCompactor`、`PruningContextCompactor` | 可选确定性裁剪，默认关闭，无语义摘要 |
| 完成检查 | `CompletionVerifier` | 默认接受，评测可注入 workspace 变更检查 |

依赖快照通过下述采集脚本输出 `dependencies.json` 与 `dependencies.txt`。
保留 registry 包精确版本及依赖边，不把传递依赖数量当作性能本身。

## 会话 v1 格式

源文件：`crates/aicoder-core/src/session/jsonl.rs`。一会话一个 JSONL 文件，首行为
`session` 记录，随后为 `message` 记录。时间戳单位为 Unix 毫秒，cwd 是规范化路径。

```json
{"type":"session","version":1,"id":"<uuid>","created_at":0,"cwd":"/workspace"}
{"type":"message","id":"<uuid>","created_at":0,"message":{"role":"user","content":"hello"}}
```

系统消息属于运行时上下文，不写入会话。末行不完整记录在下次 append 时修复；
中间损坏报错；写入使用文件锁和长度检测拒绝过期句柄。当前只有 turn 成功后才追加
模型生成消息，中途失败/取消不保存已完成的工具交互。这是 S2 需要修复的行为。
S2 升级前保留 v1 fixture 与恢复测试，明确备份及迁移路径。

## 离线 smoke 与测量

```sh
cargo run --locked -p aicoder-core --example offline_smoke
python3 scripts/baseline.py --clean-build --output artifacts/s0-baseline
```

smoke 使用临时工作区和脚本 provider，依次验证实际编辑、ripgrep 搜索、未知工具错误
回传、请求期间取消、关闭并重开 JSONL repository 后继续会话。失败返回非零退出码，
CI 必须执行示例，不能只编译它。支持 `--iterations N` 重复同组任务。

采集脚本在独立临时 target 目录编译 release，保留用户现有构建目录；记录编译时间、
三个入口的二进制体积与进程 `--help` 耗时、离线运行峰值 RSS、空闲 runtime RSS、
每 case 事件数、消息数和序列化字节。输出含 revision、源码摘要和 dirty 状态，
因为未提交的工作区不能只靠 commit 标识。

测量限制：不清除操作系统文件缓存；第一次进程启动不是磁盘冷启动。`--help` 计时
含进程创建及 GNU time 包装成本，不代表 TUI 完成渲染。空闲内存来自 current-thread
离线示例，不代表交互式应用；运行峰值不含真实模型网络响应。干净 target 编译不含
显式依赖下载，但缓存未命中时仍可能下载；需要离线编译可先 `cargo fetch --locked`。

## 消息和事件复制起点

以下是源码检查，不能解释为 allocator 实测结果：

- `Agent` 构造请求时将 session 消息转换为 owned `ChatMessage`。
- `TurnExecutor` 分别维护完整 transcript 和模型上下文；工具结果会进入两份消息集合。
- raw event 中大载荷使用 `Arc` 减少回调间复制，但队列无容量上限。
- 当前 compaction 不裁剪持久化历史；长会话峰值内存尚无硬上限。

因此 S1/S2 的重点是理清所有权与事件契约；在 S4 前不宣称长会话内存已经有界。
本阶段的事件数/序列化字节是后续比较指标，不是精确分配次数或复制字节数。

## 检查入口

```sh
cargo fmt --all --check
cargo clippy --locked --workspace --all-targets -- -D warnings
cargo test --locked --workspace
cargo run --locked -p aicoder-core --example offline_smoke
RUSTDOCFLAGS='-D warnings' cargo doc --locked --workspace --no-deps
```

任何阶段的基线变化都需说明原因，不能用更新快照掩盖 smoke 行为回退。

## 首轮记录与后续预算

首轮记录见 [报告](baselines/s0-report.json)、[依赖树](baselines/s0-dependencies.txt)
和 [smoke 摘要](baselines/s0-smoke.json)。采集时工作区包含尚未提交的迁移，
以报告中的 source SHA-256 标识构建输入。该快照在 S1 分离 providers 之前采集，
后续代码变化需要重新采集比较，不能把该报告当作当前任意版本的测量结果。

| 指标 | 首轮实测 |
| --- | --- |
| 干净 target release 编译 | 44.6 秒 |
| CLI / TUI / SWE-bench 二进制 | 10.66 / 11.05 / 11.06 MiB |
| 三个入口 warm `--help` 中位数 | 3.84 / 3.66 / 3.77 毫秒 |
| 25 轮、125 个 smoke case 总进程耗时 | 306.8 毫秒 |
| 离线 runtime 峰值 / 空闲 RSS | 4.75 / 3.77 MiB |
| Cargo resolve 包版本数（含非 Linux 平台依赖） | 261 |

S1/S2 的初始回退审查预算：单个二进制体积增加超过 10%、同机 runtime RSS 增加超过
20%、干净编译或 smoke 耗时增加超过 20%，需要解释新增成本并考虑是否能放入可选组件。
`--help` 时间太短，采用增加 20% 且至少 2 毫秒才触发审查；时间/RSS 以同机同工具链
连续三次采集的中位数确认。首轮记录不是统计置信区间，不把噪声直接判成回退。
这些是审查预算而非跨硬件 CI 硬阈值；后续功能确需超出时记录取舍，不能静默重设。
