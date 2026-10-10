# Ephemeral stream v1 — P1 协议切片

状态：**Rust/iOS codec、Engine 发送和 Desktop/iOS 展示已实现，默认关闭；服务端 relay 已移除**。
Edge 的开发 relay 及其 `dev-locked` 鉴权已随托管开发 Worker 一并删除，Edge 的 TS codec
（`stream-preview.ts`）随后也已删除（可从 git 历史取回）；生产 `ChatRoom` 对 `0x20–0x26`
帧回 `bad_frame`（`apps/edge/test/workerd/preview.workerd.test.ts`）。启用本功能需在新服务端
（`docs/plans/MIGRATION.md` §13 UX-2）上按下文契约重建 relay。正式设备身份、全故障矩阵
与云端性能仍待验收。现有 HELLO、STATE、PUSH、ACK 和 durable 提交频率完全不变。

## 协商与权限门禁

- 能力名 `ephemeral-stream-v1`。将来只能在显式开启的开发房间协商；双方未明确
  确认前不得发送新帧。生产、旧客户端、未知能力、恢复中的房间一律 legacy。
- 房间协商结果绑定当前参与连接集合与执行 epoch；新加入者能力未知时立即撤销
  新模式资格。P1 仍维持旧 durable 频率；P2 才实现 flush + ACK 后模式切换。
- `epoch` 是**可信执行权授予方**分配的不可复用 ID，不是时间戳，也不比较大小。
  转发方必须以服务端验证的发布连接及 chat/run/segment/epoch 授权记录校验来源。
  下列 header 字段和现有 HELLO.device 均不是身份凭证。
- 当前 ChatRoom 只有用户所有权认证，HELLO.device 可由客户端自报，**不足以授予
  preview 作者权限**。relay 须用独立发布凭据补充连接级授权，具体见下节；
  这不是正式设备身份机制。codec 本身仍只校验格式，不授权。
- DO 唤醒后不能相信丢失的内存协商记录；重新确认授权与能力。允许握手级持久操作，
  禁止每个 delta 写 SQL、serializeAttachment、storage.put 或用永久定时器保活。

### relay 授权与协商契约（重建时须满足）

- 沿用 `/chat2/:chatId/ws`，不新增 namespace/端点。正常登录认证及
  room owner 校验先执行；路由层已有的 chatId 重写用于绑定房间，不相信客户端自报 device。
- relay 必须显式启用并持有独立的发布凭据（原实现为 64 位小写十六进制
  `DEV_PREVIEW_PUBLISH_TOKEN`），否则关闭；误设环境变量不得在生产入口启用它。
- 支持端在 WS upgrade 发 `x-cypher-preview-capability: ephemeral-stream-v1`；
  Engine 另发 `x-cypher-preview-publisher: <独立发布凭据>`。观看端不持有此凭据。
  发布凭据不得放 query、HELLO、附件、iOS 登录配置或日志；不得复用现有登录 token。
  后续应由 Engine 单独私有配置读取，不写入 Desktop/iOS 共用的开发登录凭据文件。
  原生 Engine 已支持从自己的进程环境读取发布凭据；iOS 不读取此凭据。
- 每个活跃连接都完成 HELLO 且声明精确能力，并有验证后的发布连接，服务端才发
  `StreamState(mode=ready)`。发布者发 Start 后，服务端生成 UUID epoch 并广播 grant。
  一次只允许一个发布者和一个活跃 run/segment。发布者接管会关闭旧发布连接；旧连接
  的迟到帧和 close 不撤销新连接的 grant。
- 任意连接加入、有效连接退出、room reset 都撤销当前 grant。legacy/未知/未 HELLO
  连接存在时不得 Start；不向 legacy 连接发送新增控制帧。全部就绪后 Engine 必须重新
  Start、等待新 epoch、发完整 Snapshot，不能接着发送旧 epoch 的 delta。
- 执行权、协商与流控只在实例内存中；不把角色/发布凭据写进 serializeAttachment。
  冷启动后，旧连接不能通过 HELLO 或自报 epoch 恢复权限，需重新连接验证发布凭据。
  旧未知连接尚存时阻止新 grant，宁可退回 legacy 也不沿用不可验证的权限。
- grant 空闲 60 秒后，在下一条预览消息上惰性失效，无后台计时器。Snapshot 必须先于
  Delta，Delta 的 prevRevision 必须匹配当前 revision，baseSeq 不得倒退。Finished 不
  持久化任何状态，之后只允许同 revision 的恢复快照，不允许继续追加。
