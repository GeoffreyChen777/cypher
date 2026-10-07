# Cypher 同步后端迁出 Cloudflare：架构决定与迁移计划

> 状态：设计决定 + 实施计划（2026-09-20）。本文件不改动任何代码。
>
> **本文是下一阶段的计划，不是当前正在做的事。** 当前进行中的是 Cloudflare 上的
> 持续测量—优化循环（`docs/local-edge.md`：本地 `wrangler dev` +
> `scripts/edge-billing-local.mjs`，按实测排优先级）。本文描述的 Rust + Postgres
> 自建服务端**尚未开工**，§11 的 WP0 前置问题也还没回答；在那之前不要按本文的
> 工作包表安排实施。
>
> 两者不冲突：循环里做的客户端改动（注册表合并窗口、活动心跳节奏、`touch_session`
> 改 ephemeral）同时就是 §14 给 Rust 服务端定下的规格——做一次，两边受益。反过来，
> 本文 §12 的协议冻结清单与 §12.3 的事故加固行为，是**任何**优化（包括循环里的）
> 都不得破坏的边界。
>
> 前置阅读：`HANDOFF-SELF-HOST-SYNC.md`（流量/账单观察，无结论）、`ARCHITECTURE.md` §1/§2/§6、`docs/chat2-sync.md`、`docs/registry-sync.md`、`docs/ephemeral-stream-v1.md`。
>
> 全文用三种标注区分事实与判断：
> - **【现状】** 代码今天的行为，均附 `文件:行号`；
> - **【建议】** 本文推荐的做法；
> - **【假设】** 未在仓库内验证、实施前必须核对的事项。本文引用的 macOS 笔记本（Node 24）测量值均属此类：方向可信，绝对值必须在真实服务器上重测。
>
> 用户已做的决定（本文不再讨论）：
> - Cloudflare **完全退役**，不做备份或回退目标；用户量小，**可接受计划内停机**，采用 stop-the-world 迁移而非双写。
> - **Rust 原生服务端**（tokio/axum 或等价）重实现 room 协议，不引入 Durable Object 兼容层。
> - 存储用 **Postgres**；**不用 Redis**。
> - **保留 WorkOS** 作为 IdP；**保留主机名 `edge.letscypher.app`**（`apps/cypher/src/main.rs:100-117` 以它门控 WorkOS；iOS 持久化它）。
> - **保留 room-actor 形态**：每个 room 一个单写者内存 actor，拥有自己的 socket；fan-out 是本地循环；串行化由 actor 完成，Postgres 只负责持久化；`seq` 排序**永不**依赖数据库锁。
> - **保留 local-first 契约**：服务端不含 CRDT 逻辑，载荷保持不透明（`edge/src/chat-frames.ts:1-13`）。
> - 大对象（checkpoint、sidecar、附件、发布产物）放**自托管 MinIO**（S3 API，同机部署）；Postgres 只存指针。
> - **保留现有 room 命名前缀**（`chat2/`、`reg1/`、`d2/`、`apns/`）原样导入，不在迁移中改名（§1.4、§2.5）。
> - 初期一台 Linux 服务器（100+ 核、500 GB 内存、NVMe）；地点待定（§11）。

---

## 0. 结论摘要

| 决定 | 结论 |
|---|---|
| 实施策略 | **Rust 原生服务端**：单个 Rust 二进制（tokio + axum + tokio-tungstenite），把 ChatRoom / RegistryRoom / DeviceRoom / PushDevice 四类 room 重实现为进程内 actor；协议按 §4 逐字节冻结，客户端一行不改。 |
| 存储 | **Postgres**（切换时单实例；热备与自动故障转移按 §3.9 后续部署）。room 之间零跨 room 查询，一个数据库承载全部 room 类；`chat_rows` 按 `room_id` 哈希分区以并行 autovacuum；`synchronous_commit=on` 保住"ack 即持久"契约。**不用 Redis**：热状态全部在 actor 内存，没有需要共享的东西。 |
| 大对象 | checkpoint（≤16 MiB）、sidecar、附件、发布产物放**自托管 MinIO**（S3 API，同机；四个 bucket；先 PUT 对象再提交指针）；Postgres 只存 `bucket/key` + sha256 + 大小。应用只依赖 S3 API，后端可换（§3.5）。 |
| 身份提供方 | **保留 WorkOS**。替换 IdP 的成本落在客户端与全部 room 的 owner/用户 id 上，与"退出 Cloudflare"无关（§2.4）。 |
| 主机名 | **`edge.letscypher.app` 不变**（§1.2 第 1、2 条）。 |
| 协议 | 全部路由与帧行为**冻结**（§4）。安全论证见 §12：逐条列出线上行为、标出今天有/没有跨语言向量的，缺的先从 TS 实现导出 golden 测试，再用差分回放把 Rust 服务端与 TS Worker 对跑，作为 WP 测试阶段的验收门。 |
| 切换方式 | 维护窗口内：通告 → 客户端排空 outbox → 服务端冻结写 → 导出 → 导入 Postgres → 校验 → DNS 切换 → 解冻（§6）。DNS zone 窗口内留在 Cloudflare 只改一条记录。 |
| 预计工期 | **约 29–38 个工作日（6–7.5 周，一人）**；关键路径是 WP1 → WP3（Registry + 通知 + 推送）→ WP6（差分测试）→ WP9 → WP11；除切换本身外全部工作包可在窗口前完成并演练（§10）。 |
| 切换后 | UX-1：客户端 2 秒合并窗口降到约 0（自托管后 rows 免费），感知延迟降为 120 ms 提交节拍 + RTT；UX-2：把已建成但默认关闭的 `ephemeral-stream-v1` 上生产，前提是设计设备绑定的发布者身份（§13）。 |

一句话理由：服务端全部协议语义定义在约 4,000 行 TypeScript 里，且 Rust/Swift 客户端已通过共享或镜像的测试向量与之字节级对齐（`edge/src/chat-frames.ts:11-13`、`edge/src/registry-core.ts:1-6`、`crates/rpc/src/device_room.rs:6-14`）；本设计接受"第三份服务端实现"的重写风险，换取一个没有私有运行时层、与客户端同语言、天然多线程的长期形态，并把重写风险收敛到 §12 的可枚举清单和差分测试上。当前负载（月约 131 万次 DO 调用，HANDOFF §2.3，折合约 0.5 次/秒）余量两个数量级以上，"用满 100 核"不是选型依据；选 Postgres 的理由是**可用性路径**（流复制热备 + PITR）与单一备份口径，不是吞吐。

---

## 1. 现状事实：迁移必须保留的东西

### 1.1 组件与 Cloudflare 依赖清单【现状】

| 组件 | 文件 | Cloudflare 能力依赖 |
|---|---|---|
| Worker 路由/鉴权前端 | `edge/src/index.ts`、`auth.ts`、`auth-routes.ts`、`workos.ts` | `fetch` handler、DO namespace binding、R2 binding、Worker secret（`WORKOS_API_KEY`）、`cf-connecting-ip`（仅日志，`auth-routes.ts:151`） |
| ChatRoom（`chat2/{chatId}`） | `chat-room.ts`、`chat-log.ts`、`chat-frames.ts`、`blobs.ts` | DO SQLite（同步 `sql.exec`）、hibernatable WebSocket、`serializeAttachment`、auto-response、alarm、R2 备份写 |
| RegistryRoom（`reg1/{orgId}/{userId}`） | `registry-room.ts`、`registry-core.ts`、`notifications.ts`、`notifications-model.ts` | 同上 + 跨 DO 调用 `PUSH_DEVICES`（`notifications.ts:224,343`） |
| DeviceRoom（`d2/{deviceId}`） | `device-room.ts` | DO SQLite、带 tag 的 `acceptWebSocket`/`getWebSockets(tag)`、`getWebSocketAutoResponseTimestamp`（`device-room.ts:135`） |
| PushDevice（`apns/{env}/{token}`） | `push-device.ts` | **KV 型** `ctx.storage.get/put/list/delete/transaction`（`push-device.ts:29-119`），`blockConcurrencyWhile`（`:20`），service binding `APNS_SENDER`（`:102`） |
| APNs 发送 | `apns.ts`、`apns-sender.ts` | `cloudflare:workers` `WorkerEntrypoint`（`apns-sender.ts:1,6`）；Workers `fetch` 对 Apple 走 HTTP/2 |
| SessionRoom（遗留 `s2/`、`ws4/`） | `session-room.ts`（1,441 行） | loro-wasm、DO 全套；`ARCHITECTURE.md` §1 声明"no current client dials it" |
| 附件与备份 | `index.ts:406-441`，各 room 的 `alarm()` | R2 bucket `cypher-blobs` |
| 发布产物 + 安装脚本 | `index.ts:130-165`、`install.sh` | R2 bucket `cypher-releases`；`scripts/ci/release.py:385-398` 通过 Cloudflare R2 REST API 写入 |
| 部署 | `.github/workflows/deploy.yml:83` | `wrangler deploy` 三个 Worker（edge、landing、www-redirect） |
| 域名与证书 | `edge/wrangler.jsonc:24` | Worker custom domain 自动签发 DNS + TLS；`letscypher.app` zone 托管在 Cloudflare |
| 开发环境 | `docs/local-edge.md`、`scripts/edge-billing-local.mjs` | **无云端依赖**。托管开发 Worker `cypher-edge-development` 及其 6 个 DO namespace、2 个 R2 bucket 已于 2026-09-22 删除；开发环境改为本地 `wrangler dev`。预览 relay 的实现（`development-preview.ts`）保留，但其 Worker 入口已随 `development.ts` 一并移除，生产化时需重新接线（§13 UX-2） |

### 1.2 客户端对服务端的硬耦合【现状】

这些决定了"什么不能改"：

1. **主机名门控 WorkOS**：`apps/cypher/src/main.rs:83` `DEFAULT_EDGE_URL`、`:100` `PRODUCTION_EDGE_URL`、`:116-117`：没有显式 `CYPHER_WORKOS_CLIENT_ID` 时，只有 edge URL 精确等于 `https://edge.letscypher.app` 才启用内置 client id；否则退化为 dev 模式。
2. **iOS 默认 URL**：`apps/ios/Cypher/App/AppModel.swift:32`（`@AppStorage("edgeURL")` 默认值）、`:84-86`（旧主机名迁移到新主机名）、`Views/SignInView.swift:17,29`（WorkOS redirect_uri 为 `https://edge.letscypher.app/auth/ios/callback`）。
3. **WorkOS 授权 URL 由设备自建**：`crates/engine/src/auth.rs:723`（`provider=GitHubOAuth`、PKCE S256），服务端只做持密钥的 exchange/refresh（`auth.rs:807,1000`）。access token 默认按 `exp-iat` 计算 TTL，缺省 240 秒（`auth.rs:217`），到期前 60 秒刷新（`auth.rs:481`）。**服务端必须能出站访问 `api.workos.com`**（JWKS：`edge/src/auth.ts:53-57`；认证 API：`workos.ts:14`）。
4. **WS 鉴权走查询串**：`edge/src/auth.ts:36` 接受 `?token=`；桌面 `crates/engine/src/doc_host.rs:151-153`（拼 `?token=&device=`）、`crates/rpc/src/device_room.rs:168-179`、iOS `App/AppConfig.swift:175-188`。HTTP 请求用 `Authorization: Bearer`。→ 新入口的访问日志必须脱敏 `token` 参数。
5. **TLS 信任根**：Rust 侧 `Cargo.toml:80`（`tokio-tungstenite` `rustls-tls-webpki-roots`）、`:110`（`reqwest` `rustls-tls`）—— 只信 webpki 根集，Let's Encrypt（ISRG Root X1）在内；iOS 用系统信任。
6. **发布/更新/运行时下载**：`crates/update/src/lib.rs:195-215`（`/releases/{channel}/manifest.json`、`manifest.json`）、`crates/engine/src/pi_runtime.rs:550-551`（`/releases/runtimes/pi`）、`edge/src/install.sh:15`（`CYPHER_BASE_URL` 默认同主机名）、`scripts/ci/release.py:692`（`--base-url` 默认值）。
7. **遗留路由仍有写入者**：`crates/engine/src/diff_sync.rs:625` 仍向 `POST {edge}/diff/{chatId}`（SessionRoom `s2/` 路由，`index.ts:217`）发布 diff sidecar；`session-room.ts:319-326` 只写 blob，不触碰 loro。Rust/Swift 中没有 `GET /diff/{chatId}` 的读取者。`/session/*/ws`、`/workspace/*`、`/snapshot`、`/append` 在 `crates`、`apps/ios` 中无调用点。

### 1.3 DO 运行时语义的实际使用面（Rust 必须再现的清单）【现状】

这张表是**语义清单**：每一行都要在 §3.2 的 actor 模型里有对应实现，或明确宣布不再需要。

| DO 能力 | 使用处 | Rust 对应（§3.2） |
|---|---|---|
| `ctx.storage.sql.exec` 同步游标，事件内多语句原子提交 | 所有 room 类；`chat-log.ts:72-91`（行插入 + `headSeq`）、`:135-150`（checkpoint blob + 删行 + 四个 meta，注释明说依赖"commit atomically"）、`registry-room.ts:376-396`（多行 upsert + 通知 outbox + `seq`） | 每个 actor 事件 = 一个 Postgres 事务；COMMIT 返回后才发出任何帧/响应 |
| KV `get/put/list/delete/transaction` | 仅 `push-device.ts:29-119` | 关系表（§3.4 `push_*`）；`transaction` 即同一事务 |
| `setAlarm/getAlarm/deleteAlarm`、`alarm()` | `chat-room.ts:547-549,555`、`registry-room.ts:449-464,468`（`alarmScheduling` 串行链）、`session-room.ts`（遗留） | `alarms` 表 + 进程内调度器；失败重试 |
| `acceptWebSocket(ws, tags?)`、`getWebSockets(tag?)` | `chat-room.ts:112`、`registry-room.ts:165`、`device-room.ts:171,174,129,277,302` | actor 内 `HashMap<ConnId, SocketHandle{tags,…}>` |
| `serializeAttachment/deserializeAttachment` | 各 room | `SocketHandle.state` 字段 |
| `setWebSocketAutoResponse("ping"→"pong")` | `chat-room.ts:89`、`registry-room.ts:73`、`device-room.ts:104`；客户端 15 秒 text `ping`（`chat_client.rs:29`、`registry.rs:34`、`device_room.rs:56`） | 连接任务直接应答，不进 actor，并更新 `last_pong_at` |
| `getWebSocketAutoResponseTimestamp(ws)` | `device-room.ts:135`；host 存活窗 75 秒（`:75`） | 读 `SocketHandle.last_pong_at`（`max(…, joined_at)`，同 `:133-137`） |
| 事件在**非存储** `await` 处交错（input gate） | `development.ts:26` 注释；`push-device.ts:15-19` 刻意不在 APNs 调用期间持锁；`notifications.ts:354-431` 每次 `await` 后重读 `events()` 判断是否被抢先 | actor 主循环 `select!` 同时轮询收件箱与"在途子请求集合"（§3.2(e)） |
| `blockConcurrencyWhile`、`waitUntil`、`abort` | `push-device.ts:20`、`registry-room.ts:464`、`session-room.ts`（遗留） | 单写者 actor 天然满足；`abort` 随 SessionRoom 退出 |
| `ctx.id.toString()`（64-hex） | `notifications.ts:50`（通知 `scope`）、`chat-room.ts:569`、`registry-room.ts:499`（备份 key） | `rooms.id_hex`；**导入时沿用 Cloudflare 的值**（§1.4） |
| `ns.idFromName / idFromString / get(id).fetch` | `index.ts:88,182`、`notifications.ts:224,343` | `RoomHost::get_or_spawn(class, name|id)` + actor 消息；跨 room 只有 Registry→PushDevice 一条，以及 `/notifications/revoke` 按 id 直达 PushDevice |
| `WebSocketPair` + `Response(101)` | 各 room `/ws` | axum `WebSocketUpgrade` |
| R2 `put/get/head`、`httpEtag/writeHttpMetadata` | `index.ts:145,157,421,430-434`、各 room 备份 | MinIO（S3 API）+ 指针表（§3.5） |
| `WorkerEntrypoint`（service binding） | `apns-sender.ts` | 进程内函数 |
| Web 标准全局（`Request/Response/fetch/crypto.subtle/…`） | 到处 | axum/reqwest/ring |

