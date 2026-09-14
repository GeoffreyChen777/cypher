# P0：rows_written 基线与本地恢复审计

日期：2026-09-14。被测业务代码基线：`933d6db`。
本次新增测试/报告，不改变同步协议、提交频率、生产 DO 或线上客户端。

## 结论

1. 当前 Chat2 稳态 PUSH 在本地真实 workerd SQLite 中产生 **5 个 rowsWritten**：
   rows INSERT（含唯一索引）2，headSeq / pushOutcomes / backupDirty 各 1。
   元数据第一次插入稍多，不能把 batch 数直接当写入行数。
2. 1 与 3 个观看者的同一份历史，SQL 写入量相同；广播帧数/字节增长。
   本结果不表示观看人数对 CPU、duration 或其他路径没有影响。
3. 同 batch ID 的 WS 和 HTTP 重试，在现有行尚未被 checkpoint 清理时，写入均为 0。
   所以去掉并行 HTTP 的请求收益不能等额折算成 rows_written 收益。
4. 当前未 ACK 的 replay queue 是内存结构，没有 durable outbox。
5. SIGKILL 测试中，journal 的 TextDelta 幸存，不代表 transcript 可以从它自动重建。
   **在增加云端提交间隔前，必须先解决未确认更新的本地可靠恢复。**

## 固定负载与可复现结果

`crates/doc/examples/rows_written_fixture.rs` 用固定 peer ID、时间和输入生成真实
SegmentWriter/Loro 增量；新建 Loro 文档依次导入所有增量，断言最终 entries 一致。
主负载：240 次文本追加，每次模拟间隔 120ms，总文本 UTF-8 长度 10,560 bytes。
混合负载在第 80 次追加处加入工具开始/结束，共多两个提交。

测试直接调用实际 ChatRoom handler、RegistryRoom 和 Notifications，底层为真实
DO SQLite；用包装器读取每条 SQL 的 rowsWritten/rowsRead。模拟 WS 接收器和时钟，
**不是实际网络或完整 Engine 调度器压测**，不含 DevelopmentGuard 的额外写入。

固定维护日程：约 1 秒一次 tail（27 次），第 200 个文本 tick 及结束时各一次
checkpoint。结束 checkpoint 是固定测试边界，不表示当前 Engine 每轮结束都这样做。
故此结果适合优化前后的同负载比较，不是线上每轮费用预测。

| 负载 | 观看者 | PUSH 批次 | PUSH 写入 | Tail 写入 | Checkpoint 写入 | 合计 |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| 文本 | 1 | 243 | 1,218 | 108 | 267 | 1,593 |
| 文本 | 3 | 243 | 1,218 | 108 | 267 | 1,593 |
| 文本 + 工具边界 | 1 | 245 | 1,228 | 108 | 269 | 1,605 |
| 文本 + 工具边界 | 3 | 245 | 1,228 | 108 | 269 | 1,605 |

文本负载的 SQL 分解：
- rows INSERT：486；headSeq、pushOutcomes、backupDirty：各 244。
- tail：blob DELETE 26 + blob INSERT 54 + content-type metadata 28。
- checkpoint：删除 243 个日志行计 243；blob 和元数据更新合计 24。
- 另外报告初始化 DDL 9 个观测写入；夹具直接种下的 owner、去重校准新行和
  alarm API 调用不纳入主负载合计。没有模拟午夜 R2 备份、读者重新上线和故障重试。
- 索引对照表：相同单行 INSERT，无二级索引为 1，有 UNIQUE 索引为 2。
  数字来自原生 cursor，而非按 SQL 语句数量推算。

文本稳态主负载输出：1 个观看者 486 帧 / 53,112 bytes；3 个观看者
972 帧 / 137,902 bytes。包括发送方 ACK，不含 WS/TLS 封装、握手和校准请求。
混合负载分别为 490 帧 / 53,744 bytes、980 帧 / 139,618 bytes。

其他路径独立采样（不是上述文本合计的组成部分）：
Registry 样本禁用了通知策略，避免与下面单独测量的 notification 写入重叠。

| 路径 | 初次 | 后续每次 |
| --- | ---: | ---: |
| Registry 会话 working + 更新时间更新 | 11 | 5 |
| Notifications event：working 初次 / 同状态新时间戳 | 6 | 1 |
| Notifications activity：后台、无选中 chat | 2 | 1 |

通知夹具显式开启服务但不配置收件人、不实际发送 APNs；前台 target、徽标、
通知 outbox 等情况可能产生额外写入。之前客户端已经过滤部分重复事件，不能将
服务端单次成本直接乘以前的 HTTP 总数当作新客户端的实际消耗。

纯文本负载重复运行，fixture hash 与所有报告字段完全一致：
`65596cad0476e800328666599d28173a163d00f2556bee5a0c6fdf9ae28e801c`。
混合负载 fixture hash：
`727ce14a24c2b87f153a3815fb965ae287cab5ede1776ac7ec0802b909139b11`。
只对锁定依赖和该夹具版本有效；更换编解码版本后重新建立基线。

