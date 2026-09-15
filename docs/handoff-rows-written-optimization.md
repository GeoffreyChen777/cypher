# Handoff：Durable Objects `rows_written` 优化

更新时间：2026-09-15

## 当前分支与 Git 状态

- 分支：`feat/rows-written-optimization`
- 最近提交：`9d67701 Add preview streaming and durable chat outbox`
- 最近合并：`f0a3181`，已合入 `origin/main` 的 `19f15c1`
- 当前有未提交的 P2 改动，主要文件：
  - `crates/sync/src/chat_client.rs`
  - `crates/sync/src/chat_client/tests.rs`
  - `crates/sync/src/store.rs`
  - `crates/engine/src/chat2_host.rs`
  - `crates/engine/src/doc_host.rs`
  - `crates/engine/src/sessions.rs`
  - `crates/engine/tests/rows_written_crash_baseline.rs`
  - `crates/doc/examples/rows_written_fixture.rs`
  - `edge/test/workerd/rows-written.baseline.ts`
  - `scripts/rows-written-baseline.sh`
  - `PLAN.md`
  - `docs/rows-written-baseline.md`

不要 reset 或 checkout 掉未提交改动。

## 已完成内容

### P0

- 建立真实 workerd/SQLite 的固定 rows-written 基线。
- 固定负载：240 次文本追加，120ms 本地提交节奏，1/3 viewer。
- 基线结果：243 durable batch，PUSH 阶段 1,218 rows，总计 1,593 rows。
- SIGKILL characterization 已扩展为 durable outbox 恢复测试。

### P1

- Rust / TypeScript / Swift `ephemeral-stream-v1` codec。
- 共享 wire/state reducer 测试向量。
- 开发端 preview relay：连接级授权、epoch、revision、限流、慢消费者保护。
- Desktop Engine、iOS `ChatRoomClient` / `SessionStore` preview 展示层。
- preview 不写 Loro、不推进 durable cursor、不执行命令。
- durable coverage marker：`meta.previewCoverage`。
- 本地 macOS Engine x2 + iOS Simulator + real local workerd 原生互通通过。

### Dev Edge

- 已部署开发 Worker：
  `https://cypher-edge-development.geoffreychen777.workers.dev`
- 最近已验证的 cloud preview WS 测试通过：publisher/viewer handshake、epoch、Unicode
  snapshot、delta、receipt、durable PUSH/ACK。
- dev Guard 配额已调整为：10,000 events/day、1,200 events/minute、20,000 observed rows/day。
- `DEV_PREVIEW_ENABLED=true` 已部署。
- `DEV_PREVIEW_PUBLISH_TOKEN` 已通过 Cloudflare secret 注入；值不在仓库、日志或本文档中。
- 私有本地配置：`~/Documents/cypher-development.env`，权限应保持 `600`，包含独立
  `CYPHER_DEV_PREVIEW_PUBLISH_TOKEN`。
- CI/deploy token 位于 `~/Documents/cypher-ci-secrets.env`，不要打印或提交。

### P2 durable outbox

- DocsStore migration v3 新增 `chat_outbox`。
- batch 创建时持久化 payload；重启恢复同 batch ID；ACK 只退休对应 batch。
- ACK 路径先在同一 SQLite transaction 保存 snapshot/cursor，再删除 outbox。
- 测试覆盖：顺序、chat 隔离、UUID/clock 顺序、batch ID 复用、I/O 失败、错误 ACK、
  重启恢复、HTTP ACK 失败。

## 最新负载结果

固定同一文本 fixture，tail/checkpoint 边界保持一致：

| 方案 | durable batch | PUSH rows | Tail rows | Checkpoint rows | 总 rows |
|---|---:|---:|---:|---:|---:|
| P0 / 120ms | 243 | 1,218 | 108 | 267 | 1,593 |
| P2 / 2 秒累计实验 | 15 | 78 | 108 | 39 | 225 |

结论：总写入下降 **85.88%**，PUSH 阶段下降约 **93.6%**。