### 1.4 命名、身份与外部持有的标识【现状】

- room 名称不落库：ChatRoom 只存 `owner`（`chat-room.ts:109`），不存 chatId；RegistryRoom、DeviceRoom 同样不存自己的名字；SessionRoom 存 `chatId`（`session-room.ts:229`）；PushDevice 的 registration 含 `token` 与 `environment`（`push-device.ts:8-11`），名字可重建。
- 名称由 Worker 派生：`chat2/{chatId}`（`index.ts:233`）、`reg1/{orgId}/{userId}`（`:332`）、`d2/{deviceId}`（`:378`）、`apns/{env}/{token}`（`notifications.ts:224`）。
- **外部持有的 id 字串**：通知 `scope` = RegistryRoom 的 `ctx.id.toString()`（`notifications.ts:50`），iOS 持久化在 Keychain 并逐帧比较（`apps/ios/Cypher/App/NotificationController.swift:91,273,345,356,378`，正则 `^[a-f0-9]{64}$`，`Models/Notifications.swift:9,69`）；`bindingId` = PushDevice 的 id 字串（`NotificationController.swift:266-273`），用于无鉴权撤销 `POST /notifications/revoke`（`index.ts:172-190`，服务端按 `idFromString(bindingId)` **全局**定位 PushDevice，`:182`）。Registry 的 `recipients[].id` 也是 PushDevice id 字串（`notifications.ts:230-232,343`）。
- 结论：新服务端**必须允许导入并保留这些 64-hex id**，`id → room` 映射必须逐字导入（§3.4 `rooms` 表），并且 `bindingId` 需要一个**全局唯一索引**（§3.4）。这直接排除了由运行时自行哈希 id 的做法（如自托管 workerd，见 §2.1）。

### 1.5 流量量级与初步测量

**【现状，引用 HANDOFF】** `HANDOFF-SELF-HOST-SYNC.md` §2：单个重度用户 30 天外推约 131 万次 DO Analytics 调用、131 万 rows written；Registry 与 ChatRoom 合计约 97% 的 rows。折合约 0.5 次调用/秒。HANDOFF §2.4 已声明这些数字不能直接换算成账单，本文只把它当作**容量量级**。

**【假设：初步测量在 macOS 笔记本、Node 24 上进行，方向可信，绝对值待真实服务器重测】**

- 服务端处理时间只占同步预算的很小一部分：一次 push 的服务端处理 0.6–2 ms，而端到端同步预算约 2,100 ms，由客户端 **2 秒合并窗口**（`crates/sync/src/chat_client.rs:730,752`）和 **120 ms 提交节拍**（`crates/doc/src/constants.rs:15` `STREAM_COMMIT_MS`，`crates/engine/src/sessions.rs:1805-1806`）决定。2 秒窗口的存在理由是压低 Cloudflare rows written（HANDOFF §1.3 fixture：1,593 → 225 rows）；自托管后该理由消失（§13 UX-1）。
- 单个重度用户约 **0.5 条入站消息/秒、约 0.1 次持久 push/秒**（HANDOFF §2 的 rows ÷ 每 push 约 5.2 rows）。
- 一个真实 profile：30 个 chat，文档状态合计 6.8 MB（中位 64 KB，p90 598 KB，最大 1.9 MB），增长约 13 MB/月，附件 340 KB。
- 每设备 socket 数 ≤ 14：`WARM_DOC_CAP=12`（`crates/engine/src/doc_host.rs:47`）个 chat2 房间 + 1 个 registry + 1 个 device room；活跃用户典型约 10 个。
- 单机瓶颈顺序：**上行带宽**（DeviceRoom 字节中继，终端/RPC 流量，未测）→ socket 数 → 之后很长时间内没有别的。CPU 与数据库在单机上永远不是瓶颈。
- 从 Cloudflare 全球边缘收缩到一个区域，对远离服务器的用户是**延迟回退**；最终解法是 room 放置（把用户的 room 跑在离他近的节点），而不是数据库复制。

---

## 2. 设计决定与理由

### 2.1 服务端：Rust 原生实现（tokio + axum）

服务端用 Rust 重实现 ChatRoom / RegistryRoom / DeviceRoom / PushDevice 四类 room 的协议语义，作为单个二进制运行；不引入任何 Durable Object 兼容层。

**可复用的 Rust 部件【现状】**：chat2 帧编解码（`crates/sync/src/chat_frames.rs:34,46`；注意 `:44-45` 注释：客户端版**不拒绝未知类型字节**，服务端复用时必须加 TS `chat-frames.ts:80` 同样的 allowlist）；注册表合并核心（`crates/doc/src/registry.rs:42` `encode_hlc`、`:166` `apply_op`、`:254` `row_to_seed_op`，与 `registry-core.ts` 1:1 镜像并共享向量，`crates/doc/src/registry/tests.rs:1-2`）；设备帧编解码（`crates/rpc/src/device_room.rs:102,134`，含 `byte_parity_with_ts_encoder` 测试 `:1031`）；预览帧编解码（`crates/sync/src/stream_preview.rs`，读共享 JSON 向量 `:116-117`）；`tokio-tungstenite`/`reqwest`/`rustls` 已在工作区依赖中（`Cargo.toml:80,110,111`）。

**需要重写的服务端语义（TS 行数为约数）**：ChatRoom 630 + chat-log 181、RegistryRoom 525、DeviceRoom 348、PushDevice 122、Notifications 462 + model 120、auth 73、auth-routes 331、workos 386、apns 105 + sender 24、index 451、blobs 60，合计约 3,800 行；另加 JWKS 校验、WorkOS 客户端、APNs HTTP/2 + ES256 签名、Postgres 存储层、文件对象存储、alarm 调度器。

选择 Rust 的理由：

- **与设备端同语言**：设备端全部是 Rust（`ARCHITECTURE.md` "Everything device-side is Rust"）；服务端与客户端共用同一仓库、同一套 codec crate，Rust 客户端与 Rust 服务端的字节级一致免费获得。保留一份 TypeScript 服务端意味着长期维护两套语言、两套依赖链、两套构建与部署；而 Durable Object 编程模型在 Cloudflare 之外没有生产级运行时（自托管 workerd 是开发工具，且其 id 由运行时哈希得出，无法满足 §1.4 的 id 约束）。
- **多核**：actor = tokio task，room 之间无共享锁，天然多线程。
- **id 可搬迁**：`rooms(class, name, id_hex)` 表，导入时沿用 Cloudflare 的 id（§1.4）。
- **运维**：单二进制 + Postgres + Caddy，全部是主流组件，没有私有运行时层；将来 room 放置、多节点都是普通 Rust 工程问题。

接受的风险：

- **协议耦合：差**（服务端语义的又一份实现）。下列行为都是事故后加的、今天只存在于 TS 里：重复 batchId 先于配额判定（`chat-room.ts:444-452`、`:249-251`）、注册表 rows 先于 ack 广播（`registry-room.ts:401-408`）、host 存活按最新 pong 选取（`device-room.ts:127-140,328-339`）、nudge 队列 256 上限（`:87,224-230`）、通知状态机对乱序/重放/前台阅读的全部分支（`notifications.ts:110-208,255-322,348-431`）。**缓解**：这些行为可枚举（§12.2），每一条要么已有跨语言向量，要么先从 TS 导出 golden 测试；再加差分回放（§12.4）。
- **测试复用：间接**。166 个单元 + 41 个 workerd 测试（`docs/handoff-rows-written-optimization.md:113`）不能直接跑在 Rust 上，但它们是 golden 测试的**输入与期望来源**；三端共享/镜像向量（§12.1）原样复用；Rust 客户端的 mock-server 测试（`crates/sync`，含 `plan_catch_up` 决策表 `chat_client.rs:187-214`）是服务端行为的第二份规格。
- **通知模块无真机基线**：通知模块尚未完成真机验收（`docs/notifications.md:3-15`，"neither authenticated APNs acceptance nor physical-device delivery has been verified"），在没有基线的情况下重写它等于同时改两件事——这是本设计最大的单点风险，§10 WP3 单列。

### 2.2 存储：Postgres，不用 Redis

- **Postgres，而不是每 room 一个 SQLite**：全部 SQL 本来就要重写，SQL 零改动不构成理由。Postgres 换来：流复制热备 + PITR 的成熟路径（§3.9、§8.2）、一个备份口径、`bindingId` 全局索引（§1.4）、导入时 `COPY` 批量装载与 SQL 级完整性校验（§5.5）。"把所有 room 放进单一故障域"的顾虑在单机上本来就存在（一块 NVMe）；room 间零跨 room 查询使得单库不构成耦合。
- **不用 Redis**：TS 里的 presence、配额窗口本来就是内存态（`chat-room.ts:48-52,78-80`；`registry-room.ts:17-18`）；fan-out 是 actor 本地循环；进程内 actor 已经是"单写者 + 顺序保证"。Redis 只在多 app 节点需要 pub/sub 时才有意义，而那时正确的做法是 room 放置（每个 room 只在一个节点上有 actor），不是广播。

### 2.3 安全网：协议冻结 + 差分测试

1. 安全网不是"同一份代码"，而是 §12：线上行为清单 → 向量覆盖矩阵 → 缺口 golden 测试 → 差分回放。**差分回放 0 差异是 WP 测试阶段的退出条件**，排在 §6 的 Gate A 之前。
2. TS Worker 在切换后 14 天内保持冻结部署（§6 步骤 14）；仓库中的 `edge/` 目录在差分 harness 退役前作为**参考实现**保留（只读），之后归档到分支（§11）。
3. `PLAN.md`/`docs/rows-written-baseline.md` 追求的"减少 rows written"失去成本动机；2 秒合并窗口按 §13 UX-1 处理。

### 2.4 身份提供方：保留 WorkOS

替换 WorkOS 的真实成本：

| 项目 | 保留 WorkOS | 替换为自建/其他 IdP |
|---|---|---|
| 客户端改动 | 无。redirect URI（`/auth/cli/callback`、`/auth/ios/callback`、loopback）随主机名不变 | 桌面 `auth.rs:696-730` 授权 URL 构造、`main.rs:89` client id、iOS `SignInView.swift:24-29` 全部重写；iOS 需经 TestFlight/App Store 发版，迁移与发版耦合 |
| 用户/组织 id | 不变。所有 room 名（`reg1/{org}/{user}`）、`owner` 元数据、`/registry/:orgId` 路径继续有效 | 新 IdP 的 `sub` 不同，需要旧 id→新 id 映射层，或在导入时改写全部 owner/room 名 |
| 会话 | 不变。refresh token 继续有效（`auth.rs:1000` 走 `/auth/refresh`，服务端转发 WorkOS） | 全部设备重新登录；桌面 `WorkspaceScope` 在启动时由 `session.json` 决定（`ARCHITECTURE.md` §1 Local-first） |
| 服务端改动 | 用 Rust 重写 `workos.ts`（386 行，§10 WP5）；错误码语义必须保留：设备只以 `invalid_grant` 判定会话吊销（`workos.ts:16-18`） | 新增登录/GitHub OAuth/JWT 签发/组织管理/邮箱验证（`auth-routes.ts:113-134` 有邮箱验证续流） |
| 外部依赖 | 仍依赖 `api.workos.com` 可用性（今天也如此） | 无外部依赖 |

**决定**：保留。缓解外部依赖：JWKS 缓存加"上游不可达时沿用上次成功结果"的宽限（今天 `jose` 的 `createRemoteJWKSet` 自带缓存与冷却，`auth.ts:21-31`；Rust 侧自实现同样策略）。`Verified {userId, sessionId?, orgId?}`（`auth.ts:14-20`）是将来替换时唯一需要保持的接口。

### 2.5 附带决定

- **对象存储用自托管 MinIO**：Rust 进程通过 S3 API 读写；`release.py:385-398` 的 `R2` 类只用到 `get/digest/put` 三个方法，直接换成指向 MinIO 的 S3 客户端（同一套凭据模型），不需要自建上传 API。选择同机 MinIO 的理由：**S3 作为边界**（换后端只改配置）、多盘时的**纠删码盘级冗余**、将来第二台机器/地域的 **bucket 复制只是配置**、bucket 版本化给附件免费的误删恢复；数据不离开自己的机器。代价：多一个守护进程与它的升级/备份口径；带宽不变（仍在同一上行链路后面）。备选 Garage / SeaweedFS（同为 S3 API，§11）。
- **保留 room 命名前缀**：`chat2/`、`reg1/`、`d2/`、`apns/` 是历史上按"代"废弃旧 room 的产物（`index.ts:197-201,377`；`docs/chat2-sync.md:157`；`docs/registry-sync.md:3,106`）。`chat2` 在客户端 URL 里，`reg1` 的派生 id 被 iOS 持久化为推送 `scope`（§1.4）。迁移中**原样导入、不新增、不改名**；有了 Postgres 之后，身份类变更走 SQL 迁移而不是换前缀，前缀只保留给真正的协议不兼容升级。
- **遗留 SessionRoom 不迁入线上服务**：只做冷归档导出（§5）。`POST /diff/:chatId` 以兼容路由形式保留并落到 chat2 room 的 `sidecar-diff` 槽（§4.1）。
- **codec 共享**：服务端 crate 直接依赖 `cypher-sync`（chat 帧、预览帧）、`cypher-doc`（注册表合并核心）、`cypher-rpc`（设备帧）中的纯函数，而不是复制；Rust 客户端与 Rust 服务端字节级一致由此免费获得，TS/Swift 一致性继续由 §12.1 的向量保证。【建议】后续可把这些纯 codec 抽成 `cypher-wire` 小 crate 以缩小服务端依赖面（不改行为，不是切换前提）。

---

## 3. 目标架构

### 3.1 进程/服务拓扑【建议】

```
Internet ──443/80──▶ caddy (TLS 终止, ACME, 访问日志脱敏 token, 每 IP 连接/速率限制)
                        │ 127.0.0.1:27640  (HTTP/1.1 + WebSocket upgrade 透传, X-Forwarded-For)
                        ▼
                cypher-edge.service  (Rust 单二进制, tokio 多线程运行时)
                ├─ ingress (axum)     路由 §4.1 · JWT 验签 (JWKS 缓存) · 请求体上限 · 丢弃入站 x-cypher-* 头
                ├─ RoomHost           (class,name) → id_hex → actor 句柄; 按需 spawn; 空闲退出; 进程级 leader 锁
                │   ├─ ChatRoomActor      chat2/{chatId}     rows / meta / checkpoint 指针 / presence / quota
                │   ├─ RegistryRoomActor  reg1/{org}/{user}  rows(LWW) / meta / Notifications 状态机
                │   ├─ DeviceRoomActor    d2/{deviceId}      host/client 管道 / nudges / sidecars
                │   └─ PushDeviceActor    apns/{env}/{token} registration / deliveries / badge → APNs(h2)
                ├─ AlarmScheduler     alarms 表 → 到期投递 Alarm 事件 (失败退避重试)
                ├─ BlobStore          S3 客户端 → MinIO (bucket: chat-blobs / attachments) + Postgres 指针表
                ├─ ReleaseStore       S3 客户端 → MinIO (bucket: releases)
                └─ pg pool (deadpool-postgres / tokio-postgres; 或 sqlx)
                        │ unix socket
                        ▼
                postgresql.service (16/17; synchronous_commit=on; wal_level=replica; pgBackRest 归档)
                minio.service      (127.0.0.1:9000, S3 API; ≥4 盘时纠删码, 否则单盘模式; 数据目录在 LUKS 卷, 见 §9)
                /var/lib/cypher-edge/archive                                  (Cloudflare 导出归档, NVMe)
```

systemd 管理（不用容器：单机、单二进制、需要 fsync 语义与本地 unix socket 到 Postgres）。外部服务只剩 `api.workos.com`（JWKS + 认证）与 `api.push.apple.com`。

### 3.2 Room actor 与 DO 四条保证的对照【建议】

**(a) 单写者 / 事件串行。** 每个 room 名对应进程内一个 tokio task（actor），持有该 room 的全部内存状态（缓存的 meta、socket 表、presence、配额窗口、在途子请求）和一个有界收件箱 `mpsc::Receiver<RoomEvent>`。事件来源：HTTP 请求（携带 `oneshot` 回复通道）、socket 入站帧、socket 关闭、alarm、跨 room 消息。同一 room 同时只处理一个事件；**没有任何跨 room 的锁**。`seq`/`headSeq` 由 actor 在内存中递增并写入 Postgres，主键 `(room_id, seq)` 只是持久化，不参与排序决策（用户约束："never use DB locks for seq ordering"）。actor 在**任何**数据库错误后必须把缓存 meta 标为失效并在下一事件前重新加载（§3.9 故障转移依赖这一点）。