- 上限：8 个连接；每连接每 10 秒 100 帧/1 MiB；每观看端最多 32 个未 receipt 帧/
  256 KiB。Receipt 只释放该连接已发送 epoch/revision 的预览流控额度，不是 durable ACK。
  epoch 切换不清空未消费额度；旧 epoch receipt 仍可释放对应额度，不能用反复 Start
  绕过慢端限制。超限关闭慢端并撤销 grant，走已有重连/持久同步恢复。
- 每连接总计最多 128 条服务端 State/error 控制帧，达到后要求重连，避免无消费确认的
  控制队列无限增长。这个保守开发限制并非生产调优结果。Resume 每观看端每秒最多转发
  一次，由 Engine 广播快照；DO 不缓存全文。超长累计文本退回无 grant 状态，不截断。

安全边界：独立开发发布凭据提供“有发布凭据 vs 只有观看凭据”的区分，不提供多 Engine
设备间的密码学身份隔离。正式启用仍需可信设备注册、密钥/撤销等独立设计与验收。

## 二进制帧

沿用 `[type:u8][headerLen:u32LE][header:UTF8 JSON object][payload]`。
新 codec 独立于旧 Chat2 分发器；不能借用 PRESENCE 或 PUSH 绕过协商。

| type | 名称 | 附加 header | payload |
| --- | --- | --- | --- |
| `0x20` | StreamDelta | `prevRevision` | 非空 UTF-8 追加文本 |
| `0x21` | StreamSnapshot | 无 | UTF-8 本段完整文本，可空 |
| `0x22` | StreamResume | 无 | 必须空；revision 为最后接收值，0 表示无预览 |
| `0x23` | StreamFinished | `batchId` | 必须空；关联最终 durable 批次，不是 ACK |
| `0x24` | StreamStart | 仅 `chatId, runId, segmentId` | 必须空；客户端不能指定 epoch |
| `0x25` | StreamState | 见下文 | 必须空；只允许服务端发送 |
| `0x26` | StreamReceipt | 无 | 必须空；仅预览流控，不确认 durable |

除 Start/State 外，header 必须包含 `chatId, runId, segmentId, epoch, revision, baseSeq`，不接受
未知字段。四个 ID 与 batchId 为 1–128 个 ASCII `[A-Za-z0-9._:-]` 字符。
revision/baseSeq/prevRevision 为 JSON 非负整数，最大 `2^53-1`，不接受 bool/string。
State 的 header 为 `{chatId, mode}`，mode 为 `legacy` 或 `ready`；授权时为
`{chatId, mode:"preview", runId, segmentId, epoch}`。未知 mode/附加字段均拒绝。
Delta 要求 `revision = prevRevision + 1`；Snapshot/Resume/Finished 可为 revision 0。
baseSeq 是作者生成该帧时已知的持久基点，**不是接收端可以直接推进的 cursor**。
Delta 的 payload 是追加片段，不是替换文本；替换/纠正必须发 Snapshot 并提升 revision。

限制：整个帧最多 65,536 字节、header 最多 4,096 字节、文本最多 61,440 UTF-8 字节。
超长段应暂停该段预览并继续 legacy durable，不截断 Unicode、不静默丢 durable 数据。
首次切片只表示文本；工具、命令、状态仍走 durable，不在文本里塞可执行 JSON。
发送者也须经过同一 codec 校验。接收方不依赖 JSON key 顺序。

## 接收与恢复契约（后续 reducer 必须测试）

展示 key 为 `(chatId, runId, segmentId, epoch)`，epoch 必须先由可信控制路径激活。
数据帧不能自行激活新 epoch。旧 epoch 的迟到数据全部丢弃。

- Delta：重复/更旧 revision 丢弃；仅 prevRevision 等于当前 revision 时追加。
  缺口进入 awaiting-snapshot 状态，合并 Resume 请求，期间不继续猜测追加。
- Snapshot：来自当前作者且 revision 不旧于当前显示时，替换该段预览并退出缺口状态。
  运行中加入/DO 缓存丢失时由 Engine 提供快照，不能要求 DO 保存历史。
- Finished：不删除预览，不改变 Working/Done，不推进 cursor。缺少最终 durable
  覆盖证明时保留为暂存；仅看到 batchId 或 ACK 不等于文档已经导入。
- 断线或快照超时显示“暂存/中断”，不能伪造完成。每连接的快照请求、活跃段数、
  缓存字节、发送队列和 TTL 必须有界；具体调度/背压值在接入批次中实现并测试。

## durable 覆盖与替换

