# Durable Objects rows_written 优化实施计划

状态：P0 本地基线完成（云端指标待补），P1 原生发送/展示已接入并通过本地原生互通；dev 云端联调进行中。分支：`feat/rows-written-optimization`。基线：`933d6db`。

进度（2026-09-14）：P0 本地 SQL 基线及 SIGKILL 恢复审计已落地，详见
[`docs/rows-written-baseline.md`](docs/rows-written-baseline.md)。发现无 durable outbox、
journal 不自动补齐未保存 transcript 的前置缺口。P1 已完成本地预览链路；P2 不得在恢复契约
补齐前降低持久提交频率。云端 CPU/duration/延迟测量仍未完成。

本计划只规划开发工作，不授权部署生产、新建生产命名空间、数据迁移或发布版本。
先在 Local/workerd 验证，再用现有 dev Edge 做受限的跨客户端联调。

## 1. 问题与目标

用户收到的是 **Durable Objects 免费套餐每日 100,000 rows_written 超限**通知，
重置时间为 2026-09-12 00:00 UTC。问题不是每个 batch 是否算一次 Worker HTTP 请求，
而是流式文本更新在云端落盘时产生的 SQL 写入。

当前路径：

```text
Runtime delta → Engine 合并约 120ms → SegmentWriter / SessionDoc commit
  → Loro update batch → Chat2 WS PUSH → SQL rows + metadata → ACK / 广播
```

代码依据：
- `crates/doc/src/constants.rs`：`STREAM_COMMIT_MS = 120`。
- `crates/engine/src/sessions.rs`：本地事件折叠和定时提交。
- `crates/engine/src/doc_host.rs`：本地更新订阅直接进入 ChatClient 队列。
- `edge/src/chat-log.ts`：appendRow 写入 rows，并更新 headSeq。
- `edge/src/chat-room.ts`：pushOutcomes、backupDirty 等附加写入；checkpoint 删除旧行。

**一条逻辑消息、一条 INSERT、一次 DO invocation、一个计费写入行不是同一个单位。**
索引维护、UPDATE、DELETE、blob 分块、通知和 registry 也会产生写入。不能用
HTTP 请求下降比例替代 rows_written 下降比例，也不能假设固定 key 的 UPSERT 免费。

目标：
1. 远端仍能平滑看到生成中的文本。
2. 不再将每个实时文本片段写入 DO SQLite。
3. 保留最终 transcript、命令账本、恢复及跨设备一致性。
4. 以真实 SQLite 写入计数和固定工作负载证明收益，不再只估算请求数。

非目标：重做整个同步系统、放弃现有 Loro 文档、删除旧聊天、迁移正式 DO，
或在这次工作中顺带优化全部 registry/activity/通知协议。

## 2. 推荐设计：实时预览与持久文档分离

### 2.1 本地执行和持久性

- Engine 继续拥有执行、事件 journal 和权威 SessionDoc。
- 本地 UI 不必因云端写入节流而降低刷新频率。
- 审计 journal 写入、fsync、快照与 outbox 的现有保证；不能仅因调用了 append
  就宣称宿主突然断电也不会丢数据。
- 在降低云端提交频率前，确保未获得 durable ACK 的更新在进程重启后仍可恢复。

### 2.2 实时通道（ephemeral）

```text
执行 Engine → StreamDelta / StreamSnapshot → DO 只转发 → Desktop/iOS 预览层
```

- 帧携带 chat/run/segment 标识、执行 epoch、revision 和关联 durable 基点。
- DO 验证发送者权限、消息大小和速率后转发；**普通预览帧不执行 SQL 写入**。
- 预览数据不直接导入接收方的持久 Loro 文档，不推进 durable cursor，不触发
  registry、tail、checkpoint 或通知状态写入。
- 接收端维护独立、可丢弃的展示层；最终由持久文档取代，不重复显示或重复执行。
- 增量须有单调 revision；重复丢弃，缺口请求预览快照，不猜测缺失内容。
- 预览缓存受字节数、活跃 run 数和过期时间限制；慢消费者可跳过增量，改用最新快照。

**不把 DO 内存当可靠状态源。** Hibernation、部署和驱逐都会使缓存消失。
优先由仍在线的执行 Engine 响应预览快照请求；DO 缓存只是可选加速，不用
永久定时器或逐帧 serializeAttachment 来维持它，以免转而增加 duration/写入。

### 2.3 持久通道（durable）