**(b) 存储事务性。** 一个事件内的全部写入在一个 Postgres 事务里：`BEGIN … COMMIT`，`synchronous_commit=on`。这保住 TS 代码依赖的多语句不变量：`appendRow` 的行插入 + `headSeq`（`chat-log.ts:78-88`）、`commitCheckpoint` 的删行 + 四个 meta（`chat-log.ts:143-148`）、`applyPushBatch` 的多行 upsert + 通知 outbox + `seq`（`registry-room.ts:391-393`）、PushDevice 的 `transaction(cb)`（`push-device.ts:84-95`）。checkpoint 与其它大对象先 PUT 到 MinIO（对象级原子），再在同一事务内提交指针（§3.5）。

**(c) Output gate。** 事件期间产生的所有出站帧和 HTTP 响应先缓冲，COMMIT 成功后按 TS 的顺序刷出：ChatRoom 先向**其他** ready socket 发 `row` 再向发送者发 `ack`（`chat-room.ts:465-484`）；RegistryRoom 先向**所有** ready socket（含发送者）发 `rows` 再发 `ack`（`registry-room.ts:401-408,343`）。否则"客户端收到 ack 但服务端崩溃前未落盘"会违反 `acknowledge_durable` 的假设（`chat_client.rs:422-449` 只在 ack 后退休 outbox）。COMMIT 失败 → 不发 ack，客户端按 `PUSH_ACK_DEADLINE`（`chat_client.rs:39`）重试，`batch_id` 去重保证幂等。

**(d) Alarm。** `alarms(room_id PRIMARY KEY, due_at)` 与业务写同一事务提交；进程内调度器持有最早到期的堆，actor 每次改动 alarm 后通过 channel 通知调度器；到期投递 `Alarm` 事件（room 空闲已退出则先 spawn）；`alarm()` 抛错按 DO 语义指数退避重试（上限 6 次）。启动时全表扫描重建堆（几千行，毫秒级）。`registry-room.ts:449-464` 用 `alarmScheduling` promise 链串行化 `setAlarm/deleteAlarm`——在单写者 actor 里天然串行，链本身不需要。

**(e) 非存储 await 处的交错（input gate 等价物）。** DO 允许事件在子请求 `await` 处交错，TS 代码依赖它：`notifications.ts:354-431` 的 `flush` 在每次 `await this.deviceCall(...)` 之后重读 `this.events()` 判断事件是否已被前台阅读/状态变化抢先移除（`:399-407,419`）；`push-device.ts:15-20` 刻意让 `/send` 不进 `blockConcurrencyWhile`，靠 `delivery:{id}` 状态与 lease 检查保证并发安全（`:75-79,111-114`）。Rust 对应：actor 主循环 `select!` 同时轮询收件箱和一个 `FuturesUnordered` 的**在途子请求集合**（Registry→PushDevice 消息、PushDevice→APNs HTTP）；子请求完成时作为一个事件回到 actor，执行"await 之后"的那段逻辑（重读表、更新 recipients、安排重试）。这与 DO 的交错语义一致，且不破坏单写者：子请求本身不碰数据库，只有 actor 碰。

**Hibernation 等价物。** 不需要休眠：进程常驻，attachment 就是 `SocketHandle` 上的字段。空闲 room（无 socket、无在途子请求、无 60 秒内到期的 alarm、空闲超过 `IDLE_EXIT`，建议 10 分钟）退出 task 并从 `RoomHost` 表移除，下次请求懒加载（加载 = 读 meta 一次）。presence 与配额窗口随之丢失，与 TS "resets on hibernation"（`chat-room.ts:48-49`）一致。进程重启 = 所有 socket 断开 → 客户端按既有退避重连（`device_room.rs:349` host 250 ms 起；chat/registry 250 ms→30 s，`chat_client.rs:37-38`），与今天 DO 被驱逐/部署时一致。

**Socket 所有权与背压。** 每个 WS 连接一个读任务 + 一个写任务。读任务把二进制帧作为事件 `send().await` 进 actor 的有界收件箱（满则对该连接施加 TCP 背压，不丢帧）；text `"ping"` 在读任务直接回 `"pong"` 并更新 `last_pong_at`，**不进 actor**（等价 auto-response）。actor 持有每个 socket 的有界出站 `mpsc::Sender`（建议 256 帧）做 fan-out（`try_send`）；出站队列满 = 慢消费者，actor 关闭该 socket（【建议】关闭码 1011，客户端走正常重连并按 cursor 续传）——这是与 DO 的一处**行为差异**（DO 无限缓冲），记入 §4.2。

**id 与命名。** `rooms(class, name, id_hex UNIQUE)`：`idFromName` 查表或新建（`gen_random_bytes(32)` 的 hex）；`idFromString` 按 `id_hex` 反查；导入时沿用 Cloudflare 的 id_hex（§1.4）。`ctx.id.toString()` 的所有用途读 `id_hex`。

**信任边界头。** Worker 通过 `x-cypher-auth-user` 把已验证用户传给 DO（`index.ts:98-99`；`env.ts:35-38`）。Rust 里没有进程间转发：ingress 把 `Verified` 结构体直接放进事件；入口一律丢弃入站的 `x-cypher-auth-user`/`x-cypher-room-kind`（纵深防御）。

### 3.3 并发模型与多核【建议】

- tokio 多线程运行时（worker 数 = 核数或上限 32；负载下用不满）。actor 之间无共享可变状态；唯一的全局结构是 `RoomHost` 的并发 map（`DashMap` 或分片 `RwLock`），只在 spawn/查找时触碰。
- 数据库连接池大小 ≈ 核数（起步 32，上限 64）；每个 actor 事件借一条连接跑一个事务后归还。**不部署 PgBouncer**：单机、单进程、连接数可控。
- 阻塞 I/O（文件写 + fsync）走 `spawn_blocking`，与 `ARCHITECTURE.md` §3 的 off-runtime 规则一致。
- 多节点：**当前不做**。将来的形态是 room 放置（每个 room 只在一个节点上有 actor，前端按 `rooms.node` 路由），不是共享数据库上的多写者。§3.9 的进程级 leader 锁保证在那之前同一时刻只有一个 app 实例在写。

### 3.4 Postgres 设计【建议】

**设计要求（来自 §1.4/§1.5）**：room 之间独立，零跨 room 查询；唯一的跨 room 流量是 Registry→PushDevice 的**消息**（`notifications.ts:224,343`），不是 join；`/notifications/revoke`（`index.ts:172-190`）在鉴权之前按 `bindingId` 全局查找 → `rooms.id_hex` 唯一索引。

**Schema 草图**（列名沿用 TS，便于导出/导入/校验逐字对照）：

```sql
-- 控制面：名字 ↔ 64-hex id（导入时逐字沿用 Cloudflare 的 id）
CREATE TABLE rooms (
  room_id    bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
  class      text NOT NULL CHECK (class IN ('chat','registry','device','push')),
  name       text NOT NULL,                 -- chat2/{chatId} | reg1/{org}/{user} | d2/{deviceId} | apns/{env}/{token}
  id_hex     char(64) NOT NULL UNIQUE,      -- ctx.id.toString(); /notifications/revoke 与 recipients[].id 的全局索引
  created_at timestamptz NOT NULL DEFAULT now(),
  UNIQUE (class, name)
);
CREATE TABLE alarms (room_id bigint PRIMARY KEY REFERENCES rooms, due_at bigint NOT NULL, attempts int NOT NULL DEFAULT 0);
CREATE INDEX alarms_due ON alarms (due_at);

-- ChatRoom（chat-log.ts:33-35）
CREATE TABLE chat_rows (
  room_id     bigint NOT NULL,
  seq         bigint NOT NULL,
  device      text   NOT NULL,
  batch_id    text   NOT NULL,
  bytes       bytea  NOT NULL,              -- ≤ 1 MiB（MAX_ROW_BYTES）
  received_at bigint NOT NULL,
  PRIMARY KEY (room_id, seq),
  UNIQUE (room_id, batch_id)
) PARTITION BY HASH (room_id);              -- 64 个分区：chat_rows_p00 … p63
CREATE TABLE chat_meta (room_id bigint NOT NULL, key text NOT NULL, value text NOT NULL, PRIMARY KEY (room_id, key)) WITH (fillfactor = 70);
CREATE TABLE chat_blobs (                   -- checkpoint / checkpoint-frontier / sidecar-tail / sidecar-diff
  room_id bigint NOT NULL, name text NOT NULL,
  object_key text NOT NULL, size bigint NOT NULL, sha256 bytea NOT NULL, content_type text,   -- bucket 固定为 chat-blobs
  updated_at bigint NOT NULL, PRIMARY KEY (room_id, name)
);

-- RegistryRoom（registry-room.ts:63-67）+ Notifications（notifications.ts:34-38）
CREATE TABLE reg_rows (
  room_id bigint NOT NULL, kind text NOT NULL, id text NOT NULL,
  seq bigint NOT NULL, deleted boolean NOT NULL, del_hlc text,
  fields jsonb NOT NULL, clocks jsonb NOT NULL,
  PRIMARY KEY (room_id, kind, id)
) WITH (autovacuum_vacuum_scale_factor = 0.05);
CREATE INDEX reg_rows_seq ON reg_rows (room_id, seq);
CREATE TABLE reg_meta      (room_id bigint, key text, value text NOT NULL, PRIMARY KEY (room_id, key)) WITH (fillfactor = 70);
CREATE TABLE notify_kv     (room_id bigint, key text, value jsonb NOT NULL, PRIMARY KEY (room_id, key));
CREATE TABLE notify_events (room_id bigint, id text, due bigint NOT NULL, value jsonb NOT NULL, PRIMARY KEY (room_id, id));
CREATE INDEX notify_events_due ON notify_events (room_id, due);
CREATE TABLE notify_unread (room_id bigint, chat_id text, value jsonb NOT NULL, PRIMARY KEY (room_id, chat_id));

-- DeviceRoom（device-room.ts:98-103）
CREATE TABLE device_meta     (room_id bigint, key text, value text NOT NULL, PRIMARY KEY (room_id, key));
CREATE TABLE pending_nudges  (room_id bigint, chat_id text, queued_at bigint NOT NULL, PRIMARY KEY (room_id, chat_id));
CREATE TABLE device_sidecars (room_id bigint, name text, value jsonb NOT NULL, PRIMARY KEY (room_id, name));

-- PushDevice（push-device.ts:8-11 Registration；KV 键 registration / retired:* / delivery:* / badge）
CREATE TABLE push_registrations (
  room_id bigint PRIMARY KEY, scope char(64) NOT NULL, installation_id text NOT NULL,
  epoch bigint NOT NULL, lease uuid NOT NULL, active boolean NOT NULL,
  token text NOT NULL, environment text NOT NULL CHECK (environment IN ('development','production')),
  retired text[] NOT NULL DEFAULT '{}'
);
CREATE TABLE push_retired     (room_id bigint, installation_id text, PRIMARY KEY (room_id, installation_id));
CREATE TABLE push_deliveries  (room_id bigint, message_id uuid, state text NOT NULL, at bigint NOT NULL, PRIMARY KEY (room_id, message_id));
CREATE TABLE push_badge       (room_id bigint PRIMARY KEY, badge_count bigint NOT NULL, badge_revision bigint NOT NULL, scope char(64) NOT NULL, lease uuid NOT NULL);

-- 附件 /blob/{user}/{chat}/{part}（index.ts:406-441）：MinIO bucket attachments + 指针；发布产物只在 MinIO bucket releases，无表
CREATE TABLE attachments (key text PRIMARY KEY, object_key text NOT NULL, size bigint NOT NULL, sha256 bytea NOT NULL, content_type text NOT NULL, updated_at bigint NOT NULL);
```

要点：

- **分区**：checkpoint 提交是"追加后批量 DELETE"（`chat-log.ts:143`），每次留下与被裁剪行数相同的死元组。把 `chat_rows` 按 `room_id` 哈希分 64 个分区，每个分区是独立关系，autovacuum 的多个 worker 可以并行处理，且单个分区的 vacuum 不阻塞其它分区的写。分区数在建表时固定（改动 = 重建表），64 对"几千 room、几十万行"的规模够用几年。其它表不分区。
- **autovacuum**（`postgresql.conf`）：`autovacuum_max_workers = 8`、`autovacuum_naptime = 15s`、`autovacuum_vacuum_cost_delay = 0`（NVMe，没有理由节流）、`autovacuum_vacuum_scale_factor = 0.02`、`autovacuum_vacuum_threshold = 200`；`chat_rows` 分区继承全局值即可；`chat_meta`/`reg_meta` 是高频 UPDATE 的 KV 表，`fillfactor=70` 让 HOT 更新生效。
- **`synchronous_commit = on`**（默认）：每个事件的 COMMIT 等 WAL fsync。这是"ack 即持久"的契约（`chat_client.rs:422-449` 的 outbox 语义），NVMe 上 fsync 亚毫秒，负载下无感。**不要**为吞吐改成 `off`。
- **`jsonb` 与键顺序**：`reg_rows.fields/clocks` 用 `jsonb`，键顺序不保留。客户端用 serde/Codable 解码，不依赖顺序（`crates/sync/src/registry.rs`、`apps/ios/Cypher/Sync/RegistryClient.swift:483-491`）；§12.4 的差分比较对 JSON 做结构比较而非字节比较。
- **hot state 缓存**：actor 启动时读一次 meta（`owner`、`headSeq`、`seqFloor`、`checkpoint*`、`seq`、`gcFloor`…），之后内存为准、写穿到表。`pushOutcomes`、`backupDirty` 按 §14 改为采样/去重写。
- **池与连接**：`max_connections = 100`；应用角色非超级用户，仅拥有上述表；`scram-sha-256`；仅监听 unix socket。
- **容量**：§1.5 的 profile（6.8 MB/重度用户文档状态、13 MB/月增长）即使 UX-1 把行数放大 7 倍，100 个用户一年也在几 GB 量级；`shared_buffers` 8 GB 足够，不要按 500 GB 内存去配（大页与巨大 shared_buffers 只会拖慢 checkpoint 与重启）。

### 3.5 大对象与对象存储（MinIO）【建议】

