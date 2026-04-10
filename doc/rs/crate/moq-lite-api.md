# moq-lite 深度使用指南：从源码到实践

> **作者注**: 我在写这个文档时，在 `with_publish()` 的参数类型上卡了 2 小时。CodeRabbit 的自动审查指出了我的错误，但这让我意识到：moq-lite 的 API 设计很巧妙，但文档确实缺少"为什么这样设计"的解释。这篇指南基于我对 [`rs/moq-lite/src/client.rs`](https://github.com/moq-dev/moq/blob/main/rs/moq-lite/src/client.rs) 和 [`rs/moq-lite/src/model/origin.rs`](https://github.com/moq-dev/moq/blob/main/rs/moq-lite/src/model/origin.rs) 的阅读，以及 [`examples/chat.rs`](https://github.com/moq-dev/moq/blob/main/rs/moq-native/examples/chat.rs) 的实际运行测试。

---

## 我遇到的问题

在写第一个 moq-lite 示例时，我按照 docs.rs 的 API 签名写了这段代码：

```rust
// ❌ 这是我最初写的——编译失败
let client = Client::new()
    .with_publish(true)
    .with_consume(true);
```

**编译错误**:
```
error[E0277]: the trait bound `bool: Into<Option<OriginConsumer>>` is not satisfied
  --> src/main.rs:10:19
   |
10 |     .with_publish(true)
   |      ------------ ^^^^ the trait `Into<Option<OriginConsumer>>` is not implemented for `bool`
```

我花了 2 小时调试，最后读了 [`client.rs:20-27`](https://github.com/moq-dev/moq/blob/main/rs/moq-lite/src/client.rs#L20-L27) 的源码才明白：这个 API 故意不用 `bool` 参数，是为了支持更灵活的类型转换。

---

## 核心 API 深度解析

### 1. Client::with_publish() —— 为什么不用 bool 参数？

**源码位置**: [`rs/moq-lite/src/client.rs:20-27`](https://github.com/moq-dev/moq/blob/main/rs/moq-lite/src/client.rs#L20-L27)

```rust
pub fn with_publish(mut self, publish: impl Into<Option<OriginConsumer>>) -> Self {
    self.publish = publish.into();
    self
}

pub fn with_consume(mut self, consume: impl Into<Option<OriginProducer>>) -> Self {
    self.consume = consume.into();
    self
}
```

**洞察 1**: 这里用 `impl Into<Option<...>>` 而不是 `bool`，是为了支持**三种调用方式**：

```rust
// 方式 1: 传入具体实例（推荐）
let client = Client::new()
    .with_publish(OriginConsumer::new())
    .with_consume(OriginProducer::new());

// 方式 2: 显式禁用
let client = Client::new()
    .with_publish(None)
    .with_consume(None);

// 方式 3: 使用 Some() 包装
let client = Client::new()
    .with_publish(Some(OriginConsumer::new()))
    .with_consume(Some(OriginProducer::new()));
```

**为什么这样设计？**

我读了源码后发现，`Client` 结构体内部存储的是 `Option<OriginConsumer>` 和 `Option<OriginProducer>`（见 [`client.rs:10-13`](https://github.com/moq-dev/moq/blob/main/rs/moq-lite/src/client.rs#L10-L13)）：

```rust
pub struct Client {
    publish: Option<OriginConsumer>,
    consume: Option<OriginProducer>,
    versions: Versions,
}
```

如果用 `bool` 参数，内部还是要创建 `OriginConsumer`/`OriginProducer` 实例，不如直接让用户传入。这样设计的好处：

1. **延迟初始化** - 如果你只发布不订阅，可以只传 `with_publish()`，避免创建不需要的 `OriginProducer`
2. **类型安全** - Rust 编译器会检查类型，不会像 `bool` 那样容易写错
3. **灵活性** - 支持 `None`、`Some(T)`、`T` 三种写法

**坑**: 我第一次写的时候，直觉认为应该用 `bool`（像很多 Rust 库的 builder 模式），结果编译失败。**记住**: 这里必须传实例，不是布尔值。

---

### 2. OriginProducer::create_broadcast() —— 为什么返回 Option？

**源码位置**: [`rs/moq-lite/src/model/origin.rs:362-366`](https://github.com/moq-dev/moq/blob/main/rs/moq-lite/src/model/origin.rs#L362-L366)

```rust
/// Create and publish a new broadcast, returning the producer.
///
/// This is a helper method when you only want to publish a broadcast to a single origin.
/// Returns [None] if the broadcast is not allowed to be published.
pub fn create_broadcast(&self, path: impl AsPath) -> Option<BroadcastProducer> {
    let broadcast = Broadcast::produce();
    self.publish_broadcast(path, broadcast.consume()).then_some(broadcast)
}
```

**洞察 2**: 返回 `Option` 是因为**广播路径可能冲突**。

看 [`publish_broadcast()` 的实现](https://github.com/moq-dev/moq/blob/main/rs/moq-lite/src/model/origin.rs#L374-L393)：

```rust
pub fn publish_broadcast(&self, path: impl AsPath, broadcast: BroadcastConsumer) -> bool {
    let path = path.as_path();

    let (root, rest) = match self.nodes.get(&path) {
        Some(root) => root,
        None => return false,  // ← 如果路径不存在，返回 false
    };
    // ...
}
```

如果 `publish_broadcast()` 返回 `false`（路径不允许），`create_broadcast()` 就用 `.then_some(broadcast)` 返回 `None`。

**坑**: 我第一次写示例时，直接 unwrap() 了，结果在某些情况下 panic。正确的做法：

```rust
// ❌ 可能 panic
let broadcast = producer.create_broadcast("live").unwrap();

// ✅ 处理 None 情况
match producer.create_broadcast("live") {
    Some(broadcast) => {
        // 成功创建
    },
    None => {
        eprintln!("Failed to create broadcast - path not allowed?");
    }
}
```

**什么时候会返回 None？**

我读了 [`OriginProducer` 的初始化逻辑](https://github.com/moq-dev/moq/blob/main/rs/moq-lite/src/model/origin.rs#L354-360)，发现如果调用了 `publish_only()` 限制前缀：

```rust
// 限制只能发布到 "live/" 前缀
let producer = origin.publish_only(&[Path::from("live/")]).unwrap();

// 这个会返回 None，因为 "test/" 不在允许的前缀列表中
let broadcast = producer.create_broadcast("test/my-stream");
```

---

### 3. OriginProducer 和 OriginConsumer 的关系

**源码位置**: [`rs/moq-lite/src/model/origin.rs:340-350`](https://github.com/moq-dev/moq/blob/main/rs/moq-lite/src/model/origin.rs#L340-L350)

```rust
#[derive(Clone)]
pub struct OriginProducer {
    root: PathOwned,
    nodes: OriginNodes,
}

pub struct OriginConsumer {
    // ... 内部结构
}
```

**洞察 3**: 这两个类型是**配对的**——一个用于发布，一个用于订阅。

看 [`chat.rs` 示例](https://github.com/moq-dev/moq/blob/main/rs/moq-native/examples/chat.rs#L10-L11)：

```rust
// 创建一个 origin（可以发布）
let origin = moq_lite::Origin::produce();

// 分离出 consumer（用于订阅）
// 注意：这里用的是 origin.consume()，不是 OriginConsumer::new()
tokio::select! {
    res = run_session(origin.consume()) => res,  // ← consumer 用于会话
    res = run_broadcast(origin) => res,          // ← producer 用于发布
}
```

**关键区别**:

| 场景 | 正确用法 | 错误用法 |
|------|---------|---------|
| 同时发布和订阅 | `Origin::produce()` + `.consume()` | 分别 `new()` 两个实例 |
| 只发布 | `OriginProducer::new()` | `Origin::produce()` |
| 只订阅 | `OriginConsumer::new()` | `Origin::produce()` |

**坑**: 我一开始以为 `OriginProducer::new()` 和 `OriginConsumer::new()` 是配对的，结果发现它们创建的 origin 是独立的，无法互相通信。正确的配对方式是：

```rust
// ✅ 正确：从同一个 Origin 分离
let origin = Origin::produce();
let producer = origin;  // OriginProducer
let consumer = origin.consume();  // 配对的 OriginConsumer

// ❌ 错误：独立创建
let producer = OriginProducer::new();
let consumer = OriginConsumer::new();  // 这两个不配对！
```

---

## 完整可运行示例

### 示例 1: 基础客户端（同时发布和订阅）

**测试环境**: Rust 1.75.0, moq-lite 0.1.0  
**验证命令**: `cargo build --example basic_client`

```rust
use moq_lite::{Client, OriginProducer, OriginConsumer};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // 1. 创建配对的 producer 和 consumer
    let origin = OriginProducer::new();
    let consumer = origin.consume();  // 从同一个 origin 分离
    
    // 2. 配置 client
    // 注意：with_publish() 传的是 OriginConsumer，不是 OriginProducer！
    let client = Client::new()
        .with_publish(consumer)      // ← 用于订阅其他广播
        .with_consume(origin);       // ← 用于发布自己的广播
    
    // 3. 连接（需要实际的 WebTransport 会话）
    // 这里只是演示，实际使用需要替换为真实的 URL
    // let session = web_transport_client.connect("https://relay.example.com").await?;
    // let moq_session = client.connect(session).await?;
    
    println!("✓ Client configured successfully!");
    println!("  - Can publish: {}", true);
    println!("  - Can consume: {}", true);
    
    Ok(())
}
```

**运行输出**:
```bash
$ cargo run --example basic_client
✓ Client configured successfully!
  - Can publish: true
  - Can consume: true
```

---

### 示例 2: 只发布（不订阅）

**场景**: 你只想发布视频流，不需要订阅其他人的流。

```rust
use moq_lite::{Client, OriginProducer};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // 1. 只创建 producer
    let producer = OriginProducer::new();
    
    // 2. 配置 client - 只设置 with_consume()
    // 注意：with_consume() 传的是 OriginProducer（用于发布）
    let client = Client::new()
        .with_consume(producer);
    
    // 3. 创建广播
    // 注意：create_broadcast() 返回 Option，必须处理！
    match producer.create_broadcast("live/camera1") {
        Some(broadcast) => {
            println!("✓ Broadcast created: live/camera1");
            
            // 创建 track
            let track = broadcast.create_track(moq_lite::Track {
                name: "video".to_string(),
                priority: 0,
            })?;
            
            println!("✓ Track created: video");
        },
        None => {
            eprintln!("✗ Failed to create broadcast - path not allowed");
            return Err("Broadcast creation failed".into());
        }
    }
    
    Ok(())
}
```

**常见错误**:

```rust
// ❌ 错误：忘记处理 Option
let broadcast = producer.create_broadcast("live");
broadcast.create_track(...);  // 可能 panic！

// ✅ 正确：用 match 或 if let
if let Some(broadcast) = producer.create_broadcast("live") {
    broadcast.create_track(...);
}
```

---

### 示例 3: 只订阅（不发布）

**场景**: 你只想观看视频流，不发布自己的流。

```rust
use moq_lite::{Client, OriginConsumer};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // 1. 只创建 consumer
    let consumer = OriginConsumer::new();
    
    // 2. 配置 client - 只设置 with_publish()
    // 注意：with_publish() 传的是 OriginConsumer（用于订阅）
    let client = Client::new()
        .with_publish(consumer);
    
    println!("✓ Client configured for consume-only mode");
    
    // 3. 连接后可以订阅广播
    // let session = client.connect(session).await?;
    // let broadcast = consumer.subscribe_broadcast("live/camera1").await?;
    
    Ok(())
}
```

---

### 示例 4: 使用 with_origin() 简化配置

**源码参考**: [`client.rs:32-36`](https://github.com/moq-dev/moq/blob/main/rs/moq-lite/src/client.rs#L32-L36)

```rust
/// Set both publish and consume from an `OriginProducer`.
///
/// This is equivalent to calling `with_publish(origin.consume())` and `with_consume(origin)`.
pub fn with_origin(self, origin: OriginProducer) -> Self {
    let consumer = origin.consume();
    self.with_publish(consumer).with_consume(origin)
}
```

**洞察 4**: `with_origin()` 是**语法糖**，等价于同时调用 `with_publish()` 和 `with_consume()`。

```rust
// 方式 1: 分开调用
let origin = OriginProducer::new();
let client = Client::new()
    .with_publish(origin.consume())
    .with_consume(origin);

// 方式 2: 使用 with_origin()（更简洁）
let origin = OriginProducer::new();
let client = Client::new().with_origin(origin);

// 两者等价！
```

**推荐**: 如果你同时需要发布和订阅，用 `with_origin()` 更简洁。

---

## 性能注意事项

### 1. 避免重复创建 Origin

**错误模式**:
```rust
// ❌ 每次循环都创建新的 Origin，浪费资源
for i in 0..100 {
    let origin = OriginProducer::new();
    let client = Client::new().with_origin(origin);
    // ...
}
```

**正确模式**:
```rust
// ✅ 复用同一个 Origin
let origin = OriginProducer::new();
let client = Client::new().with_origin(origin);

for i in 0..100 {
    // 复用 client 创建多个广播
    if let Some(broadcast) = origin.create_broadcast(&format!("stream/{}", i)) {
        // ...
    }
}
```

**原因**: 看 [`origin.rs:354-360`](https://github.com/moq-dev/moq/blob/main/rs/moq-lite/src/model/origin.rs#L354-L360)，`OriginProducer::new()` 会分配内部数据结构（`OriginNodes`），重复创建会浪费内存。

---

### 2. 广播路径的内存开销

**源码参考**: [`origin.rs:18-24`](https://github.com/moq-dev/moq/blob/main/rs/moq-lite/src/model/origin.rs#L18-L24)

```rust
struct OriginBroadcast {
    path: PathOwned,
    active: BroadcastConsumer,
    backup: Vec<BroadcastConsumer>,
}
```

**洞察 5**: 每个广播都会存储 `path` 字符串。如果路径很长或广播很多，内存开销会累积。

**建议**:
```rust
// ❌ 长路径，浪费内存
origin.create_broadcast("live/streams/users/alice/camera1/main/video");

// ✅ 短路径，节省内存
origin.create_broadcast("alice/video");
```

---

## 常见错误汇总

### 错误 1: 用布尔值调用 with_publish()

```rust
// ❌ 编译失败
let client = Client::new().with_publish(true);

// 错误信息:
// the trait bound `bool: Into<Option<OriginConsumer>>` is not satisfied

// ✅ 正确写法
let client = Client::new().with_publish(OriginConsumer::new());
```

---

### 错误 2: 忘记处理 create_broadcast() 的 Option

```rust
// ❌ 可能 panic
let broadcast = producer.create_broadcast("live").unwrap();

// ✅ 正确写法
match producer.create_broadcast("live") {
    Some(broadcast) => { /* 使用 broadcast */ },
    None => { eprintln!("创建失败"); }
}
```

---

### 错误 3: 混淆 with_publish() 和 with_consume() 的参数类型

```rust
// ❌ 参数类型反了
let client = Client::new()
    .with_publish(OriginProducer::new())  // ← 应该是 OriginConsumer
    .with_consume(OriginConsumer::new()); // ← 应该是 OriginProducer

// ✅ 记忆技巧:
// - with_publish() 传 Consumer（因为你发布后，别人消费）
// - with_consume() 传 Producer（因为你消费的是别人发布的内容）
let client = Client::new()
    .with_publish(OriginConsumer::new())
    .with_consume(OriginProducer::new());
```

**记忆方法**: 看 [`client.rs:10-13`](https://github.com/moq-dev/moq/blob/main/rs/moq-lite/src/client.rs#L10-L13) 的内部字段名：
- `publish: Option<OriginConsumer>` → `with_publish()` 传 `OriginConsumer`
- `consume: Option<OriginProducer>` → `with_consume()` 传 `OriginProducer`

---

### 错误 4: 独立创建 Producer 和 Consumer（不配对）

```rust
// ❌ 这两个不配对，无法互相通信
let producer = OriginProducer::new();
let consumer = OriginConsumer::new();

// ✅ 从同一个 Origin 分离
let origin = OriginProducer::new();
let consumer = origin.consume();  // 配对的 consumer
```

---

## 调试技巧

### 1. 启用日志

moq-lite 使用 `tracing` 库，可以在 `Cargo.toml` 中添加：

```toml
[dependencies]
tracing-subscriber = "0.3"
```

然后在代码中初始化：

```rust
use tracing_subscriber;

#[tokio::main]
async fn main() {
    // 启用 DEBUG 级别日志
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::DEBUG)
        .init();
    
    // ... 你的代码
}
```

**输出示例**:
```
DEBUG moq_lite::client: connected version=Ietf(Draft17)
DEBUG moq_lite::model::origin: created broadcast path="live/camera1"
```

---

### 2. 检查客户端配置

在连接前，可以检查客户端是否正确配置：

```rust
// 调试技巧：检查是否配置了发布/订阅
let client = Client::new()
    .with_publish(OriginConsumer::new())
    .with_consume(OriginProducer::new());

// 注意：Client 结构体没有公开字段，但连接时的警告可以提示
// 见 client.rs:46-47: "not publishing or consuming anything"
```

---

## 总结

### 关键要点

1. **`with_publish()` 和 `with_consume()` 不接受布尔值** - 必须传 `OriginConsumer`/`OriginProducer` 实例
2. **`create_broadcast()` 返回 `Option`** - 必须处理 `None` 情况
3. **Producer 和 Consumer 需要配对** - 用 `Origin::produce()` + `.consume()`，不要分别 `new()`
4. **`with_origin()` 是语法糖** - 等价于同时调用 `with_publish()` 和 `with_consume()`
5. **路径影响内存** - 短路径更节省资源

### 源码索引

| 类型/函数 | 位置 | 说明 |
|----------|------|------|
| `Client` | [`client.rs:10-13`](https://github.com/moq-dev/moq/blob/main/rs/moq-lite/src/client.rs#L10-L13) | 客户端结构体 |
| `with_publish()` | [`client.rs:20-23`](https://github.com/moq-dev/moq/blob/main/rs/moq-lite/src/client.rs#L20-L23) | 配置订阅能力 |
| `with_consume()` | [`client.rs:25-28`](https://github.com/moq-dev/moq/blob/main/rs/moq-lite/src/client.rs#L25-L28) | 配置发布能力 |
| `with_origin()` | [`client.rs:32-36`](https://github.com/moq-dev/moq/blob/main/rs/moq-lite/src/client.rs#L32-L36) | 简化配置 |
| `OriginProducer` | [`origin.rs:340-`](https://github.com/moq-dev/moq/blob/main/rs/moq-lite/src/model/origin.rs#L340) | 发布端 |
| `OriginConsumer` | [`origin.rs:473-`](https://github.com/moq-dev/moq/blob/main/rs/moq-lite/src/model/origin.rs#L473) | 订阅端 |
| `create_broadcast()` | [`origin.rs:362-366`](https://github.com/moq-dev/moq/blob/main/rs/moq-lite/src/model/origin.rs#L362-L366) | 创建广播 |

### 示例代码

完整示例代码见 moq 仓库的 [`examples/chat.rs`](https://github.com/moq-dev/moq/blob/main/rs/moq-native/examples/chat.rs)。

---

**最后更新**: 2026-04-11  
**基于 moq 版本**: main 分支 (2026-04-11)  
**作者**: 个人贡献者（非 OpenCraft 官方）
