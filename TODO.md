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
- [x] 加入可配置的上下文 token 预算，按完整工具调用单元裁剪旧历史
- [ ] 增加模型语义摘要压缩策略，并支持 provider 精确 tokenizer
- [ ] 区分 `stop`、`length`、`content_filter`、空回复和异常 tool call
- [ ] 超过最大轮次时返回消息、Usage 等部分执行结果
- [ ] 限制任务总工具调用数、总执行时间和累计输出大小
- [ ] 以流式、分页或提前终止方式读取大文件和搜索结果，避免先全部载入内存

## P0：评估闭环

- [x] 独立 `aicoder-eval` crate，支持隔离工作区、轨迹和结构化报告
- [x] SWE-bench JSON/JSONL 数据适配、固定 commit 检出和官方 prediction 导出
- [x] SWE-bench 批量筛选、有界并发、仓库缓存、断点续跑和逐 case 产物
- [x] 接入官方 Python/Docker harness 命令并支持汇总报告导入
- [x] 固化 baseline 参数、token、耗时和失败分类
- [ ] 基于首轮 baseline 实现在线 completion verifier

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
- [x] 支持会话保存、恢复、`--continue` 和 `--session`
- [ ] 增加 `--model`、`--base-url`、`--timeout`、`--max-rounds`、`--temperature` 和 `--stream`
- [ ] 使用现有的 `dotenvy` 依赖真正加载 `.env`
- [ ] 支持从 stdin 读取 prompt
- [ ] 增加 `--json` 机器可读输出
- [x] core 支持协作式取消模型请求、流式响应、审批和工具执行
- [ ] CLI 将 Ctrl-C 接入协作式取消
- [ ] 展示模型轮次、工具调用、耗时和结果摘要

## P1：TUI

- [x] 独立 `aicoder-tui` crate，采用单向 AppEvent/reducer 架构
- [x] 支持 Session 列表、新建、打开和确认删除
- [x] 支持流式 content/reasoning、工具参数/结果、状态、Usage 和耗时展示
- [x] 支持工具审批弹窗、单 active turn 和协作式取消
- [x] 异常退出恢复终端，日志写入文件而不污染界面
- [ ] 增加多行输入、输入历史和更完整的滚动选择状态
- [ ] 增加 Session 重命名、搜索和工作区切换
- [ ] 增加 git status/diff 专用视图及文件编辑审批 diff
- [ ] 增加设置页并统一 CLI/TUI 配置加载

## P2：项目配置与提示词

- [ ] 支持 `.aicoder.toml` 项目级配置
- [ ] 自动读取 `AGENTS.md` 等项目说明文件
- [ ] 完善系统提示词，要求先检查项目、避免覆盖文件、修改后验证并报告结果
- [ ] 分离 Provider 配置，避免将非 OpenAI Key 与默认 OpenAI 地址错误组合

## P3：工程化

- [x] 拆分为 library crate 和 binary crate
- [ ] 增加多轮工具调用、重试、审批拒绝和上下文裁剪的端到端测试
- [ ] 增加 SSE 跨 TCP chunk、同 chunk 多事件以及流式 tool call 测试
- [ ] 增加凭证不进入日志、路径无法逃逸和超时清理子进程等安全回归测试
- [x] 配置 CI，执行 `cargo fmt --check`、`cargo test` 和 Clippy
- [x] 补充架构和评估 README
- [ ] 增加依赖安全检查和 `.env.example`
- [ ] 统一中英文错误信息和日志格式

## 后续增强

- [ ] 实现 `LoopAgent`（命名预留）：在 `TurnExecutor` 之上组合 Planner、Evaluator 和持久化 `LoopState`，负责跨 turn 的自主规划、执行、评估与继续/终止决策
- [ ] MCP 工具接入
- [ ] LSP 和语法树搜索
- [ ] 多 workspace 支持
- [ ] 图片及多模态消息
- [ ] Responses API
- [ ] 子 Agent 和任务分解
- [ ] Token 成本估算和调用统计
- [ ] 工具插件动态加载

## 建议的下一阶段

1. 在 baseline 数据上实现并验证 completion verifier。
2. 加入上下文 token 预算、旧工具输出裁剪和历史摘要。
3. 增加 `list_files`、`git_status` 和 `git_diff`。
4. 增加任务级工具调用、时间、token 和输出预算。
5. 完善代理模式与不同 OpenAI 兼容服务的错误格式解析。

当前 `Agent::run` 中的请求和消息 clone 属于性能优化项，不影响正确性，优先级低于以上工作。