不能通过文本前缀、长度、baseSeq 或“收到任意新 row”推断覆盖关系。
作者应在**同一 Loro 提交**中写入文本及其展示覆盖标记
`(runId, segmentId, epoch, revision)`，使 checkpoint 同样包含覆盖信息。
标记不创建 command，不在每条 preview 到达时更新，只随原有 durable 提交更新。
接收端必须在成功导入 durable 文档并读取同 epoch 的覆盖标记后，才可以退休已覆盖
的预览；周期提交不能盖掉更高 revision 的预览。Finished.batchId 可辅助匹配最终 row，
但不能替代文档中的覆盖标记（该 row 可能已被 checkpoint 裁剪）。

原生映射为 `meta.previewCoverage` JSON 字符串，字段是 `runId, segmentId, epoch,
revision, complete`；segmentId 使用实际 assistant entry ID。SegmentWriter 在提交文本/
状态的同一事务末尾写入标记；没有 preview hook 时不增加操作。重建 checkpoint 保留标记。
`complete` 表示该文本预览停止，不表示 Agent Done（工具/输入边界也会停止文本预览）。
不改 SessionMessageEntry 的持久 schema、Chat2 row 格式或 Loro lineage。

Engine 的显示投影先读 coverage、后读 entries，避免用新标记配旧文本。iOS 从同一
deep-value 投影取得两者并一起更新缓存。只有匹配的已物化 assistant entry 和匹配
epoch/run/segment/revision 才退休预览；仅 cursor、文本前缀或 Finished hint 都不算证明。
预览不写 Loro，不触发 saver/命令/通知；iOS 的 command basedOn、输入请求和 pending
echo 清理仍使用 durable entries，而非加了预览的显示数组。

原生发送端暂不发 `StreamFinished`：当前 outbox 没有可靠的提交到 batch ID 原子映射，
不能猜测一个 ID。结束依靠最终持久覆盖标记；三端 codec 和接收方仍支持 Finished hint，
但不把它当确认。P2 补齐 outbox 后再决定是否需要发送这个可选提示。

### 原生发送与显示边界

- 首版仅预览单一纯文本 part；工具、输入、错误、混合 parts 或超过 60 KiB 的段保持
  durable 展示，不把它们编码成可执行预览状态。下一纯文本段可重新协商。
- 发送缓存只有最新 source/sent 文本；正常追加走 Delta，跳过多个本地 revision、非追加
  修改或 Resume 时发 Snapshot。待发送控制队列最多 4 项，receipt 按 epoch 取最高 revision。
- 预览发送用有界通道 `try_send`，不阻塞 durable。每秒 transport tick 可重试快照；
  Start 5 秒无回答可重试。断线清除授权、旧增量不能复用。预览/其 error 不满足 durable
  ACK、补行或 probe 时限，也不会导致 durable batch 被拒绝或额外启动 HTTP recovery。
- 展示会标注“实时预览 · 本段结果待确认”。片段被周期性 durable 覆盖后使用持久文本，
  但保留同高度的提示至该段预览结束，避免每个 ACK 让提示反复出现/消失造成跳动。
  断线、legacy 回退或 60 秒无新数据后变为
  暂存；若已有 durable entry，显示其内容并加提示，不用过时预览覆盖更新的持久文本。
  未确认缓存按 5 分钟惰性 TTL 在 transport tick/重连活动时过期，最多保留一段。
- 所有工具/命令执行、通知和派生上传仍只订阅持久状态。P1 没有降低 120ms 提交频率，
  因而没有宣称已经节省 rows_written。

### 开关与复现

- macOS 必须是 `development` feature 构建、Development profile、独立 dev Edge 或
  loopback Edge，且显式设置 `CYPHER_DEV_STREAM_PREVIEW=1`。
- 执行 Engine 另持有 `CYPHER_DEV_PREVIEW_PUBLISH_TOKEN`；远端观看 Engine 不设置此项。
  原生调试输出不含 token。Dev UI/iOS 启动脚本主动清除此发布凭据。
  工作区中未知/无法读取的 chat row 不授予发布角色；若 WatchDocMessages 先于 CreateChat，
  确认本机成为 host 并启动 Run 后通过同一 ChatClient redial 升级角色，保留待确认队列。
- iOS `CypherDev` 用 `-dev-stream-preview` 启动；正式构建恒为关闭。独立单元测试可在
  AppConfig 中显式注入开发开关，但同样受构建/地址限制。
- 服务端 relay 已移除（见上方状态），客户端开关在重建前不会产生预览。
- 共享向量：`protocol/vectors/stream-preview-v1.json`（wire，48 例）与
  `preview-reducer-v1.json`（状态机，13 例）。macOS job 的 "Preview protocol" 步骤运行
  `cargo test -p cypher-sync --lib preview`，并直接编译 iOS 的 Swift 源文件执行同一份
  向量（`scripts/tests/stream-preview-vectors.swift`）。iOS Simulator 另有
  `CypherTests/StreamPreviewTests`、`PreviewProjectionTests`。