- **原则**：Postgres 只存 `object_key` + sha256 + 大小；字节放 MinIO。理由：checkpoint 上限 16 MiB（`chat-room.ts:45`）、附件 1 MiB（`index.ts:70`）、sidecar 4 MiB（`chat-room.ts:42`），TOAST 能放但备份/复制/vacuum 都要多搬这些字节。`blobs.ts:1-4` 的 1.5 MB 分块只是 DO 的 2 MB 值上限的绕法，对象存储下不需要。
- **部署**：同机 `minio.service`，只监听 `127.0.0.1:9000`（应用与 `mc` 走 loopback，不暴露公网，不需要 TLS）；数据目录在 LUKS 卷上。**盘数决定模式**：≥4 块 NVMe 用纠删码（`EC:2`，容忍单盘故障）；否则单盘模式，无盘级冗余，靠 §8.2 的离机复制。ZFS 与 MinIO 纠删码二选一，不叠加（§11）。
- **bucket**：`chat-blobs`（checkpoint / frontier / sidecar-tail / sidecar-diff，key `{room_id}/{name}-{seq}-{sha256[..16]}`）、`attachments`（key `{user}/{chat}/{part}`，开启**版本化**以获得误删恢复）、`releases`（原 `cypher-releases` 的 key 原样）、`archive`（Cloudflare 导出与遗留 SessionRoom 冷归档）。应用使用一个最小权限的 access key，只对这四个 bucket 有读写；`mc admin` 凭据不进应用。
- **原子性**：先 `PUT` 对象（S3 PUT 对象级原子，成功即持久）→ 在业务事务里更新 `chat_blobs` / `attachments` 指针 → COMMIT → 旧对象由后台 sweep 删除（任何时刻数据库指向的对象一定存在；崩溃只会留下孤儿对象，sweep 按"指针表中不存在且创建时间 > 1 小时"清理）。
- **checkpoint GET**（`chat-room.ts:136-165`）：Rust 服务端按指针从 MinIO 做 S3 range GET 并流式回给客户端，`bytes=N-` Range 语义、`x-chat2-checkpoint-seq` 头逐字保留。**不用预签名 URL 重定向**：那会改变客户端的取回契约（协议冻结，§4）；切换后可作为优化另议。
- **`cypher-blobs` 的附件**（`/blob/{user}/{chat}/{part}`）：不经 actor（Worker 今天也直接写 R2），`PUT` 到 `attachments` bucket + `attachments` 表；`GET` 回放 `content-type`（`index.ts:433` `writeHttpMetadata`）与 `etag`（客户端不解释，`doc_host.rs:2358-2364` 只读 body；实现为 sha256）、`cache-control: private, max-age=300`（`:435`）。
- **各 room 的夜间 R2 备份**（`chat-room.ts:555-581`、`registry-room.ts:494-501`）：写到 `archive` bucket 的 `backup/…` 前缀；它们在 Postgres + pgBackRest 之下是冗余的，【建议】保留 alarm 逻辑（差分测试要覆盖 alarm 路径）但输出降级为可关闭（§11）。
- **`releases`**：只读服务由 Rust 原路由提供（`index.ts:138-165`，头部逐字：按扩展名的 content-type、`content-length`、`cache-control` 可变/不可变分流、`access-control-allow-origin: *`；`etag` 改为 sha256），服务端从 MinIO 流式读出。写入口：`release.py` 的 `R2` 类改为通用 S3 客户端（boto3 或 `mc`），endpoint/凭据指向 MinIO（CI 经 SSH 隧道或 WireGuard 到 loopback，§11）；"同名不同摘要即拒绝"由 `release.py` 现有逻辑在 `get/digest` 上继续成立；工作流的 `CLOUDFLARE_*` 变量换成 `CYPHER_S3_*`（`linux.yml:147-148`、`macos.yml:150-151`）。
- **带宽风险**：MinIO 不改变这一点——`/releases/*` 无鉴权，产物几十到上百 MB，仍走同一上行链路。`release.py:478-484` 已把产物同步发布到 GitHub Release；【建议】大文件的 GET 改为 302 到 GitHub Release 资产，manifest/latest/stem/sha256 仍自己服务。`install.sh` 用 curl 下载【假设：需确认其 curl 参数允许跟随 https 重定向】；`crates/update` 用 reqwest 默认跟随重定向【假设：需确认 `download_release_file` 未禁用】。§11 开放问题，不阻塞切换。
- **可替换性**：应用只依赖 S3 API（`aws-sdk-s3` 或 `object_store` crate）。MinIO 的开源发行在 2025 年调整过许可与社区版功能范围，实施前核对当前条款是否满足需要（§11）；不满足则 Garage / SeaweedFS 同为 S3 API，配置级切换。

### 3.6 入口、TLS、域名、DNS【建议】

- Caddy 作入口：ACME 自动续期、HTTP→HTTPS、WS 透传、请求体上限、每 IP 连接数与速率限制（`/auth/*`、`/notifications/revoke`、`/releases/*` 单独更严）、访问日志**删除或哈希 `token` 查询参数**（今天 Cloudflare 侧为此关闭了调用日志与 trace：`wrangler.jsonc:102-106`、`wrangler.development.jsonc:38`）。Rust 进程只监听 127.0.0.1，从 `X-Forwarded-For` 取客户端 IP（替代 `cf-connecting-ip`，`auth-routes.ts:151`）。
- 主机名不变（§1.2 第 1、2 条）。
- **DNS 切换与 zone 迁出解耦**：Worker custom domain 要求 zone 在 Cloudflare（`wrangler.jsonc:18-24`）。窗口内只做一件事：删除 Worker 的 custom domain 路由，在 Cloudflare DNS 上新建 `edge` 的 A/AAAA 记录，**DNS only**（灰云），TTL 60。这一步可在一分钟内回退。zone 迁到别的 DNS 托管（NS 变更）作为窗口后独立步骤；landing 与 `www` 301 同期迁到同一台机器的 Caddy 或其它静态托管——是否属于"完全退役"范围见 §11。
- **证书预签发**：切换前用 DNS-01（Cloudflare API token 限定该 zone）为 `edge.letscypher.app` 签出证书并交给 Caddy 静态使用，避免切换后 HTTP-01 生效前的 TLS 失败窗口；zone 迁出后改回 Caddy 自动 HTTP-01。
- 服务器需要稳定的 IPv4；IPv6 可选但若提供必须真实可达（客户端 happy-eyeballs 会交错尝试两族：`crates/sync/src/dial.rs:1-16`）。

### 3.7 DDoS / 滥用暴露与替代【建议，含诚实边界】

Cloudflare 之前吸收：L3/L4 洪水、L7 洪水、TLS 握手放大、机器人。单机替代：

| 面 | 措施 |
|---|---|
| 无鉴权路由 `/health` `/install.sh` `/releases/*` `/auth/*` `/notifications/revoke` | Caddy 每 IP 速率限制；`/auth/exchange|refresh|verify-email` 消耗 WorkOS 配额，限制最严（如每 IP 10 次/分）；`/releases/*` 大文件 302 到 GitHub（§3.5） |
| 有鉴权路由 | JWT 验签（RS256/ES256）每次 <0.1 ms，先验签再进 actor；未验签流量不能触达 Postgres |
| 连接层 | nftables SYN 速率与 conntrack 上限、Caddy 每 IP 连接数上限、空闲 WS 由客户端 15 秒 ping 维持 |
| 应用层配额 | ChatRoom 每设备 300 次/60 秒、8 MiB（`chat-room.ts:50-52`）原样保留；Registry 每批 500 ops（`registry-room.ts:32`）；有界收件箱/出站队列（§3.2） |
| 体积型攻击 | **无法在单机吸收**超过上行带宽的流量。诚实结论：主机名未公开列出、用户量小，接受该风险；若将来需要，可在不回到 Cloudflare 的前提下前置任一 anycast/清洗服务。托管商自带的 L3/L4 清洗能力是 §11 的选址问题 |

### 3.8 APNs 发送【建议】

`apns.ts:63-88` 用全局 `fetch` 对 `api.push.apple.com` 发 POST；Apple 只接受 HTTP/2。Rust：`reqwest` 客户端 `http2_prior_knowledge()` + rustls，长连接复用；ES256 provider token 用 `jsonwebtoken`（`EncodingKey::from_ec_pem`），50 分钟缓存（`apns.ts:41`）。请求头（`apns-topic`、`apns-push-type`、`apns-priority` 5/10、`apns-id`、`apns-collapse-id` = sha256 前 48 位十六进制、`apns-expiration`）与 payload 形状（`apns.ts:74-93`）逐字段保留；返回值三态 `sent|invalid|retry` 与判定（410 或 `BadDeviceToken|DeviceTokenNotForTopic|Unregistered` → invalid，`:98-100`）保留；诊断日志沿用 `SAFE_APNS_REASONS` 白名单（`apns.ts:6-14,16-30`）。`APNsSender` 从 service binding 变为 PushDeviceActor 的在途子请求（§3.2(e)）。

### 3.9 高可用设计（选 Postgres 的理由；可后续部署）【建议】

分三个阶段，切换时只需 H0：

| 阶段 | 内容 | 目标 |
|---|---|---|
| **H0（切换时）** | 单节点 Postgres；`wal_level=replica`、`archive_mode=on`、pgBackRest 持续 WAL 归档到**离机**仓库（`archive_timeout=60s`）；每日增量 + 每周全量；PITR 演练每月一次（§8.2） | RPO ≤ 1 分钟（归档超时）；RTO = 在别处恢复的小时级 |
| **H1（切换后，独立变更）** | 第二台节点，流复制热备；故障转移自动化用 **pg_auto_failover**（两节点 + 一个小监控 VM，比 Patroni + etcd 三节点更适合两台机器的规模；已有 etcd 时选 Patroni）；应用连接串 `host=a,b target_session_attrs=read-write`；是否 `synchronous_standby_names`（同步复制，每次 COMMIT 多一个同机房 RTT）按实测决定【假设】 | 数据库单机故障分钟级自动切换 |
| **H2（可选）** | 应用层冷备：同一 Rust 二进制部署在节点 2，平时不启动；**进程级 leader 锁**（应用启动时 `pg_try_advisory_lock(<固定键>)` 并终身持有；拿不到就拒绝服务）保证任一时刻只有一个实例在写——这是**唯一**允许使用数据库锁的地方，与 `seq` 排序无关。故障转移 = 启动节点 2 的应用 + 切 DNS/VIP；socket 全部重连，actor 从表重建 | 整机故障分钟级人工/半自动切换 |

诚实边界：H1 只解决数据库可用性，不解决应用可用性；H2 之前，应用进程崩溃由 systemd 秒级拉起，整机故障仍是人工恢复。Postgres 热备也不解决 §1.5 的区域延迟问题——那是 room 放置的事。

### 3.10 开发环境【建议】

托管开发 Worker 与 `DevelopmentGuard` 预算门已于 2026-09-22 提前删除（`docs/local-edge.md`），迁移无需再处理它。本地开发直接跑 Rust 服务端（`AUTH_MODE=dev`，语义同 `auth.ts:44-51`；`scripts/e2e-smoke.sh:73-86` 改为启动 Rust 进程替代 `wrangler dev`）；云端联调用同一台服务器上的第二个实例（`edge-dev.letscypher.app`，独立 Postgres 数据库与数据目录、独立 systemd 单元，`AUTH_MODE=dev-locked`，`auth.ts:40-43`）。开发预览 relay 的四道 dev 门在 §13 UX-2 统一处理。

---

## 4. 协议兼容契约

### 4.1 路由清单与处置【现状 → 建议】

处置：**保留** = 字节级一致（状态码、头、体、帧）；**变更** = 列明差异；**删除** = 返回 404/410。

| 路由（`index.ts` 行） | 现状实现 | 已知调用方 | 处置 |
|---|---|---|---|
| `GET /health`（:123） | `{ok, auth}` | `auth.rs:375` 健康探测；`e2e-smoke.sh:73` | 保留 |
| `GET|HEAD /install.sh`（:130） | 文本 + `cache-control: public, max-age=0, must-revalidate` | `README.md`、`docs/linux-setup.md` | 保留 |
| `GET|HEAD /releases/*`（:138-165） | R2 直出，头部见 §3.5 | `crates/update`、`pi_runtime.rs`、`install.sh`、landing、`release.py check-deploy` | 保留；**变更**：`etag` 值不再是 R2 的（无客户端解释）；大文件可选 302（§3.5，需决定） |
| `POST /auth/exchange` `POST /auth/verify-email` `POST /auth/refresh` `GET|POST /auth/orgs` `GET /auth/cli/callback` `GET /auth/ios/callback`（`auth-routes.ts`） | WorkOS 转发；错误信封 `{error, code, retryable}`（`auth-routes.ts:46-62`） | `auth.rs:807,882,1000,649-667`；iOS `Auth/AuthClient.swift:125-148`、`SignInView.swift:29` | 保留；**变更**：日志中的 `cf-connecting-ip`（`auth-routes.ts:151`）改读 `X-Forwarded-For` |
| `POST /notifications/revoke`（:172） | 无鉴权撤销能力，转 PushDevice `/unregister` | iOS `NotificationController.swift:303` | 保留；`bindingId` 走 `rooms.id_hex` 全局索引 |
| `GET /session/:id/ws` `GET /tail/:id` `GET /stats/:id` `GET /snapshot/:id` `POST /append/:id`（:193-226） | SessionRoom `s2/` | 无（`ARCHITECTURE.md` §1；grep 无命中） | **删除**（410 Gone）；数据冷归档 |
| `GET|POST /diff/:chatId`（:217） | SessionRoom `s2/` 的 blob 槽（`session-room.ts:311-326`） | **桌面仍 POST**：`diff_sync.rs:625`；GET 无读者 | **变更**：兼容路由，POST 落到对应 chat2 room 的 `sidecar-diff`（等价 `PUT /chat2/:id/diff`，`chat-room.ts:272-282`，content-type `application/json`），响应 `{ok:true}`；GET 从同一槽返回。客户端后续改为 `PUT /chat2/{id}/diff`（非切换前提） |
| `/workspace/:orgId/*`（:268-327） | SessionRoom `ws4/` | 无 | **删除**（410） |
| `GET /chat2/:id/ws`（:233-245） | 二进制帧协议 | `doc_host.rs:1307`；iOS `AppConfig.swift:186` | 保留（帧、`state` 载荷为 frontier 字节、`hello_first`、关闭码 1003/1009/4410） |
| `GET /chat2/:id/checkpoint`（`chat-room.ts:136-165`） | 200/206/416，`accept-ranges`、`content-range`、`x-chat2-checkpoint-seq`，只接受 `bytes=N-`（`:605-610`） | `chat2_host.rs:177-274`（续传 + seq 校验 `:233-247`）；iOS `AppConfig.swift:196` | 保留 |
| `POST /chat2/:id/checkpoint?seqCovered=`（`:119-135`） | `x-chat2-frontier` base64；floor 单调守卫 409 `floor_regression|ahead_of_head`（`chat-log.ts:138-139`）；16 MiB 上限 | `doc_host.rs:1576,1874` | 保留 |
| `GET /chat2/:id/rows?after&device&excludeOwn`（`:166-231`） | 长度前缀帧流，4 MiB 截断时省略 `rowsDone`（`:198-213`） | `chat2_host.rs:312-340`；iOS `:206` | 保留 |
| `POST /chat2/:id/rows?batchId&device`（`:232-271`） | 重复 → `{batchId, seq, dup:true}` 先于配额（`:249-251`）；`quota` 429 | `chat2_host.rs:349-375`；iOS `:220` | 保留 |
| `GET|PUT /chat2/:id/tail` `GET|PUT /chat2/:id/diff`（`:272-296`） | 原样存取，`content-type` 记在 meta，4 MiB 上限 | tail：`doc_host.rs:1812`；diff：暂无（见上） | 保留 |
| `GET /chat2/:id/stats` `POST /chat2/:id/reset`（`:297-335`） | 运维面 | 运维脚本 | 保留 |
| `GET /registry/:org/ws?device`（`registry-room.ts:165`） | JSON 文本帧：hello/state/push/ack/rows/presence/probe（`docs/registry-sync.md` 协议表） | `workspace_host.rs:457`；iOS `AppConfig.swift:175` | 保留（rows 先于 ack、`full` 判定 `registry-room.ts:319`、关闭码 1002/1003/1009/4410） |
| `GET /registry/:org/rows?since&device&beat=1`（`:194-221`） | 全量/增量 + HTTP presence 心跳 | `workspace_host.rs:159-180`；iOS `:235` | 保留 |
| `POST /registry/:org/push?device`（`:222-238`） | 与 WS 同一 `applyPushBatch` | `workspace_host.rs`；iOS `:252` | 保留 |
| `GET /registry/:org/stats` `POST /registry/:org/reset` | 运维面 | — | 保留 |
| `/registry/:org/notifications/{settings,activity,event,register,unregister}`（`index.ts:333-336`） | `notifications.ts:97-252`；`scope` 即 room id | 桌面 `auth.rs:1156,1200`；iOS `NotificationController.swift:145` | 保留；**id 沿用**（§1.4） |
| `GET /device/:id/ws?role&connId`（`:371-386`） | 字节管道；host 唯一并 4409 抢占（`device-room.ts:166`）；`" relay"` 控制帧（`:80`）；nudge 回放 | `device_room.rs:168-179`；iOS `DeviceRelayClient.swift:54-63` | 保留（含 75 秒存活窗、`host_offline|host_closed|client_gone|client_closed`） |
| `GET|POST /device/:id/sidecar/:name`（:387） | JSON 槽 | 桌面 repos 快照 | 保留 |
| `GET /device/:id/status`（:390） | `{hostConnected, hostSockets}` | `workspace_host.rs:1735`；iOS `:279` | 保留 |
| `POST /device/:id/nudge`（:396） | 在线投递或入队（256 上限） | `doc_host.rs:2243`；iOS `:290` | 保留 |
| `PUT|GET|HEAD /blob/:chatId/:partId`（:406-441） | key `blob/{user}/{chat}/{part}`；PUT 1 MiB 上限；GET `cache-control: private, max-age=300` | `doc_host.rs:2288,2361` | 保留 |
| `PUT /attachments/*`（:445） | 应答并丢弃（≤0.1.62 客户端） | 老客户端 | 保留（零成本） |
| 其余 | 404 `{error:"not_found"}` | — | 保留 |