- 首版保留现有 Chat2 rows / checkpoint 格式与 Loro lineage。
- 文本生成期间由 Engine 以较低频率导出**一个覆盖累积操作的 Loro update**，
  初始实验间隔 2 秒，同时设置累计字节阈值；间隔可在开发实验中调整。
- 不是把 120ms 的多个 batch 排队后逐个发送：那样只是延迟写入，没有减少行数。
- 不能直接拼接任意 Loro 二进制块，也不能跳过提供因果依赖的操作。使用受支持的
  version-vector/export 语义，并验证合并后的文档能独立收敛。
- 用户消息、Run/Steer/Interrupt/RespondInput 命令及关键输入/完成/失败边界
  及时走 durable；工具进度可合并，工具开始/结束不无条件等到周期届满。
- 在途 ACK、重试和新写入分开管理：固定 batch ID、固定覆盖范围，ACK 只确认
  对应批次；不能把发送期间新增的操作误标为已确认。
- 最终提交必须覆盖本段所有操作和结束状态；持久 ACK 成功后才算云端同步完成。
- 先用累计增量减少写入，不引入每两秒上传整段全文的固定 key 快照方案。
  全文 UPSERT 仍可能产生大 blob、多行/索引写入和重复传输，需测量后再考虑。

## 3. 必须保持的正确性与安全约束

1. **两种确认分离**：preview receipt 不是 durable ACK；可见不代表云端已持久化。
2. **两套游标分离**：preview revision 与 Chat2 seq/cursor 不可混用。
3. **无执行副作用**：预览帧绝不创建或执行 durable command。
4. **作者权限**：WorkOS/dev token 认证只是第一层；同用户的观看设备不能伪装执行者。
   发送权与可信 host/执行 epoch 绑定，不能仅相信帧中自报的 device/run ID。
5. **重连换代**：连接、run、segment、steer 切段和 host epoch 的变化必须显式处理，
   防止旧连接迟到帧覆盖新运行。
6. **替换规则**：durable 数据覆盖到哪个 preview revision 必须可判定；晚到的周期
   提交不能使预览倒退，最终结果也不能出现两份 assistant 消息。
7. **断线恢复**：socket 活着或收到 pong 不表示业务有进展；保留 ACK/补行期限与 HTTP 恢复。
8. **崩溃恢复**：正常退出尝试 flush；SIGKILL/断电不能依赖退出回调，重启后从本地
   durable outbox/journal 补交。失联执行者的预览标记为暂存/中断，不能伪造完成状态。
9. **故障窗口如实说明**：宿主磁盘永久丢失时，尚未提交到云端的内容可能丢失。
   在默认启用前明确并确认该恢复窗口，不能以“最终会提交”承诺零损失。
10. **派生写入不得反向放大**：tail、checkpoint、备份、通知不得订阅每个 preview。
    现有通知状态变化与用户输入请求仍由可信的持久业务状态驱动。

## 4. 协议与兼容策略

拟议能力名：`ephemeral-stream-v1`。帧名称暂定，实施阶段确定精确字段与编码：

| 帧 | 作用 |
| --- | --- |
| StreamDelta | 指定 epoch/revision 的文本或展示状态增量 |
| StreamSnapshot | 当前段的有界完整预览及其 durable 基点 |
| StreamResume | 接收方报告最后预览 revision，缺口时请求快照 |
| StreamFinished | 预览段结束，关联最终 durable 提交，不替代其 ACK |

- 加入明确能力协商，未知版本不误解释为现有 PUSH；Rust/TypeScript/Swift 共用协议测试向量。
- **没有协商成功时保持旧路径**。不能同时给支持端发送新预览，又按旧频率持续落盘，
  然后声称获得了写入收益。
- 开发第一阶段仅在明确启用、能力一致的测试房间开启新模式。
- 后续混合版本首选兼容优先：旧客户端加入时恢复该房间的 legacy 高频 durable 路径；
  切换前 flush 当前累积操作，等待确认，不能丢掉 preview-only 尾部。
- 客户端集、能力/host 记录在 DO hibernation 后需要重新建立；未知状态先恢复或走
  legacy，不靠内存中的旧协商结果继续发送新协议。
- 功能关闭时先停止新预览、补交未确认状态再回到旧路径。不删除旧数据、不改生产
  class 名或迁移历史；回退不得丢弃本地 outbox。

## 5. 分阶段实施

### P0：建立计量基线与恢复契约

