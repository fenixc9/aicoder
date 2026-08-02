# aicoder TODO

当前项目已经具备最小可用的编码 Agent：多轮模型调用、Tool 注册与分发、只读工具并发、变更工具串行、人工审批、workspace 路径限制，以及基础文件、搜索和命令工具。

## P0：可靠性与安全性

- [x] 修复 HTTP 重试逻辑，确保网络错误后真正重新发送请求
- [x] 区分网络错误、HTTP 429 和 5xx，并支持 `Retry-After`、指数退避和随机抖动
- [x] 禁止日志输出完整 API Key、Authorization 和其他凭证
- [x] 对请求、响应、Prompt、工具参数和文件内容进行脱敏及长度限制
- [x] 重写流式 SSE 解析，正确处理跨字节块事件和同一字节块内的多个事件
- [x] 把流式响应接入 Agent，并正确拼接流式 `tool_calls`
- [ ] 支持 `auto`、`no-proxy` 和显式代理配置，移除硬编码的 `.no_proxy()` 行为
- [ ] 兼容标准 OpenAI、DeepSeek 及其他兼容服务的错误响应格式

## P1：编码能力

- [x] 添加 `edit_file` 或 `apply_patch` 工具，避免修改文件时整体覆盖
- [ ] 编辑文件时支持原内容 hash/version 校验，防止覆盖并发修改
- [ ] 添加结构化的 `list_files` 工具，支持深度、glob 和忽略规则
- [ ] 添加只读的 `git_status`、`git_diff` 工具
- [ ] 加入上下文 token 预算、旧工具输出裁剪和历史摘要
- [ ] 区分 `stop`、`length`、`content_filter`、空回复和异常 tool call
- [ ] 超过最大轮次时返回消息、Usage 等部分执行结果
- [ ] 限制任务总工具调用数、总执行时间和累计输出大小
- [ ] 以流式、分页或提前终止方式读取大文件和搜索结果，避免先全部载入内存

## P1：工具安全

- [ ] 审批 `write_file` 时展示 diff，审批 `bash` 时展示工作目录和命令
- [ ] 支持仅本次允许、会话内允许该工具、全部拒绝等审批策略
- [ ] Bash 子进程改用环境变量白名单，而不是只删除少数已知密钥
- [ ] 命令超时时终止整个进程组
- [ ] 进一步防止路径检查和实际写入之间的竞态条件
- [ ] 统一 JSON Schema 和 Rust 参数类型，避免两边手写后不一致

## P2：CLI 体验

- [x] 增加串行 Agent 事件回调，覆盖 reasoning、content、tool call、工具执行、重试和 Usage
- [ ] 支持交互式 REPL，可以在一次启动中连续提问
- [ ] 支持会话保存、恢复和 `--resume`
- [ ] 增加 `--model`、`--base-url`、`--timeout`、`--max-rounds`、`--temperature` 和 `--stream`
- [ ] 使用现有的 `dotenvy` 依赖真正加载 `.env`
- [ ] 支持从 stdin 读取 prompt
- [ ] 增加 `--json` 机器可读输出
- [ ] 支持 Ctrl-C 优雅取消模型请求和工具执行
- [ ] 展示模型轮次、工具调用、耗时和结果摘要

## P2：项目配置与提示词

- [ ] 支持 `.aicoder.toml` 项目级配置
- [ ] 自动读取 `AGENTS.md` 等项目说明文件
- [ ] 完善系统提示词，要求先检查项目、避免覆盖文件、修改后验证并报告结果
- [ ] 分离 Provider 配置，避免将非 OpenAI Key 与默认 OpenAI 地址错误组合

## P3：工程化

- [ ] 拆分为 library crate 和 binary crate
- [ ] 增加多轮工具调用、重试、审批拒绝和上下文裁剪的端到端测试
- [ ] 增加 SSE 跨 TCP chunk、同 chunk 多事件以及流式 tool call 测试
- [ ] 增加凭证不进入日志、路径无法逃逸和超时清理子进程等安全回归测试
- [ ] 配置 CI，执行 `cargo fmt --check`、`cargo test`、Clippy 和依赖安全检查
- [ ] 补充 README 和 `.env.example`
- [ ] 统一中英文错误信息和日志格式

## 后续增强

- [ ] MCP 工具接入
- [ ] LSP 和语法树搜索
- [ ] 多 workspace 支持
- [ ] 图片及多模态消息
- [ ] Responses API
- [ ] 子 Agent 和任务分解
- [ ] Token 成本估算和调用统计
- [ ] 工具插件动态加载

## 建议的下一阶段

1. 加入上下文 token 预算。
2. 正确实现流式 SSE 和 tool call 拼接。
3. 增加代理模式配置。
4. 完善不同 OpenAI 兼容服务的错误格式解析。
5. 增加 `list_files`、`git_status` 和 `git_diff`。

当前 `Agent::run` 中的请求和消息 clone 属于性能优化项，不影响正确性，优先级低于以上工作。