### 4.2 客户端依赖的线上行为清单

| 行为 | 服务端定义 | 客户端依赖点 | 处置 |
|---|---|---|---|
| batchId 去重：重放返回原 seq 且 `dup:true`，不写入 | `chat-log.ts:61-63,77-78`、`chat-room.ts:444-448` | `chat_client.rs:422-449` `acknowledge_durable`；outbox 重放（`store.rs:42-49`）；iOS `ChatRoomClient.swift:75,274,535` | 保留 |
| 去重先于配额判定 | `chat-room.ts:444-452,249-251` | `docs/chat2-sync.md` "Reconnect replay is head-serialized" | 保留 |
| 行上限 1 MiB；WS 帧上限 `MAX_ROW_BYTES+8192` | `chat-log.ts:18`、`chat-room.ts:40,344-347` | `chat_client.rs:59` `MAX_PUSH_BYTES = 1 MiB − 4096`（`:53-58` 注释：运行时在 1 MiB 关闭帧） | 保留；Rust WS `max_message_size` = `MAX_FRAME_BYTES`，超限关闭 1009【假设：Cloudflare 运行时在 1 MiB 处的关闭码为 1009；客户端对任何关闭都重连，差异无害】 |
| 错误帧携带 `batchId`；`too_large|empty|bad_push` 永久、`quota` 临时 | `chat-room.ts:430-462` | `chat_client.rs:1664-1690` | 保留 |
| `state{headSeq, seqFloor, checkpointSeq, checkpointSize, rowCount, rowBytes}` + frontier 载荷 | `chat-room.ts:388-410` | `chat_client.rs:187-214` `plan_catch_up`（以 `checkpoint_size` 判存在）、`chat2_host.rs:146-171` `contains_frontier`；iOS `ChatRoomClient.swift:210,598` | 保留 |
| 服务端 `headSeq < cursor` = 已重置 | 隐含于 hello 应答 | `chat_client.rs:1163-1176` `ServerReset` → host 重新 checkpoint；iOS `ChatRoomClient.swift:582-594` | 保留（也是 §7 回滚的自愈路径） |
| Range 续传 + `x-chat2-checkpoint-seq` 校验 | `chat-room.ts:143-163` | `chat2_host.rs:233-247` | 保留 |
| `rows` GET 4 MiB 截断省略 `rowsDone` | `chat-room.ts:198-213` | `chat_client.rs` http_sync 按连续性应用 | 保留 |
| 两秒合并窗口 | 纯客户端（`chat_client.rs:730,752`）；iOS 客户端无此窗口 | — | 无服务端依赖；§13 UX-1 |
| tail/diff sidecar 原样回放 + content-type | `chat-room.ts:272-296` | `doc_host.rs:1812`；iOS 读 tail | 保留 |
| `x-cypher-auth-user` 由服务端内部写入，客户端值被覆盖 | `index.ts:98-99` | 无（信任边界） | Rust 无转发；入口丢弃 |
| Registry：rows 广播先于 ack；`full` 判定；重置后客户端重播种 | `registry-room.ts:319,401-408,343` | `registry.rs:845`、`crates/doc/src/registry.rs:435-494`；iOS `RegistryClient.swift:389-400` | 保留 |
| Registry 墓碑 30 天 GC 与 `gcFloor` | `registry-room.ts:29,476-491` | `registry.rs` 处理 `full` | 保留 |
| DeviceRoom 角色管道、`from/to` 标记、`" relay"`、4409、nudge 队列 | `device-room.ts:75-87,161-181,253-306` | `device_room.rs:33-46,172-179`；iOS `DeviceRelayClient.swift:5-10,32,211` | 保留 |
| text `ping`→`pong` 不惊动 room | 三个 room 的 `setWebSocketAutoResponse` | 三个客户端 pump 的 `SILENCE_LEASE` | 保留（连接任务层实现） |
| 通知 `scope`/`bindingId`/`lease` 语义 | `notifications.ts:50,223-236`、`push-device.ts` | iOS `NotificationController.swift` | 保留 + id 沿用 |
| 预览帧类型 `0x20–0x26` 保留段 | `development-preview.ts`（仅开发） | `chat_client.rs:1581` 忽略 | 生产不出现；保留空间（§13 UX-2 启用） |
| **新差异**：慢消费者出站队列满 | DO 无限缓冲 | 客户端对任何关闭都按 cursor 重连 | Rust 关闭 1011（§3.2） |

### 4.3 客户端代码

**切换前提：`crates/sync`、`crates/engine`、`crates/rpc`、iOS 一行都不改。** 这由协议冻结（§4.1/§4.2）与"主机名不变"共同保证；§12 证明冻结成立。

**可选跟进（切换后、独立发版）**：
1. `crates/engine/src/diff_sync.rs:625` 改为 `PUT /chat2/{chatId}/diff`，随后删除兼容路由。
2. 注释与文档中的"Cloudflare/DO"措辞（`doc_host.rs`、`chat_client.rs`、`device_room.rs:6` 等）—— 纯文档。
3. §13 UX-1/UX-2 与 §14 的客户端改动。

---

## 5. 数据迁移

### 5.1 需要迁出的数据

| 数据 | 位置 | 体量估计【假设】 | 目标 |
|---|---|---|---|
| ChatRoom（`chat2/*`）：rows、meta、blobs（checkpoint、frontier、sidecar-tail、sidecar-diff）、alarm | DO namespace `ChatRoom` | 每 room ≤ 512 KiB 行 + ≤16 MiB checkpoint（阈值策略 `doc_host.rs:1829-1834`）；§1.5 profile 合计 6.8 MB/重度用户 | `chat_rows`/`chat_meta`/`chat_blobs` + 文件 |
| RegistryRoom：rows、meta、notify_kv、notify_events、notify_unread、alarm | `RegistryRoom` | 每用户数 MB 以内（`docs/registry-sync.md` "Why"） | `reg_*`/`notify_*`，**保留 id** |
| DeviceRoom：meta、pending_nudges、blobs（sidecar） | `DeviceRoom` | KB 级 | `device_*`/`pending_nudges` |
| PushDevice：KV `registration`、`retired:*`、`delivery:*`、`badge` | `PushDevice` | KB 级 | `push_*`，**保留 id** |
| SessionRoom（`s2/*`、`ws4/*`、更早） | `SessionRoom` | 可能有 MB 级 whale | 冷归档，不导入线上 |
| R2 `cypher-blobs` | `blob/{user}/{chat}/{part}`、`backup/chat2/{id}/latest.json`、`backup/registry/{id}/latest.json`、遗留 `backup/{chatId}/latest.loro` | 附件极少（§1.5：340 KB）；备份数千小文件 | `attachments/` + 表；备份进 `archive/` |
| R2 `cypher-releases` | 产物、`.sha256`、manifests、`runtimes/pi/*` | 数 GB | `releases/` |
| Secrets | `WORKOS_API_KEY`、`APNS_PRIVATE_KEY`（Worker secret，**不可读回**） | — | 从原始来源重新配置（§11） |
| Vars | `WORKOS_CLIENT_ID`、`APNS_KEY_ID`、`APNS_TEAM_ID`、`NOTIFICATIONS_ENABLED`（`wrangler.jsonc:89-101`） | — | 环境文件 |

### 5.2 DO 导出机制【建议】

DO 存储没有官方 dump API。做法：

1. **临时导出部署**（Worker 一次性版本，与冻结开关一起）：
   - 每个 room 类增加内部路径 `/__export`：返回 `sqlite_master` 的 `sql`、每张表全部行（BLOB 以 base64 编码）、`getAlarm()`、`ctx.id.toString()`、若运行时提供则附 `ctx.id.name`【假设：`ctx.id.name` 对按名创建的对象可用，导出脚本不依赖它，只在存在时用于交叉校验】；PushDevice 额外输出 `storage.list()` 全部 KV。
   - Worker 增加 `GET /__export/{class}?name=…` 与 `?id=…`，要求独立 secret `EXPORT_TOKEN`；`GET /__export/{class}/id-of?name=…` 只返回 id。
   - 增加冻结开关 `FREEZE_WRITES=1`：非导出路径的 WS upgrade、POST、PUT 返回 503 + `retry-after: 120`，GET 照常。客户端按既有退避重试，不丢数据（outbox 已持久，`store.rs:42-49`）。
2. **枚举**：Cloudflare REST API 列出每个 namespace 的对象 id 及 `hasStoredData`。这是完整性的**基准集合**。
3. **名称还原**（room 不存名字，§1.4）：RegistryRoom 候选名来自 WorkOS 用户列表 × 组织成员关系（`workos.ts:346-360` 同一 API 家族）；ChatRoom 的 chatId 来自每个 RegistryRoom 导出的 `chats` 行（含墓碑）；DeviceRoom 来自 `devices` 行；PushDevice 由自身 registration 重建，另由 RegistryRoom `recipients[].id` 直接按 id 导出。**无法匹配到名字但有数据的对象**：按 id 导出到 `archive/orphans/`，不导入线上；数量写进报告。
4. **成本**：几千个对象，几分钟到几十分钟，数美分。

### 5.3 R2 → MinIO 导出【建议】

为两个 R2 bucket 开启 S3 兼容 API 凭证，用 `rclone sync` **S3 到 S3** 直接同步到 MinIO：`cypher-releases` → bucket `releases`（key 原样）；`cypher-blobs` 的 `blob/{user}/{chat}/{part}` → bucket `attachments`（key 去掉 `blob/` 前缀），`backup/…` 与遗留 `backup/{chatId}/latest.loro` → bucket `archive`。`rclone` 在 S3→S3 时保留 `Content-Type` 元数据；另用 `rclone lsjson --metadata` 存一份清单，`rclone check` 做 md5 一致性核对。发布产物可在窗口前完成并在窗口内增量同步；附件/备份桶在冻结后同步一次。

### 5.4 导入 Postgres【建议】

导入工具 `crates/edge/src/bin/import-cloudflare-export.rs`（与服务端同一 schema 定义）：

1. **登记 room**：对每个导出对象 `INSERT INTO rooms(class, name, id_hex)`，**`id_hex` 逐字沿用导出值**；orphans 不登记。得到 `room_id`。
2. **装载**：每张表用 `COPY … FROM STDIN (FORMAT binary)` 批量写入（`chat_rows` 按 `room_id` 自动落到分区）；base64 解码 BLOB；`meta` 原样成对写入；`alarm` 非空则写 `alarms`。PushDevice 的 KV 键映射：`registration` → `push_registrations`（`lease` 解析为 uuid、`retired[]`）、`retired:*` → `push_retired`、`delivery:*` → `push_deliveries`、`badge` → `push_badge`。
3. **大对象**：ChatRoom 的 `blobs` 表按 `name` 把分块（`blobs.ts:20-27`）按 `idx` 拼接成一个对象，`PUT` 到 bucket `chat-blobs`（key 按 §3.5），指针入 `chat_blobs`（含 sha256、大小、`sidecar-*-type` meta 转为 `content_type`）；DeviceRoom `sidecar:*` 直接转 `device_sidecars` jsonb。附件已由 §5.3 落在 bucket `attachments`，此处只按清单写 `attachments` 表（content-type 来自 `rclone lsjson`）；releases 无需导入步骤。
4. **幂等**：以 `id_hex` 为键，重复导入先删后写（单事务 per room），便于演练反复执行。
5. **导入后**：`ANALYZE`；对每个分区 `VACUUM`；然后 pgBackRest 全量备份一次（作为已知良好状态，§6 Gate D）。

### 5.5 完整性校验【建议】

全部用 SQL 在 Postgres 上跑，输出报告；任何一项失败即 Gate D 失败。

| 对象 | 校验 |
|---|---|
| 全局 | 基准集合（API 列表中 `hasStoredData=true`）⊆ 导出清单；导出清单 = `rooms` 行数 + orphans；每个对象导出 JSON 的 sha256 与从新服务端 `/__export` 同格式重新导出一致；`rooms.id_hex` 全部匹配 `^[a-f0-9]{64}$` |
| ChatRoom | `chat_meta.headSeq == COALESCE(MAX(chat_rows.seq), headSeq)`；`(seqFloor, headSeq]` 内 `COUNT(*) == MAX(seq) - MIN(seq) + 1`（无洞）；`rowCount/rowBytes` 与 `logStats` 重算一致；checkpoint 文件长度 == `checkpointSize`、sha256 与导出时一致；`checkpointSize>0 ⇔ frontier 指针存在`；`owner` 非空 |
| RegistryRoom | `reg_meta.seq >= MAX(reg_rows.seq)`；`gcFloor <= seq`；行数与墓碑数与导出一致；`notify_kv.recipients[*].id` 全部能在 `rooms(class='push').id_hex` 中找到 |
| DeviceRoom | `owner`、`pending_nudges` 行数 |
| PushDevice | `push_registrations.scope` 等于某个 `rooms(class='registry').id_hex` |
| 对象存储（MinIO） | 每个 bucket 对象数与 R2 一致；每对象 md5 与 R2 一致（`rclone check`）；`chat_blobs`/`attachments` 每个指针在 MinIO 中存在且大小/sha256 匹配；`releases/{linux,macos}/manifest.json` 列出的产物与 `.sha256` 全部存在且摘要匹配（复用 `release.py check-deploy` 的逻辑指向新服务端） |
| 端到端 | 新服务端对每个导入 room 的 `/chat2/:id/stats`、`/registry/:org/stats`、`/device/:id/status` 与 Cloudflare 冻结时刻的同名接口输出逐字段一致（`connectedSockets`、`presence` 除外） |

### 5.6 顺序

1. 窗口前：releases 全量（可反复增量）；导出脚本对生产做**只读演练**（不冻结），导入到预发 Postgres 数据库，跑 §5.5 全套与 §6 步骤 10 的客户端验证。
2. 窗口内：冻结 → PushDevice → RegistryRoom（提供名字）→ DeviceRoom → ChatRoom → 附件桶增量 → 校验 → 全量备份。
3. 窗口后：SessionRoom 冷归档（只读，可慢慢做，不影响服务）。

---

## 6. 切换 Runbook

窗口建议 4 小时（预期实际 1–2 小时），选用户活跃度最低时段。每个 Gate 未通过则执行 §7 对应回滚，不带着问题往下走。

**T-14 天起（窗口前，全部可演练）**
1. **Gate 0（§12.5）**：差分回放 0 差异、golden 测试全绿、Postgres 崩溃/恢复测试通过——A 特有的前置门，不通过不得进入下面任何步骤。
2. 预发实例上线：`edge-staging` 数据库 + 只读演练导入；两台测试机（桌面 + iOS 开发构建）通过 `/etc/hosts` 把 `edge.letscypher.app` 指向服务器 IP，用生产 URL 与生产 WorkOS 完成登录、同步、跨设备 RPC、推送注册。
3. 证书预签发（§3.6）并装入 Caddy；`curl -v https://<ip> -H 'Host: edge.letscypher.app'` 验证链。
4. pgBackRest 备份 + PITR 恢复演练一次通过（§8.2），监控告警可达（§8.3）。
5. 部署"导出 + 冻结"版本 Worker 到生产，`FREEZE_WRITES` 未设置、`EXPORT_TOKEN` 已设置；用 `/__export` 只读抽样 10 个 room 与 §5.5 对照。**Gate A**：抽样全部一致。
6. 通告用户：窗口时间、期间"停止使用并等待"、结束通告方式。

