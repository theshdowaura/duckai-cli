# duckai-cli

duckai-cli 是一个基于 Rust 的命令行工具，用于自动完成 Duck.ai 会话准备（VQD 哈希、前端版本等）并向 DuckDuckGo Chat 发送请求，可执行一次性对话或以 OpenAI 兼容协议暴露本地服务。增加了手动选择图片过418问题。

## 主要特性
- 自动化 VQD 会话协商：通过嵌入式 Boa 引擎执行 Duck.ai 下发的 JavaScript。
- 聊天请求流式转发：支持 SSE 推送、挑战重试与事件透传。
- OpenAI 兼容服务：基于 Axum 提供 `/v1/models` 与 `/v1/chat/completions` 代理。
- 灵活配置：可自定义 User-Agent、模型 ID，并可通过特性开关模拟 HTTP。

## CLI 使用说明
作为 duckai-cli，我支持下列常用参数与用法：
- `duckai-cli --help`：我会展示完整命令指南与参数解释。
- `duckai-cli --ua "Mozilla/5.0 (...)" --text "hi"`：我用指定的 User-Agent 并立即向 Duck.ai 发送一次性对话。
- `duckai-cli --prompt-file ./prompt.txt`：我读取给定文件内容作为用户输入。
- `cat prompt.txt | duckai-cli --stdin-prompt`：我从标准输入接收内容，适合脚本化流程。
- `duckai-cli --only-vqd`：我只打印协商得到的 VQD、哈希与前端版本，不发送聊天请求。
- `duckai-cli --model gpt-4o-mini`：我改用指定模型（默认是 `gpt-5-mini`）。
- 常见组合示例：
  ```bash
  duckai-cli --text "Explain VQD" --model gpt-5-mini --ua "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/91.0.4472.124 Safari/537.36" --listen 0.0.0.0:8844  --server-api-key your-secret
  ```

## 服务器模式
以 OpenAI 兼容接口启动本地代理并启用鉴权：
```bash
export DUCKAI_API_KEY=your-secret
RUST_LOG=info cargo run -- --serve --listen 127.0.0.1:8080
```
客户端需在请求头携带 `Authorization: Bearer your-secret`，即可访问 `/v1/models` 与 `/v1/chat/completions`，并获得 Duck.ai 返回的 SSE 流。

## 开发流程
- 代码格式化：`cargo fmt`。
- 静态检查：`cargo clippy --all-targets --all-features`。
- 单元测试：`cargo test`，若要隔离网络交互可使用 `cargo test --features http-mock`。
- 更多贡献规范参考 `AGENTS.md`。

## 项目结构
- `src/main.rs`：程序入口，负责选择 CLI 或服务器模式。
- `src/cli.rs`：命令行参数解析与 prompt 读取逻辑。
- `src/session.rs`：基于 reqwest 的会话构建与公共请求头。
- `src/vqd.rs`：状态查询、JS 评估、哈希与 FE 版本解析。
- `src/chat.rs`：聊天请求发送、SSE 事件解析与转发。
- `src/server.rs`：Axum 路由与 OpenAI 兼容接口实现。
- `src/js/mod.rs` 与 `js/runtime.js`：嵌入式 Boa 环境与运行时脚本。
- `duckai_challenge/`：本地调试或挑战脚本的暂存目录（默认忽略）。

## 配置与安全
- 运行服务器模式时，通过环境变量设置 `DUCKAI_API_KEY`，勿将密钥写入代码仓库。
- 默认基地址为 `https://duckduckgo.com`，仅在可控测试环境中修改。
- 开发时建议绑定 `127.0.0.1`，分享日志前请删除 VQD 哈希或 Bearer Token。 