## SIGKILL 复现与保证边界

测试：`crates/engine/tests/rows_written_crash_baseline.rs`。
父进程只启动并杀死它自己创建的 helper，不操作任何现有 App/Engine。

步骤：
1. 子进程组装真实 EngineCore、RunJournal、DocHost、DocsStore 和 EngineChatSink。
2. 本地模拟 WS 服务给出已确认 baseline，cursor=1。
3. journal 写入 TextDelta，同时修改 doc，经真实 local-update 订阅加入 ChatClient；
   服务收到 PUSH 但不回复 ACK，确认 pending=1。
4. 分别测试「不保存新快照」与「显式 flush 快照」；冻结单线程 executor，避免
   debounce 时钟抢先落盘；父进程发送 SIGKILL，确认退出 signal=9。
5. 重新组装 EngineCore，检查实际恢复后的 transcript、journal 与 SQLite。

| 杀进程前状态 | journal 新文本 | 重启后的 transcript 新文本 | 保存的 cursor |
| --- | --- | --- | --- |
| 未保存新快照、未 ACK | 存在 | **不存在** | 1 |
| 已保存新快照、未 ACK | 存在 | 存在 | 1 |

此测试验证组件在 EngineCore 进程中的组合，不是完整 CLI/真实模型端到端故障注入。
本地 WS 模拟服务仅确认收到了待处理数据，不能证明远端已持久化；有意不回复 ACK。
后续还要覆盖「远端已持久化但 ACK 丢失」的独立服务生命周期场景。

源码审计：
- RunJournal `append` 使用 File.write_all/flush，没有 sync_data/sync_all；进程被杀
  与机器断电是不同的持久性等级，本测试不能证明断电数据安全。
- DocsStore 使用 SQLite WAL + synchronous=NORMAL。成功提交通常可跨进程崩溃恢复，
  不等于最近事务已被强制同步到持久介质。
- ChatClient pending/in_flight、DocHost chat2_pending_local 都是内存队列。
  数据库只有 snapshots、processed_commands、schema_migrations，没有 batch outbox。
- `EngineChatSink::advance_cursor` 在 ACK 后保存当前文档+cursor；保存失败只记录
  日志，接口没有把失败传回传输确认逻辑。
- DocHost 周期性保存本地文档，却不保存未 ACK 的 batch ID/因果覆盖范围。
- 重启首次打开 cursor=0 的文档会导出全量 update 作为首个 batch；cursor>0 不走
  此重播分支。保存的新文本可能由后续 checkpoint 等路径恢复到云端，但当前不能保证
  原未 ACK batch 会被自动重发，也没有给出这种补偿的严格时间界限。
- `recover_stale` 会标记中断、尝试恢复 harness 会话，不是逐条 TextDelta 的 transcript
  重建器；存在非空历史时，salvage 也不等于补齐最后一段文本。

## P2 前的硬性前置条件

在保持现有协议/文档格式的前提下，先确定并实现本地 durable outbox 或等价可重放
覆盖信息：写入先于发送、失败可见、重启可重构、ACK 只能退休对应覆盖范围。
必须分别测试进程崩溃、I/O 失败、已提交但 ACK 丢失、ACK 与新写入并发等情况。
fsync 策略及宿主磁盘永久丢失时的尾部恢复窗口，需要明确契约后再实施。
P1 可以继续做关闭状态的无落盘预览，但 **不能提前降低持久提交频率**。

## 复现

```sh
# 本地执行，不需要 Cloudflare token，也不访问 dev Edge。
bash scripts/rows-written-baseline.sh /tmp/cypher-p0-text
bash scripts/rows-written-baseline.sh /tmp/cypher-p0-tools tools
cargo test --locked -p cypher-engine --test rows_written_crash_baseline -- --nocapture
```

脚本输出 fixture.json、workerd.log、report.json（含 SQL 逐类计数及来源状态）。
本地证据位于 `/tmp/cypher-p0-{text,tools,repeat}/` 和 `/tmp/cypher-p0-crash.log`。
helper 的 ignored 标记只防止直接运行，它由父测试实际执行两次。
这是旧行为的 characterization test；补齐恢复后应将相应断言升级为不丢失的保证，
不要为了让基线测试保持绿色而保留已知缺陷。

## 尚未验证

- 云端 Worker/DO CPU、duration、网络 p95/首字延迟、慢读端背压；本地模拟时钟不能
  产生这些生产指标。这些应在后续真实客户端固定负载阶段补齐。
- 全命令矩阵、长时间超 300 PUSH/min quota 的行为、午夜备份和大型 checkpoint。
- 断电/磁盘故障及完整 Engine 重连后精确 batch 重播的保证；P0 已发现前置缺口。
- 账户账单金额和超限当天各 DO class 的占比。

因此本报告建立了**本地 SQL 基线与进程崩溃证据**，不是生产用量归因、优化后收益
报告，也不是可以立即部署双通道协议的验收证明。