**T-0 窗口**
7. **排空客户端 outbox**：请每位用户在所有设备执行 `cypher sync`，确认 `pendingPushes` 为 0（`crates/engine/src/rpc.rs:2107-2124` 输出该字段）；headless 设备由用户 `cypher status`/`cypher sync` 确认。**Gate B**：所有已知设备报 0（未响应的设备记录在案——其未 ack 的 batch 会在切换后由 batchId 去重安全重放）。
8. **冻结**：设置 `FREEZE_WRITES=1` 重新部署。确认 `POST /chat2/x/rows` 返回 503、`GET /health` 正常。采集所有 room 的 `/stats` 快照（脚本）。
9. **导出**（§5.2-5.3）：运行导出脚本；输出清单与 sha256。**Gate C**：基准集合覆盖 100%，脚本零错误；orphans 数量在预期内。
10. **导入**到生产 Postgres（服务此时对外未暴露，Caddy 仅允许维护者 IP）；运行 §5.5 全套；pgBackRest 全量备份。**Gate D**：全部通过；`/stats` 对照与冻结快照逐字段一致。
11. **/etc/hosts 验证**：两台测试机对新服务端完成：登录刷新、打开已有 chat 并收到 `CaughtUp`、跨设备发送一条命令由 host 执行并回传、iOS 前台活动上报得到相同 `scope`、一次 APNs 沙盒/生产推送到达。**Gate E**：全部通过。
12. **DNS 切换**：Cloudflare 上删除 Worker 的 custom domain 路由；新建 `edge` A/AAAA（DNS only，TTL 60）指向服务器；Caddy 放开公网访问。观察：Caddy 访问日志出现真实用户 IP 的 `/health`、`/registry/*/ws` 101。
13. **解冻**：新服务端无冻结概念；Cloudflare 侧保持 `FREEZE_WRITES=1`（仍解析到旧地址的客户端只会收到 503 并重试，不会分叉写入）。**Gate F**：15 分钟内活跃设备数（WS 连接数指标）达到冻结前水平的大部分；无 5xx 尖峰；`server_resets`/`floor_regression` 计数为 0（表示 seq 未回退）；Postgres 无锁等待、事务 p99 < 10 ms【假设：真实机器上重测阈值】。
14. 通告用户恢复使用；持续观察 24 小时。

**T+1 天 … T+14 天**
15. 保持 Cloudflare Worker 冻结部署与数据不动（回滚窗口，§7）。
16. 切换 CI：`deploy.yml` 改为构建 + SSH 部署到服务器（§8.4）；`release.py` 换 `HttpStore`；用一次真实发版验证 `install.sh` 全流程。
17. **T+14 天**：确认无回滚需要 → 删除 Worker、DO namespaces、R2 bucket、API token；zone 迁出与 landing/www 迁移作为独立变更（§3.6）；`edge/` TS 目录归档（§11）。

## 7. 回滚

| 时点 | 可逆性 | 操作 |
|---|---|---|
| Gate 0–D 之前/失败 | 完全可逆，用户无感 | 撤销 `FREEZE_WRITES`（重新部署未冻结版本）。导出为只读，无副作用；生产 Postgres 数据库整体 `DROP` 重来 |
| Gate E 失败（/etc/hosts 验证） | 完全可逆 | 同上 |
| DNS 切换后、解冻前 | 可逆，分钟级 | 删除 A 记录，恢复 Worker custom domain（证书重新签发需数分钟，期间 TLS 失败、客户端退避重试）；撤销冻结 |
| **解冻后（PONR）** | 部分可逆 | 新服务端上已有用户写入。回退到 Cloudflare 会让旧 room 的 `headSeq` 落后于客户端 cursor → 客户端走 `ServerReset` → host 用 checkpoint 重播种（`chat_client.rs:1163-1176`、`doc_host.rs` 的 `spawn_chat2_checkpoint`），registry 由客户端重播种（`registry.rs:845`）。**不会自愈的**：切换后新增的通知 outbox/未读/徽标状态、排队的 nudge、`/blob` 上传、新注册的 PushDevice、切换后删除的 chat 墓碑。因此 PONR 定义为"解冻通告发出"，之后原则上向前修复而不回滚（新服务端自身的问题用 pgBackRest PITR 回到任一时间点）；若必须回到 Cloudflare，先从新服务端导出增量再手工合并 |

## 8. 运维

### 8.1 systemd、Postgres 安装与进程布局

- Rust 服务：专用用户 `cypher-edge`；`EnvironmentFile=/etc/cypher-edge/env`（0600，root 属主）或 `LoadCredential=`；`ProtectSystem=strict`、`ReadWritePaths=/var/lib/cypher-edge`、`PrivateTmp`、`NoNewPrivileges`；`Restart=on-failure`；`LimitNOFILE` 放宽（每 WS 一个 fd）。单二进制由 CI 静态构建（`x86_64-unknown-linux-gnu`，与 `scripts/package-linux.sh` 同工具链）。
- MinIO：专用用户 `minio`；`minio.service` 只绑定 `127.0.0.1:9000`（控制台不开或只绑 loopback）；`MINIO_ROOT_*` 走 `LoadCredential=`；`cypher-edge.service` 声明 `After=minio.service postgresql.service`。
- Postgres：发行版 PGDG 包（16 或 17，§11），数据目录在 NVMe（LUKS 之上）；只监听 unix socket；`pg_hba` 仅 `local … scram-sha-256`。关键参数：`synchronous_commit=on`、`wal_level=replica`、`max_wal_senders=5`、`archive_mode=on`、`archive_command='pgbackrest … archive-push %p'`、`archive_timeout=60s`、`checkpoint_timeout=15min`、`max_wal_size=4GB`、`shared_buffers=8GB`、`effective_cache_size=64GB`、`work_mem=32MB`、`max_connections=100`，autovacuum 见 §3.4。
- Caddy 用发行版包 + 自带单元。

### 8.2 备份、PITR 与恢复（恢复必须实测）

- **pgBackRest**：仓库一份本地（NVMe 另一分区）+ 一份**离机**（S3 兼容/SFTP/Storage Box，§11），仓库加密（`repo-cipher-type=aes-256-cbc`）；每周全量、每日差异、WAL 持续归档；保留 4 个全量 + 对应 WAL（≈ 4 周 PITR 窗口）。备选 WAL-G，功能等价。
- **大对象（MinIO）**：用 `mc mirror --watch`（或 bucket 复制规则，若离机目标也是 S3）把 `chat-blobs`、`attachments`、`archive` 持续同步到离机目标，`releases` 每日一次；`attachments` 开版本化。**不要**只对 MinIO 数据目录跑 restic（纠删码分片对文件级备份没有意义，单盘模式下才可作为补充）。与数据库指针的一致性由"对象先于指针"保证——恢复时数据库指向的对象一定在更早的副本里。
- **恢复演练**（每月自动化）：从离机仓库把最近的备份 + WAL 恢复到 `/var/lib/postgresql/restore`（`pgbackrest restore --type=time --target=…`），以 `CYPHER_EDGE_PG_URL` 指向它在 27641 端口启动第二个 Rust 实例，运行 smoke（改造后的 `edge/scripts/smoke.mjs`，覆盖 chat2/registry/device/blob/auth 501 路径）并跑 §5.5 的 room 级不变量；演练失败触发告警。**首次演练在 Gate A 之前完成**。
- 各 room 的夜间"R2 备份" alarm（§3.5）不计入正式备份策略。
- 客户端是 local-first：每个 chat 的 host 设备持有全文档并能重播种，registry 亦然。这使服务器级数据丢失的实际后果小于 RPO 字面值，但**不能**替代备份（通知状态、nudge、附件、墓碑、已退役设备的数据没有第二份）。

### 8.3 监控、告警、日志

- 外部探测：第三方拨测 `GET /health` 每分钟。
- 应用指标（Prometheus `/metrics`，仅 loopback，`metrics-exporter-prometheus`）：活跃 actor 数按类、WS 连接数按类、每分钟事件数、事务时长 p50/p99、收件箱/出站队列满次数、alarm 积压（最早到期与现在之差）与重试次数、WS 关闭码分布（4409/4410/1009/1011 尖峰是线索）、JWT 验签失败率、WorkOS/APNs 出站结果分布、`server_resets`/`floor_regression`/`quota` 计数、在途子请求数。
- Postgres 指标（`postgres_exporter`）：连接池饱和、锁等待、每分区死元组与 autovacuum 最近运行时间、checkpoint 时长、WAL 归档失败（pgBackRest `archive-push` 错误）、复制延迟（H1 后）、数据库大小。
- 主机指标：node_exporter（磁盘、inode、fd、内存、证书到期）。
- 告警：进程不在 / 拨测失败 5 分钟；磁盘 >80%；证书 <14 天；WAL 归档失败 >5 分钟；备份年龄 >26 小时；恢复演练失败；JWT 失败率或 5xx 突增；某分区死元组 >100 万（autovacuum 没跟上）。
- 日志：`tracing` → journald，保留 30 天；应用日志不含 token（ingress 从记录用 URL 中剥除 `token`）；Caddy 访问日志对 `token` 查询参数删除或哈希；APNs 相关日志沿用 `apns.ts:16-30` 的白名单诊断。

### 8.4 升级流程

- **应用**：CI 构建静态二进制，`cypher-edge-<sha>.tar.gz`；GitHub Actions 通过 SSH 上传到 `/opt/cypher-edge/releases/<sha>/`，切 `current` 符号链接，`systemctl restart cypher-edge`。保留前两版供回切。重启代价：所有 WS 断开约 1–3 秒，客户端自动重连；不做蓝绿。
- **Schema**：应用启动时跑嵌入式迁移（`refinery` 或 `sqlx::migrate`），版本号单调；破坏性变更走"先加列后删列"两次发布；分区数不在迁移里改。
- **Postgres 小版本**：包升级 + `systemctl restart postgresql`（秒级，应用事务失败→客户端重试）。**大版本**：维护窗口内 `pg_upgrade --link`（分钟级），之后重建 pgBackRest 的 stanza 并立即全量备份；H1 之后走"先升备再切换"的滚动方式。
- 内核/OS 更新：`unattended-upgrades` 仅安全更新，重启放在每周固定低峰时段并提前通告。

### 8.5 单机故障模式与可用性预期（诚实）

| 故障 | 影响 | 恢复 |
|---|---|---|
| 应用进程崩溃 | 秒级，systemd 拉起，客户端重连；actor 从表重建 | 自动 |
| Postgres 重启 | 秒级；期间事件 COMMIT 失败 → 不发 ack → 客户端重试；actor 失效缓存并重载 | 自动 |
| 计划重启（部署/内核） | 秒级到 5 分钟 | 自动 |
| 磁盘满 | COMMIT 失败 → 客户端收到无 ack/error、outbox 堆积（不丢） | 告警 + 人工 |
| NVMe 损坏 | ≥4 盘纠删码时 MinIO 无损；Postgres 视 RAID/ZFS 而定。否则服务中断至恢复完成；RPO = 上一次 WAL 归档（≤1 分钟）+ 大对象为镜像滞后（分钟级） | 人工，小时级（H1 后分钟级） |
| 整机/机房故障 | 服务中断至在别处重建；RTO 取决于 H2 | 人工，小时到天级 |
| 上行链路被打满 | 服务中断，无法本地缓解（§3.7） | 依赖托管商 |
| WorkOS 不可达 | 新登录与 token 刷新失败；已持有效 token 的连接继续（≤ TTL，`auth.rs:217`）；JWKS 缓存宽限 | 外部 |

对比 Cloudflare：DO 是多副本、多区域自动迁移的托管服务，历史上的不可用主要来自应用层 wedge（`docs/chat2-sync.md`、`docs/registry-sync.md` 记录的事故），而非平台。单机的可用性上限由重启窗口和硬件决定，合理预期 99.5% 左右（每月约 3.6 小时不可用预算，含计划维护），H1/H2 后可到 99.9% 量级，仍无区域冗余。客户端的 local-first 设计把"服务端不可用"降级为"跨设备同步暂停"，这是接受单机的前提。

## 9. 安全

- **信任边界**：Worker/DO 的进程间边界消失，变成同进程的函数调用。JWT 验签仍是进入任何 actor 的唯一入口（`index.ts:189`）；ingress 丢弃入站 `x-cypher-auth-user`/`x-cypher-room-kind`。所有 room 继续按 `owner`/`userId` 做所有权判定（`chat-room.ts:100-118`、`device-room.ts:151-158`、`registry` 由路径推导用户 `index.ts:332`）。
- **网络暴露**：仅 Caddy 监听 80/443；Rust 监听 127.0.0.1；Postgres 仅 unix socket；SSH 仅密钥、`fail2ban`；`/metrics`、`/releases-admin` 仅 loopback 或 token。
- **数据库权限**：应用角色只拥有自己的表，无 `SUPERUSER`/`CREATEDB`；迁移由部署步骤用独立角色执行；pgBackRest 用 `postgres` 系统用户。
- **密钥**：`WORKOS_API_KEY`、`APNS_PRIVATE_KEY`（PKCS#8）、`RELEASES_UPLOAD_TOKEN`、`EXPORT_TOKEN`（仅导出期）、pgBackRest 仓库密码放 `/etc/cypher-edge/env`（0600 root）由 systemd 注入；不进 git、不进日志、不进 shell 历史（`docs/notifications.md` "Enabling delivery" 的纪律沿用）。WorkOS key 建议在切换时**轮换**一把新的。
- **静态加密**：数据卷（Postgres、MinIO 数据目录、archive）用 LUKS（dm-crypt）。代价：无人值守重启需要远程解锁（dropbear-initramfs 或 TPM/clevis）——§11 由用户决定。Postgres 备份由 pgBackRest 仓库加密；MinIO 离机副本走加密传输，落地加密取决于目标端（§11 第 16 项）。
- **用户隐私数据**：transcript、checkpoint、tail、diff、附件均为用户私有内容，以明文存于服务器磁盘（与 Cloudflare 时期在 DO/R2 中的状态相同，不是端到端加密）。区别在于：现在**运营者对全部数据有 root 权限**。应在用户通告与隐私声明中明确。
- **日志脱敏**：`?token=`（§8.3）；APNs URL 含设备 token（`apns.ts:16` 注释），沿用白名单诊断；`auth-routes.ts:146-153` 只记 refresh token 的 SHA-256 前缀——保留。
- **无鉴权面**：`/notifications/revoke` 只需 64-hex 的 `bindingId/scope` + `lease` + 更新的 `epoch`（`index.ts:176-187`），能力模型不变；加每 IP 限速。
- **供应链**：Rust 依赖由 `Cargo.lock` 固定并经 `cargo audit`；Caddy/Postgres 用发行版包或官方签名；不安装 Cloudflare 相关工具到生产机。

## 10. 分阶段工作包

工期为一人估算，含测试。"窗口前"= 可在维护窗口之前完成并演练。**总计约 29–38 个工作日（6–7.5 周）**；下限只有在 WP3（通知/推送）不返工时才可能达到。