报告：`/tmp/p2-load-final/report.json`
基线 fixture hash：
`65596cad0476e800328666599d28173a163d00f2556bee5a0c6fdf9ae28e801c`

注意：这个 P2 对比是“合法 Loro cumulative export → real workerd ChatRoom → SQLite”，
不是完整云端账单预测。还需要长时间 Engine/客户端运行、混合工具/输入负载、CPU/duration/
latency、ACK 丢失和真实 dev Edge 端到端负载验收。

## 当前运行进程

开发实例使用：

- Engine：当前通常是 `target/debug/cypher headless`，数据目录
  `~/.cypher-development/dev-engine`
- UI：当前通常是 `target/debug/cypher`，工作目录仓库根目录
- IPC：
  `/tmp/cypher-ipc-501/794486d73c4708a26ce31aed112b08c2/engine.sock`
- 另有旧的独立 headless 实例 PID 66501，使用不同 IPC namespace；不要误杀。

恢复对话时先执行：

```sh
ps -axo pid,ppid,command | grep -E 'target/debug/cypher|/Applications/Cypher.app' | grep -v grep
CYPHER_DATA_DIR="$HOME/.cypher-development/dev-engine" target/debug/cypher status --verbose
CYPHER_DATA_DIR="$HOME/.cypher-development/dev-engine" target/debug/cypher sync
git status --short --branch
```

重启前必须确认 pending batch 为 0；当前最后一次检查为 `pending 0`。
如果 Engine 又退出，优先检查日志和 IPC，不要删除 socket 或数据目录。

## 已验证测试

- `cargo test --locked -p cypher-sync --lib`：最新 **50 passed**。
- `cargo test --locked -p cypher-engine --test rows_written_crash_baseline -- --nocapture`：
  通过，输出 `P2_CRASH ... durable_outbox=true`。
- `npm --prefix edge test`：最新 **166 unit + 41 workerd passed**。
- 三端 preview vectors：Rust/TypeScript/Swift 通过。
- iOS 全套 Dev tests：之前 **177 tests，176 passed，1 expected skipped**；原生互通测试
  通过 `scripts/test-preview-native.py` 单独运行。
- `python3 -m unittest discover -s scripts/ci -p 'test_*.py'`：28 passed。
- Dev binary build：`cargo build --locked -p cypher --features development` 通过。
- 全仓 `cargo fmt --all --check` 仍可能报告合入 main 的 MCP 文件既有格式差异；不要用
  全仓 fmt 覆盖未相关文件。

## 下一步建议

1. 不要立即扩大功能范围或部署生产。
2. 先审查当前未提交 P2 diff，确认 `ChatClient` 的 2 秒合并窗口、force flush 和
   `acknowledge_outbox` 的锁/错误语义。
3. 增加真实 `ChatClient → workerd` 的长时间 1/3 viewer 负载，记录 batch 数、payload
   bytes、SQLite rowsWritten、rowsRead、CPU/duration（本地只能测前两类和 SQLite）。
4. 补齐关键业务边界：完成、失败、Steer、Interrupt、RespondInput、工具开始/结束、
   正常退出；确认每个边界会 flush 且不会把新写入误标为已 ACK。
5. 测试 ACK 丢失、上传期间新写入、重连、DO hibernation、磁盘写失败，以及 outbox
   与 snapshot/cursor 的恢复一致性。
6. 只有上述测试稳定后，才考虑把 2 秒节奏作为 dev 默认；生产仍需单独审批。

## 重要风险

- 120ms 本地刷新不能直接等同于 120ms 云端写入；当前 P2 正在把两者解耦。
- preview WS 不是 durable ACK，也不能证明云端文档已持久化。
- Durable Objects 的真实生产计费还涉及 CPU、duration、请求、R2、通知/registry 写入。
- 生产环境未启用 preview，也未改变正式 Worker、正式 DO class 或正式客户端协议。
- 当前方案仍不能宣称宿主断电零丢失；SQLite `WAL + synchronous=NORMAL` 与 journal
  `flush` 的 fsync 边界仍需明确。