- [x] 固定 1/3 个观看端、相同文本量、输出速率、工具事件及运行时长的测试负载。
- [x] 用真实 workerd SQLite `cursor.rowsWritten` 分别统计 rows、meta、索引、
  checkpoint 删除、tail/blob、registry、通知等写入；记录实际事务与 SQL 路径。
- [x] 区分生产路径与 DevelopmentGuard 自身的计量/准入开销（本地基线不经过 guard）。
- [ ] 记录 HTTP/WS 消息量、rows_read、CPU、duration、传输字节、首字及流式显示延迟。
- [x] 审计本地 journal/outbox 的崩溃恢复，明确 durable ACK 与 fsync 边界。
- [x] 固定可重放的 baseline 结果，禁止拿不同长度的运行直接比较降幅。

### P1：协议与展示层（默认关闭）

- [x] 第一批：独立 Rust/TypeScript/Swift codec 与共享向量，保持旧分发器不变。
  契约见 [`docs/ephemeral-stream-v1.md`](docs/ephemeral-stream-v1.md)。这是格式校验，
  当时仅完成格式校验；开发授权见下一项，原生运行路径与覆盖标记仍待实现。
- [x] 第二批：开发专用发布凭据、连接级授权、服务端 epoch、协商/撤销、受限转发。
  workerd 真实 WS + SQLite 验证 1/3 观看端普通预览零 SQL；开发 Guard 开销不包含在内。
  已部署 dev 默认路径；生产不注入实现。原生 Engine 发送、展示和 durable 覆盖标记已接入。
- [x] 定义开发能力协商、epoch/revision、大小限制、开发发布权限和 durable 覆盖映射。
- [x] 实现 Edge 无落盘预览转发及 Engine 发送逻辑，保持现有 durable 同步频率。
- [x] Desktop/iOS 实现纯文本可丢弃预览层、去重、补快照和最终替换。
  macOS 双 EngineCore + iOS Simulator 的 SessionStore/ChatRoomClient 经真实本地 workerd
  互通通过；测试给观看端 durable rows 注入 180ms 延迟，两端均实际显示预览，最终文档
  一致且只执行一个 Run。不是正式云端/真机/屏幕视觉验收。
- [x] 默认关闭路径与 legacy 协议回归通过；P0 固定负载仍是 243 batch / 1,593 写入。
  本阶段是正确性搭建，不宣称已降低用量；完整混合版本/故障矩阵仍属于 P3。

第三批本地验收：Rust 489 通过（6 忽略）；Edge 166 单元 + 41 workerd 通过；
iOS 177 项中 176 通过、1 项按预期跳过（该原生互通项已用隔离 fixture 单独运行通过）。
共享 48 个 wire 向量与 13 个 Rust/Swift 状态转换。最新原生证据见协议文档。
云端开发 Guard 的日操作预算已达 1,000/1,000，未扩大额度或部署。原 dev Engine
还有 6 个未确认 batch，保留其进程，避免在补齐 P2 outbox 前丢掉内存重试队列。

### P2：Engine 累积持久更新

- [x] 第一刀：将 Chat2 pending batch 写入本地 `chat_outbox`，启动时恢复，匹配 ACK
  才删除；DocsStore migration v3 与重开/顺序/单 batch retirement 测试通过。当前只
  补可靠恢复，仍保持 120ms durable 上传频率，尚未宣称降费。
- [ ] 分离本地提交与云端导出节奏，按时间/大小/业务边界导出单个合并增量。
- [ ] 在途 batch 与新累积区间隔离，补齐持久重试和重启补交。
- [ ] 完成、失败、steer、输入请求、正常退出强制 flush；离线时本地可靠入队，
  不要求断线后还能成功上传。
- [ ] 检查 tail 去重不会提前返回而跳过必要 checkpoint；检查在途上传期间最后一次
  内容变化、room reset 和失败重试不会被去重状态吞掉。
- [ ] 去除预览引出的间接持久化，验证少写确实来自更少云端操作而非丢数据。

### P3：故障恢复与兼容验证

- [ ] 完成下述故障矩阵及双向真实客户端测试。
- [ ] 验证功能开关、legacy 切换、冷启动与 outbox 保留。
- [ ] 达到性能门槛后再讨论默认开关及生产 rollout；不自动发布。

### P4：受控云端验收

- [ ] 在 `cypher-edge-development` 部署开发版本，不修改正式 `cypher-edge`。
- [ ] Desktop Dev 与 iOS Dev 实际观看、发 Run/Steer/Interrupt/RespondInput，
  使用 mock harness 和小型测试工作区；然后另行确认真实 Runtime/真机测试范围。