| WP | 内容 | 依赖 | 估时（天） | 窗口前 |
|---|---|---|---|---|
| WP0 前置确认 | §11 的必答项：服务器可登录、OS、DNS 控制权、`.p8` 与 WorkOS key 在手、Cloudflare API token（DO 读 + R2 S3 凭证）、离机备份目标、Postgres 版本、**NVMe 盘数与 MinIO 模式**、MinIO 当前许可核对 | — | 0.5 + 等待 | ✅ |
| WP1 服务端骨架 | 新 crate `crates/edge`（bin `cypher-edge`）：axum ingress + 路由表（§4.1 含 410/兼容 `/diff`）、JWT/JWKS 验签（`jsonwebtoken`）、`AUTH_MODE` 三态、`RoomHost` + actor 运行时（收件箱、socket 表、output gate、在途子请求集合、空闲退出）、AlarmScheduler、Postgres 池 + 嵌入式迁移（§3.4 schema）、BlobStore/ReleaseStore（S3 客户端 → MinIO，`object_key` 指针，孤儿 sweep）、配置/密钥、metrics/tracing、leader 锁 | WP0 | 3–4 | ✅ |
| WP2 ChatRoom | 复用 `cypher-sync::chat_frames`（加 allowlist）；chat-log 语义（append/dedupe/floor/prune）、hello/state/rowsReq/push/presence/probe、HTTP checkpoint（Range）/rows（帧流 + 4 MiB 截断）/rows POST/tail/diff/stats/reset、配额、presence TTL、`pushOutcomes` 采样与 `backupDirty` 去重（§14）、夜间备份 alarm | WP1 | 4–5 | ✅ |
| WP3 RegistryRoom + Notifications + PushDevice + APNs | 复用 `cypher-doc::registry::{apply_op, validate_op, row_to_seed_op}`；hello full/delta、`applyPushBatch`（whole-batch 拒绝、rows-before-ack）、presence（WS + `beat=1`）、GC/gcFloor alarm；**通知状态机全量移植**（`notifications.ts` 462 行：event/observe 双路径、run 标记、`enqueued` 去重、`flush` 的在途子请求形态、徽标 job）；PushDevice（registration/epoch/retired/lease/deliveries/badge）；APNs h2 + ES256。**最大风险项**：无真机基线（`docs/notifications.md:3-15`），必须先补 golden 测试（§12.3） | WP1 | 5–6 | ✅ |
| WP4 DeviceRoom | 复用 `cypher-rpc::device_room` codec；host/client 角色、tag 路由、`from/to` 标记与剥离、`pickLiveHost` 语义（`last_pong_at` ∨ `joined_at`）、4409 抢占、四种 relay 错误、nudge 队列（去重/256 上限/回放顺序）、sidecar、status | WP1 | 1.5–2 | ✅ |
| WP5 Auth/WorkOS/发布/附件 | `auth-routes` + `workos` 移植（错误信封与 `invalid_grant` 语义）、`/auth/*/callback` 页面、`/health`、`/install.sh`、`/releases/*`（头部逐字，从 MinIO 流式读）、`release.py` 的 S3 客户端替换、`/blob/*`、`/attachments/*`、`/notifications/revoke` | WP1 | 2–3 | ✅ |
| WP6 协议兼容测试 | §12：向量矩阵核对；从 TS 单元/workerd 测试导出 golden 测试（Rust，真实 Postgres）；差分回放 harness（TS Worker under `wrangler dev` vs Rust，同输入比输出）；崩溃原子性（COMMIT 前 kill -9）、output gate、alarm 重启恢复、WS 1 MiB 边界、慢消费者 1011；`scripts/e2e-smoke.sh` 改起 Rust；`smoke.mjs` 重写 | WP2–WP5 | 4–5 | ✅ |
| WP7 导出/导入 | Worker `/__export` + `FREEZE_WRITES`（一次性部署）；`export-cloudflare.mjs`（API 枚举、名称还原、校验清单）；`import-cloudflare-export`（Rust，COPY）；`verify-import.sql`（§5.5）；生产只读演练 | WP1（schema） | 2.5–3 | ✅ |
| WP8 服务器落地 | 系统加固、LUKS、Postgres 安装与调参、pgBackRest（本地 + 离机）、**MinIO 安装（纠删码或单盘）、四个 bucket 与最小权限 key、版本化、`mc mirror` 离机同步**、Caddy、systemd、Prometheus/告警（含 MinIO 指标）、证书预签发、恢复演练脚本 | WP0 | 2.5–3.5 | ✅ |
| WP9 预发验证 | 演练导入 + `/etc/hosts` 双端验证（桌面/iOS）、推送真机、24 小时浸泡、真实机器上重测 §1.5 的【假设】数字 | WP6–WP8 | 2 | ✅ |
| WP10 CI/CD 与文档 | `deploy.yml` → 构建 + SSH 部署；`ci.yml` 增加 `crates/edge` 测试与差分 job；`release.py` `HttpStore`；`docs/local-edge.md`、`docs/ci-cd.md`、`ARCHITECTURE.md` §1/§6（"DO stay TypeScript" 结论作废）；用户通告文案 | WP1、WP7 | 1–2 | ✅ |
| WP11 切换 | §6 步骤 7–14 | 全部 | 0.5（窗口 4 小时） | — |
| WP12 收尾 | 首次真实发版走新链路；T+14 天删除 Cloudflare 资源；SessionRoom 归档；zone 与 landing 迁出（若在范围）；`diff_sync.rs` 路由跟进；`edge/` 归档；H1 热备部署另立项 | WP11 | 1–2 | — |

**关键路径**：WP1 → WP3 → WP6 → WP9 → WP11（约 16–20 天串行）。WP2/WP4/WP5 与 WP3 并行（同一人则串行，这是工期主体）；WP7、WP8 只依赖 WP1 的 schema/WP0，可穿插；WP10 随时。**窗口前能完成的**：除 WP11/WP12 外全部，含 Gate 0–A 与恢复演练。

## 11. 开放问题（需用户回答）


1. **服务器地点与托管商**：用户分布在哪（决定 WS/RPC 往返延迟；终端与远程控制走 DeviceRoom 中继，对 RTT 敏感；§1.5 已指出从全球边缘收缩到单区域是延迟回退）；托管商是否提供 L3/L4 清洗（§3.7）；上行带宽（DeviceRoom 中继是第一瓶颈）。
2. **DNS 与"完全退役"的边界**：`letscypher.app` 注册商、谁能改 NS；zone 是否必须迁出 Cloudflare；迁到哪；landing 与 `www` 301 是否同期迁出。
3. **OS/发行版与磁盘加密**：假设 Ubuntu 24.04 LTS 或 Debian 12；是否接受 LUKS 及其无人值守重启的解锁方案。
4. **发布产物分发**：大文件继续由服务器直出，还是 302 到 GitHub Release；`install.sh` 与 `crates/update` 是否允许跨域重定向。
5. **密钥在手**：APNs `.p8`（`APNS_KEY_ID=8WP4N48QXB`，`wrangler.jsonc:96`）原件是否保存；`WORKOS_API_KEY` 建议直接新建。
6. **Cloudflare 凭证**：能否创建具备 Durable Objects 读、Workers 部署、R2 S3 访问的 API token 用于导出。
7. **离机备份目标**：第二台机器 / 对象存储 / Storage Box；保留期。
8. **维护窗口**：时长与时间；用户名单与通告渠道；是否所有用户都能配合排空 outbox。
9. **兼容路由保留期**：`POST /diff/:chatId` 与 `PUT /attachments/*` 保留多久。
10. **遗留 SessionRoom 数据**：仅冷归档是否足够；是否需要工具从归档恢复某个 s2 文档。
11. **开发实例**：是否在同一台服务器上运行 `edge-dev` 第二实例（§3.10）。托管开发 Worker 已删除，当前开发环境是本地 `wrangler dev`；若本地足够，这一项可以直接取消。
12. **隐私声明**：是否需要向用户说明"运营者对服务器数据有完全访问权"的变化（§9）。
13. **可用性目标**：是否接受 §8.5 的 99.5% 量级；H1（数据库热备）与 H2（应用冷备）是否立项、何时。
14. **自建 IdP**：是否作为独立后续项目评估（本迁移保留 WorkOS）。

**Postgres 与服务端实现相关：**

15. **Postgres 大版本**：16 还是 17（PGDG 包）；是否接受 pgBackRest 作为备份工具（备选 WAL-G）。
16. **pgBackRest 离机仓库**：目标类型（S3 兼容 / SFTP / Storage Box）与仓库加密密钥的保管方式；`archive_timeout`（RPO，建议 60 秒）与 PITR 保留期（建议 4 周）。
17. **热备时机**：H1 是否在切换前就把第二台机器准备好（增加窗口前工作但切换当天就有热备），还是切换后独立立项；同步还是异步复制。
18. **故障转移工具**：pg_auto_failover（需一个小监控 VM）还是 Patroni（需 etcd 三节点）。
19. **分区数**：`chat_rows` 64 个哈希分区在建表时固定，接受"改动需重建表"的约束吗。
20. **夜间 room 级备份**：在 pgBackRest 之下是否保留 §3.5 的 alarm 备份输出（建议保留逻辑、输出可关）。
21. **`edge/` TS 目录去向**：差分 harness 退役后归档到分支还是保留在主干作只读参考（建议归档，避免两份"实现"并存）。
22. **慢消费者关闭码**：§3.2 建议 1011；是否需要在客户端加区分（当前任何关闭都重连，无需改）。
23. **codec 抽 crate**：是否在切换前就把 `chat_frames`/`stream_preview`/设备 codec/注册表核心抽成 `cypher-wire`（不改行为；建议切换后）。
24. **UX-2 的发布者身份方案**：§13.2 给了两个候选（WorkOS `sid` 绑定 vs 服务端签发的设备发布密钥），需要选一个并做设计评审。

**对象存储（MinIO）相关：**

25. **盘布局**：服务器有几块 NVMe；MinIO 用纠删码（≥4 盘）还是单盘模式；Postgres 数据卷用 ZFS/RAID 还是单盘。MinIO 纠删码与 ZFS 不叠加。
26. **MinIO 许可与发行**：核对当前开源版的许可条款与功能范围是否满足（2025 年有过调整）；不满足则改用 Garage 或 SeaweedFS（应用只依赖 S3 API，配置级切换）。
27. **CI 到 MinIO 的通道**：`release.py` 上传经 SSH 隧道、WireGuard，还是给 MinIO 单独开一个仅限 CI 出口 IP 的公网监听；以及大文件 GET 是否 302 到 GitHub Release（§3.5 带宽风险）。

---

## 12. 协议兼容与差分测试（本设计的核心风险章节）

本设计的安全论证由四层组成：**(1)** 已有的跨语言向量原样复用；**(2)** 对没有向量的线上行为，先从 TS 实现导出 golden 测试；**(3)** 事故后加固的行为逐条列为必须逐字移植项；**(4)** 差分回放把 Rust 服务端与 TS Worker 对跑。差分回放 0 差异是 §6 的 Gate 0。

### 12.1 今天已有的跨语言向量【现状】

只有预览协议使用**共享 JSON fixture**；其余三组是**三端手工镜像**的测试（同样的输入与期望分别写在三种语言里）：

| 契约 | TS | Rust | Swift | 形态 |
|---|---|---|---|---|
| chat2 帧封装 `[type u8][len u32 LE][header][payload]`、拒绝畸形/超长 header | `edge/src/chat-frames.test.ts`（5 例） | `crates/sync/src/chat_frames.rs:151-209`（3 例） | `apps/ios/CypherTests/ChatFramesTests.swift`（4 例，头注释声明三端镜像） | 镜像 |
| 注册表合并核心（HLC 序、字段 LWW、tombstone/revive、guard tombstone、re-seed 保留时钟、任意到达序收敛、`validateOp`、`maxClock`） | `edge/src/registry-core.test.ts`（13 例） | `crates/doc/src/registry/tests.rs`（31 例，`:1-2` 声明镜像） | `apps/ios/CypherTests/RegistryCoreTests.swift`（16 例，`:1-4` 声明三端向量） | 镜像 |
| 设备帧 `uleb128(len) ‖ JSON ‖ payload`、relay 错误载荷 | `edge/src/device-frame.test.ts`（2 例） | `crates/rpc/src/device_room.rs:973-1088`（7 例，含 `byte_parity_with_ts_encoder`） | **无独立测试文件**（`DeviceRelayClient.swift:5-10` 只有注释） | 镜像（Swift 缺） |
| 预览帧 wire + 状态机 | `edge/src/stream-preview.test.ts` 读 `fixtures/stream-preview-v1.json`（48 例） | `crates/sync/src/stream_preview.rs:116-117`（48）、`preview_link.rs:571-573` 读 `preview-reducer-v1.json`（13） | `CypherTests/StreamPreviewTests.swift:9`、`PreviewProjectionTests.swift:8`；`scripts/test-stream-preview.sh` 独立编译 Swift 跑同一 JSON | **共享 JSON** |

Rust 服务端复用 `cypher-sync`/`cypher-doc`/`cypher-rpc` 的这些 codec/核心后，上表自动覆盖服务端；需要补的是 Swift 设备帧向量（不阻塞切换，客户端已在线上验证）。

### 12.2 线上行为清单与向量覆盖（缺口即待补 golden 测试）

| 行为（服务端定义） | 今天的测试 | 跨语言向量 | 处置 |
|---|---|---|---|
| chat-log：dedupe 返回原 seq、append 顺序与 `headSeq`、1 MiB 行上限、`rowsAfter`/excludeOwn、checkpoint 裁剪 + blob、floor-monotonic/head-bounded 守卫、churn 有界（`chat-log.ts`） | `edge/test/workerd/chat-log.workerd.test.ts`（7 例，真实 SQLite） | **无** | 逐例移植为 Rust golden（真实 Postgres） |
| ChatRoom WS：hello→`state` 六字段 + frontier 载荷、`hello_first`、`ack{dup}`、错误帧带 `batchId`、配额帧、presence 仅转发他人、probe→`probeOk`、1003/1009/4410、`bad_frame`（`chat-room.ts:340-513`） | **无直接测试**（仅 e2e-smoke 与预览 workerd 测试间接覆盖） | **无** | 新写 golden（从 TS 行为推导）+ 差分 |
| ChatRoom HTTP：owner claim/403/404 分支、checkpoint GET 200/206/416 + 头、POST checkpoint 400/409/413、rows GET 帧流与 4 MiB 截断、rows POST dup-before-quota + 429、tail/diff content-type 回放、stats JSON、reset（`chat-room.ts:95-336`） | **无** | **无** | 同上 |
| RegistryRoom：`full` 判定三条件、rows-before-ack、`ack{batch,seq,applied}`、`bad_push`/`invalid_op` 整批拒绝、500 ops 上限、1,000,000 字符→1009、非文本→1003、坏 JSON→1002、`/rows?beat=1` presence、reset→4410、墓碑 GC 与 `gcFloor`、`seq` 仅在 `applied>0` 时递增（`registry-room.ts`） | 仅通过通知 workerd 测试间接触达 | **无** | 新写 golden + 差分 |
| DeviceRoom：`from` 打戳/`to` 剥离、`host_offline|client_gone|client_closed|host_closed`、`host_closed` 仅在无存活 host 时、4409、1002、nudge 队列（去重/256 丢最旧/升序回放/回放后清空）、status、sidecar（`device-room.ts:143-306`） | `pickLiveHost` 纯函数 9 例（`device-host-liveness.test.ts`）；其余**无** | **无** | `pickLiveHost` 镜像为 Rust 单元；其余新写 golden + 差分 |
| Notifications 状态机：`event` 路径（5 分钟时钟窗、`eventState` 去重、`run:` 标记、short-run 30 秒、`enqueued` 去重、iOS 前台即读）、`observe` 路径（host 归属、`childrenSettled`）、`flush`（每次 alarm 2 条、`defer` 15 秒、目标平台选择、每次 await 后重读、5 次重试 5·2ⁿ 秒封顶 120 秒）、徽标 job（`notifications.ts`、`notifications-model.ts`） | `edge/test/workerd/notifications*.workerd.test.ts`（16 例，真实 SQLite）+ `notifications-model.test.ts`（5）+ `notification-routes.test.ts`（2） | **无**（iOS 的 `NotificationTests` 测客户端） | **逐例移植为 Rust golden**，且 Rust 侧注入时钟以复现时间分支；差分只覆盖非时间路径 |
| PushDevice：注册 epoch/`retired` 水位、lease、`stale` 409、`permanent`、`delivery:` 状态与 20 秒 `sending` 窗、512 条/24 小时 GC、徽标归一化（`push-device.ts`） | 含于通知 workerd 测试（"global APNs token ownership" 2 例 + badges） | **无** | 同上 |
| APNs 请求形状（头、collapse id、payload）、三态返回、白名单诊断（`apns.ts`） | `apns.test.ts`（7 例） | **无** | 移植为 Rust 单元（mock HTTP） |
| auth-routes / WorkOS：错误信封 `{error, code, retryable}`、`invalid_grant` 唯一吊销码、邮箱验证续流、callback 页面（`auth-routes.ts`、`workos.ts`） | `auth-routes.test.ts`（39 例） | **无** | 移植为 Rust 单元（录制的 WorkOS 响应作 fixture） |
| JWT 验签：issuer、JWKS、`sub/sid/org_id` 提取、`dev`/`dev-locked` 模式（`auth.ts`） | **无**（依赖 `jose`） | **无** | 新写单元（测试 JWKS） |
| 路由与校验正则：`ID_RE`、`PART_RE` 解码后校验、sidecar 名、`CHAT_ID_RE`、64-hex、lease uuid、404 信封、`x-cypher-*` 剥离（`index.ts`、`device-room.ts:88`） | **无** | **无** | 新写单元 + 差分（含 fuzz） |
| SessionRoom 更新日志（遗留） | `update-log.test.ts`、`update-log.workerd.test.ts` | — | 不移植（410） |

**汇总：今天缺跨语言向量的线上行为** = 上表除第一行 12.1 所列四组之外的全部：chat-log 语义、ChatRoom WS/HTTP 全部处理器行为、RegistryRoom 全部处理器行为、DeviceRoom 路由/nudge/状态、通知状态机、PushDevice、APNs 请求形状、auth-routes/WorkOS 错误语义、JWT 验签、路由/正则校验；外加 Swift 侧缺设备帧 codec 测试。这些都需要在 TS 实现退役前从它导出 golden 测试。

### 12.3 必须逐字移植的事故加固行为

每一条都在 §12.2 有对应 golden 项，且在差分回放里有专门场景：

1. **去重先于配额**与 batchId 重放 ack：`chat-log.ts:61-63,77-78`、`chat-room.ts:444-452`（WS）、`:249-251`（HTTP）。
2. **行先于 ack**（output gate 顺序）：`chat-room.ts:465-484`、`registry-room.ts:401-408,343`。
3. **checkpoint floor 单调**与 head 上界：`chat-log.ts:129-149`（`floor_regression`/`ahead_of_head` → 409）。
4. **host 存活选择**：`device-room.ts:75,127-140,328-339`（最新 pong ∨ joinedAt，75 秒窗，`exclude` 正在关闭的 socket）。
5. **nudge 有界**：`device-room.ts:87,224-230`（去重 upsert、超过 256 丢最旧）。
6. **通知状态机**：`notifications.ts` 全部分支，尤其 await 后重读（`:399-407,419,424`）与 `iosViewingChat` 即读（`:199-203,313`）。
7. **注册表 HLC/LWW 合并**：`registry-core.ts`（复用 `cypher-doc`，向量已在）。
8. **ServerReset / 重播种路径**：服务端只需保证 `state.headSeq`/`state.seq` 真实反映存储（`chat_client.rs:1163-1176`、`registry.rs:845` 靠它自愈）；reset 后 4410 关闭（`chat-room.ts:327`、`registry-room.ts:250`）。

### 12.3b 内存态重建约束（任何把持久状态移入内存的优化都适用）

room actor 随时可能被逐出并重建（Cloudflare 的 hibernation，Rust 侧的空闲退出）。
因此凡是从表里移进内存的状态，必须满足下列之一：

1. 能在构造/首次使用时用**一次读**精确重建（`backupDirty`、`getAlarm()` 的当前时刻）；
2. 丢失**无害且可收敛**（`pushOutcomes` 这类纯归因遥测，socket 关闭与 alarm 时落表）；
3. **构造上单调**，丢失只导致跳号而非回退（`seq` 的块预留式分配）。

**不满足任何一条的状态，不得移入内存。** 这条规则是 2026-09-21 那批 per-push 开销
优化（`pushOutcomes` 内存化、`backupDirty` 只在 0→1 写、registry `setAlarm` 去抖）
成立的前提，对 Rust 服务端的 actor 同样适用。

### 12.4 差分回放 harness【建议】

- **两个被测端**：TS Worker 以 `wrangler dev --var AUTH_MODE:dev`（`scripts/e2e-smoke.sh:80` 已有启动方式）；Rust 服务端以 `AUTH_MODE=dev` + 一次性 Postgres 数据库。两端每个场景前重置状态。
- **驱动**：一个 Rust 二进制（复用客户端 codec）读取场景文件（JSON/YAML）：步骤 = HTTP 请求（方法、路径、头、体）或 WS 动作（open/send/expect/close）；对两端执行相同序列，记录全部响应与帧。
- **比较**：HTTP 状态码 + 允许列表内的头 + 体；JSON 体做**结构**比较；二进制帧按类型字节 + header JSON 结构 + payload 字节比较；忽略名单：`at`、`receivedAt`、`lastOkAt`、`checkpointAt`、`connectedSockets`、`etag`、随机 id（uuid/lease 按位置对应）。任何不在忽略名单内的差异 = 失败。
- **场景来源**：(a) rows-written 基线的 P0 回放 fixture（243 个 durable batch，`docs/rows-written-baseline.md:36-37`、`scripts/rows-written-baseline.sh`），一份真实转录流；(b) `scripts/e2e-smoke.sh` 的两引擎流程分别对两端跑（不比较字节，只比较最终状态）；(c) 按 §4.1 每条路由 × §4.2 每条行为手写场景；(d) fuzz：按 codec 语法生成合法/非法帧，比较错误码与关闭码。
- **边界（诚实）**：`wrangler dev` 的 TS 端不能注入时钟，所以配额窗口、presence TTL、通知延迟、墓碑 GC 等时间分支**不在差分覆盖内**，由 §12.2 的 golden 测试（Rust 侧可注入时钟）负责；差分覆盖时间无关路径。
- **落地**：CI job `edge-diff`（需要 Node + Postgres service），WP6 交付；切换后保留到 T+14 天，随 `edge/` 归档一起退役。

### 12.5 Gate 0 退出条件

- §12.1 四组向量在 Rust 服务端 crate 内通过；
- §12.2 每一行的 golden 测试存在且通过（真实 Postgres）；
- §12.4 全部场景连续 3 次 0 差异；
- 崩溃原子性：COMMIT 前 `kill -9`，重启后 `headSeq == MAX(seq)` 且客户端重放得到 `dup:true`；alarm 重启后恢复；WS 1 MiB 边界 1009；慢消费者 1011。

---

## 13. 切换后体验阶段

切换本身不改任何客户端行为（§4.3）。下面两阶段是自托管之后"免费"或"接近免费"的体验改进，各自独立发版。

### 13.1 阶段 UX-1：撤掉客户端 2 秒合并窗口

- **现状**：桌面 Engine 把 2 秒内的 Loro 更新合并成一个 batch（`crates/sync/src/chat_client.rs:725-753`，两处 `Duration::from_secs(2)`），存在理由是压低 Cloudflare rows written（HANDOFF §1.3：同一 fixture 1,593 → 225 rows）；iOS 客户端无此窗口（`ChatRoomClient.swift` 无同类常量）。
- **改动**：把窗口降到约 0（或 100 ms 以合并同一 tick 内的多次提交），并配合 §14 的服务端 per-push 开销修正。
- **预期**：跨设备感知延迟从"最多 2 秒 + 120 ms + RTT"降到"120 ms + RTT"（`STREAM_COMMIT_MS`，`crates/doc/src/constants.rs:15`）；服务端每重度用户约 0.1 → 0.7 次 push/秒【假设：按 fixture 的 7 倍外推】，Postgres 上无感；`chat_rows` 死元组按比例增加，§3.4 的 autovacuum 配置已按此预留。
- **发布**：桌面 Engine 一次发版；服务端无改动；可在切换后一周内做。

### 13.2 阶段 UX-2：`ephemeral-stream-v1` 上生产

- **现状**（`docs/ephemeral-stream-v1.md`）：帧类型 `0x20–0x26`、协商、流控、reducer 已在 TS/Rust/Swift 三端实现并通过本地原生互通；48 个 wire 向量 + 13 个状态向量共享（§12.1）；默认关闭。服务端 relay 的实现（`development-preview.ts`）保留，但其 Worker 入口已随托管开发 Worker 一并删除（2026-09-22，`docs/local-edge.md`），因此**生产化时需要重新接线**；生产 `ChatRoom` 构造时 `preview` 仍为 `undefined`（`chat-room.ts:82`）。**四道开发门**：服务端 `AUTH_MODE=dev-locked` + `DEV_PREVIEW_ENABLED=true` + 64-hex `DEV_PREVIEW_PUBLISH_TOKEN`（`development-preview.ts:40-45`）；桌面 `CYPHER_DEV_STREAM_PREVIEW=1`（`crates/engine/src/lib.rs:886`）；发布凭据 `CYPHER_DEV_PREVIEW_PUBLISH_TOKEN`（`lib.rs:898`）；iOS `-dev-stream-preview` 启动参数（`apps/ios/Cypher/App/AppConfig.swift:24`）。
- **唯一真正的阻塞项：发布者授权**。`HELLO.device` 是客户端自报（`chat-room.ts:388-391` 直接采用 header 值），ChatRoom 只有用户级所有权校验；文档明确"HELLO.device 不足以授予 preview 作者权限"。开发环境用一把共享发布 token 区分"能发布 vs 只能看"，不提供设备级隔离。
- **【建议】设备绑定的发布者身份，在 ingress 准入时判定**，两个候选（§11 第 24 项选一）：
  - **(a) WorkOS 会话绑定**：JWT 的 `sid`（`auth.ts:14-20` 已提取）由服务端验证；某个 `(userId, sid)` 以 `role=host` 成功占有 `d2/{deviceId}`（`device-room.ts:151-158` 的 owner claim）即证明该会话控制该设备；chat2 WS 准入时，ingress 向 DeviceRoomActor 查询"`(userId, sid, device)` 是否为当前存活 host"，是则该 socket 获发布者资格。零新密钥、零客户端改动（chat2 socket 已带 `device`，token 已带 `sid`）；代价：桌面 UI 与 Engine 共享会话不影响（发布者是 Engine），但同一用户在两台设备上各自的 `sid` 不同，隔离成立。需确认 WorkOS refresh 后 `sid` 稳定【假设】。
  - **(b) 服务端签发设备发布密钥**：host 占有 DeviceRoom 时服务端签发一把随机密钥并通过已鉴权的 host socket 下发，Engine 在 chat2 upgrade 时以 `x-cypher-preview-publisher` 头（`development-preview.ts:13`，已存在）出示；服务端按 `(deviceId → 当前密钥)` 校验。需要 Engine 一处改动（收密钥、带头）；隔离更强（不依赖 IdP 的 `sid` 语义）。
- 其余：去掉四道 dev 门（服务端默认注入 relay；客户端按能力协商，不再看环境变量）；文本 only、60 KiB 段上限（`stream-preview.ts:9-10`）、`PREVIEW_LIMITS`（`development-preview.ts:14-15`）按生产重新评估；`baseSeq` 不推进 cursor、Finished 不是 ACK 等接收契约不变。
- **对计费请求是增加，不是减少【实测推算】**：预览 delta 是入站 WS 消息，按 20:1 计费。若 delta 与 120 ms 提交节拍同频，一个 5 分钟轮次约 2,500 条消息 = 125 个计费请求，而今天 2 秒窗口下的约 150 次 durable push 只折合约 7.5 个——chat 路径的计费请求约 **×16**。按 2026-09-18 的 chat WS 量推算，全量启用后 chat 计费请求从每天约 1,400 涨到约 22,000。仍远在额度内，但**方向是增加**：UX-2 是用请求数换延迟，不是省钱，排期时不要和降本项混在一起算收益。
- **发布**：服务端 + 桌面 + iOS 各一次发版；iOS 走 TestFlight，故排在 UX-1 之后。

### 13.3 未来选项（不在计划内）：单 socket 多路复用

把一台设备的全部 room（≤14，§1.5）复用到一条 WebSocket：socket 数约减 7 倍，连接建立/ping 开销同减。代价是两端客户端的协议改动（帧加 room 路由字段、每 room 的 hello/cursor 状态机并行）与服务端的连接级复用层，且与 §1.5 的瓶颈顺序（带宽先于 socket）不符。记录但不排期。

---

## 14. 与后端无关的负载修正（无论后端都应做）

这些修正不改变线上协议，可在切换前后任意时间独立发布；对 Postgres 的直接收益是更少的行版本（更少 vacuum 工作）。

1. **注册表客户端无合并窗口**【现状】：`crates/engine/src/workspace_host.rs:700-707` `mutate()` 每次变更都 `room.nudge()`；`crates/sync/src/registry.rs:443-449` `nudge()` → `:890-903` 主循环立即 `push_pending`。`sessions.rs:1239-1278` `set_status` 每次都写 `updated_at = now`（`:1264`），经 `publish_session`（`:1154-1177`）→ `record_session`（`workspace_host.rs:928-932`）→ `upsert_session`（`crates/doc/src/registry.rs:1202-1209` 把 `updatedAt` 写进字段）→ 每次状态翻转一次立即 push。**【建议】** 在 `RegistryClient` 加 500 ms–1 s 的合并窗口（同一 batch 内多次 op 合并；断线/终止状态 `Idle|Errored|AwaitingInput`、删除类 op 立即 flush）；`set_status` 在状态未变时不改 `updated_at`（freshness 由已节流的 `touch_session` 负责，`sessions.rs:1178-1184` 注释）。桌面 Engine 一次发版。
2. **ChatRoom 每次 push 的附加写**【现状】：`recordPush`（`chat-room.ts:528-544`）每次 push 读-改-写 `pushOutcomes` JSON；`markBackupDirty`（`:546-547`）每次 push 无条件写 `backupDirty=1`。HANDOFF §1.4 把它们列为"每 push 五行"之外的附加类别。**【建议】**（在 WP2 的 Rust 实现里直接采用）：`pushOutcomes` 在 actor 内存累计，按采样（每 N 次或每 60 秒）或在 alarm/空闲退出时落表，`/stats` 读内存 + 表；`backupDirty` 在内存中做 0→1 守卫，只在翻转时写。若切换前 TS 侧也想省 rows，同样两处改动各一行，但没有必要。

---

## 附录 A：TS → Rust 模块对应与可复用件

| TS | Rust（`crates/edge`） | 复用 |
|---|---|---|
| `index.ts` 路由 + `forward` | `ingress.rs`（axum Router）+ `RoomHost` | — |
| `auth.ts` | `auth/jwt.rs`（`jsonwebtoken` + JWKS 缓存） | — |
| `auth-routes.ts`、`workos.ts` | `auth/routes.rs`、`workos.rs`（reqwest） | — |
| `chat-frames.ts` | `cypher_sync::chat_frames` + 服务端 allowlist | ✅ 复用 |
| `chat-log.ts`、`chat-room.ts`、`blobs.ts` | `rooms/chat.rs`（actor）+ `store/chat.rs`（SQL）+ `blobs.rs` | — |
| `registry-core.ts` | `cypher_doc::registry::{apply_op, validate_op, row_to_seed_op, encode_hlc}` | ✅ 复用 |
| `registry-room.ts`、`notifications.ts`、`notifications-model.ts` | `rooms/registry.rs`、`rooms/notifications.rs`（含在途子请求集合）、`notifications_model.rs` | — |
| `device-room.ts` codec | `cypher_rpc::device_room::{encode_device_frame, decode_device_frame, relay_error_code}` | ✅ 复用 |
| `device-room.ts` 其余 | `rooms/device.rs` | — |
| `push-device.ts`、`apns.ts`、`apns-sender.ts` | `rooms/push.rs`、`apns.rs`（reqwest h2 + `jsonwebtoken` ES256） | — |
| `stream-preview.ts`、`development-preview.ts` | `cypher_sync::stream_preview` + `rooms/preview.rs`（UX-2 时生产化） | ✅ codec 复用 |
| `development.ts`、`development-budget.ts` | 无（预算门随 Cloudflare 退役） | — |
| `session-room.ts`、`update-log.ts`、`session-doc/` | 无（410；数据冷归档） | — |
| `install.sh` | 原文件以 `include_str!` 嵌入 | ✅ 原样 |
| DO `alarm` | `alarm.rs`（`alarms` 表 + 调度器） | — |
| R2 | `blobs.rs`、`releases.rs`（S3 客户端 → MinIO + 指针表） | — |

## 附录 B：与现有文档的关系

- `HANDOFF-SELF-HOST-SYNC.md`：其 §6 列出的待测项中，与 Cloudflare 计费相关的（rows/read/write 分类、WS 计费折算、duration）在迁移后不再需要；与网络相关的（各地区到服务器的延迟与断线率）转为 §11 第 1 项；"单用户 rows"数字在 §1.5 折算为 push/秒。
- `docs/rows-written-baseline.md`、`PLAN.md`、`docs/handoff-rows-written-optimization.md`：成本动机消失；2 秒窗口按 §13 UX-1 撤掉；durable outbox 作为可靠性改进保留；P0 回放 fixture 转用作 §12.4 的差分场景。
- `docs/research/durable-objects-language.md`：2026-07 的"DO 留在 TypeScript"决定，其论据（loro-wasm 与 workers-rs 限制）在 chat2/registry 去 wasm 后已失效；`ARCHITECTURE.md` 顶部 "Durable Objects stay TypeScript" 一句随 WP10 作废，本文 §2.1 是新的决定记录。
- `docs/ephemeral-stream-v1.md`：其"正式设备身份"与"生产启用"的遗留边界由 §13.2 接手。
- `docs/notifications.md`：其 "Real-device rollout must separately verify" 清单（`:206-215`）在 WP3/WP9 中执行，同时作为 golden 测试的验收对照。
- `docs/local-edge.md`、`docs/ci-cd.md`、`ARCHITECTURE.md` §1/§6：切换后需更新（WP10）。