- [ ] 对比 baseline、优化版、混合版本 fallback 的完整计量结果。
- [ ] 形成验收记录、剩余风险与回退步骤，再单独申请生产发布。

## 6. 故障与功能测试矩阵

| 场景 | 必须验证 |
| --- | --- |
| 高频文本、Unicode、Markdown、工具进度 | 预览完整，最终文档逐字段一致 |
| 运行中加入第二/第三个客户端 | 从快照接续，不要求回放全部历史 delta |
| 预览丢帧、重复、乱序、晚到 | revision 检测和快照修复，无重复文本 |
| durable ACK 丢失、错误 batch ACK | 固定批次重试，不误删未确认操作 |
| pong-only 卡住、row gap | 业务期限触发恢复，不永久挂起 |
| DO hibernation、驱逐、部署重启 | 内存丢失后从 Engine/持久状态恢复 |
| Engine SIGKILL/重启、磁盘写失败 | outbox/journal 恢复正确，不提前 ACK |
| iOS 休眠/唤醒、后台、断网再连 | 预览与持久状态重新对齐，不伪造 Working/Done |
| Run/Steer/Interrupt/RespondInput | 命令走 durable，现有幂等执行约束不回退 |
| 多个作者、旧 host、伪造 epoch | 非授权发布拒绝，旧连接不覆盖新运行 |
| legacy 客户端加入/退出、开关切换 | 无丢失回退，无双通道重复导入 |
| 大片段、慢读端、长任务 | 帧/内存/队列有上限，有背压与恢复 |
| tail 在途变化、checkpoint/room reset | 派生数据最终追上，不因去重而停止维护 |

测试层次：纯协议测试 → workerd/SQLite 故障注入 → 原生 Engine 集成 →
Desktop/iOS 模拟器双向联调 → 有单独授权的真机/真实提供方验收。
不要只以“屏幕上出现了文字”判定成功；校验最终文档、命令账本、确认状态和重启后结果。

## 7. 验收标准与测量边界

- 正确性测试全部通过；持久最终文档与 baseline 一致，预览不进入 durable 数据通道。
- 纯文本流固定负载下，**ChatRoom rows_written 至少降低 80%**作为实验目标；
  不是账户总量、所有任务或账单金额的承诺。达不到时报告实际值与原因，不放宽正确性。
- 单个普通预览帧，在不包含握手/协商的稳态路径上为 **零 SQL rows_written**。
- 在相同网络条件下，预览 p95 延迟相对 baseline 增量目标不超过 100ms；
  分开记录网络时间与客户端排队时间，不把本地模拟结果当云端保证。
- 持久更新在正常网络下受配置间隔/大小阈值约束，关键边界及时提交；记录恢复尾部窗口。
- 总传输、CPU、DO duration 和内存不得出现未解释的明显回退；增加预览协议可能多发 WS
  消息，必须同时报告，不能只优化写入而掩盖其他费用增长。

## 8. 开发环境与发布纪律

- 现有 dev Edge 与生产共享账户额度。当前限制：120 操作/分钟、1,000/天、4 并发、
  10,000 观测 SQL 写入软预算、8 个 room、64KiB 帧/请求体。
- 这些限制不足以跑 120ms 的长时间 baseline 压测；基线和耗尽测试优先放本地。
  不为了跑通测试偷偷扩大同账户预算，需要扩容时单独确认。
- 开发预算统计包含 gate 自身开销，且并发/崩溃可能使软预算超出；不是账户级配额保障。
- 开发凭据只读 git 外的私有文件/Keychain，禁止写入源码、日志、测试产物或发布包。
- 官方构建保留 Production 配置及构建门禁；测试入口和开发鉴权不进入 TestFlight/正式包。
- 每阶段完成验证后按 `AGENTS.md` 构建并重启对应 Dev UI；确需重启引擎时先核对
  进程、工作目录、日志、IPC 和活跃任务，保留聊天与配置，不影响正式应用。
- 生产启用必须另行确认兼容矩阵、实际写入降幅、恢复窗口、监控与可执行回退步骤。

## 9. 首个实施切片

从 **P0 的 SQL 写入基线测试 + 本地 outbox 恢复审计**开始，然后冻结最小协议字段和
恢复契约，再做 P1 的无落盘预览。不先重写数据库、不改正式端点，也不只把 120ms
改成 1 秒后宣布问题已解决。
