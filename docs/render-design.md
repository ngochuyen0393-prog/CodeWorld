# 渲染模块设计：借鉴寒霜引擎 FrameGraph 架构

> 状态：设计提案（2026-07-23）
> 来源：Frostbite FrameGraph (GDC 2017, Yuriy O'Donnell) + UE5 RDG + Granite + ApoorvaJ Render Graphs
> 目标：将渲染模块从"直接画"升级为"渲染图编排"，保持 CodeWorld 设计哲学

## 一、寒霜引擎渲染架构总结

### 1.1 核心组件分层（2007 -> 2017 演进）

```
2007年（简单）:
  Game Renderer
    └─ World Renderer
         └─ Shading System
              └─ Render Context (GFX API 抽象)
                   └─ Direct3D / libGCM

2017年（FrameGraph 引入后）:
  Game Renderer
    └─ World Renderer（编排器，不分配资源）
         ├─ Shading System（画世界几何）
         ├─ Features（独立模块：Meshes/Terrain/Sky/Particles/Shadows/GI/Post...）
         ├─ Render Context（GFX API 抽象）
         ├─ Frame Graph（新增：渲染操作+资源的有向无环图）
         └─ Transient Resources（新增：单帧临时资源+内存别名）
```

### 1.2 FrameGraph 三阶段模型

寒霜的核心创新。每帧从零构建渲染图，三阶段执行：

**① Setup（声明阶段）**
- 定义所有 render pass（渲染通道）
- 每个 pass 声明输入/输出资源
- 代码流类似即时模式渲染（开发者像写普通渲染代码一样声明）
- 关键：render pass 必须声明所有使用的资源（读/写/创建）

**② Compile（编译阶段）**
- 剔除没有被引用的资源和 pass（子图裁剪）
- 计算资源生命周期（首次使用时分配，最后使用后释放）
- 自动推导资源绑定标志（根据使用方式推导）
- 分配具体 GPU 资源

**③ Execute（执行阶段）**
- 按拓扑序执行每个 pass 的回调函数
- 即时模式渲染代码（SetState/Draw/Dispatch）
- 从 setup 阶段生成的句柄获取真实 GPU 资源

### 1.3 关键设计决策

**每帧从零构建**
- 不是预编译的固定管线，而是每帧动态构建
- 允许条件性增删 pass（debug 视图、动态效果等）
- 代码驱动架构（code-driven），不是数据驱动

**渲染模块化**
- 两种模块类型：
  1. 无状态函数（最常见）：输入输出都是 FrameGraph 资源句柄
  2. 持久模块：有持久资源（LUT/历史缓冲等）
- WorldRenderer 只编排，不分配 GPU 资源
- 代码量从 15K SLOC 降到 5K SLOC

**Blackboard 模块间通信**
- 模块通过 Blackboard（哈希表）共享数据
- 按组件 Type ID 访问
- 允许受控耦合（不直接引用，通过 blackboard 中介）

**瞬态资源系统**
- 生命周期不超过一帧的资源
- 首次使用时分配，最后使用后立即释放
- 内存别名（aliasing）：不重叠生命周期的资源共享同一块物理内存
- 4K 分辨率下从 1042MB 降到 472MB（省 570MB）

**子图裁剪**
- 如果某个 pass 的输出没有被任何 pass 消费，该 pass 和其输出资源被自动剔除
- debug 视图通过连接到 backbuffer 节点来启用，断开则自动裁剪

## 二、其他引擎的同源实现

| 引擎 | 实现名称 | 特点 |
|------|---------|------|
| Unreal Engine 5 | RDG (Render Dependency Graph) | 寒霜模式的直接继承者，C++ lambda 声明 |
| Granite (开源) | Granite Render Graph | Rust 友好的 Vulkan 实现，themaister/granite |
| Bevy | RenderGraph -> Schedule (0.19+) | 从 DAG 演变为 Schedule API |
| ApoorvaJ/graphene | graphene | Rust 实验性实现，三阶段分离 API |

## 三、CodeWorld 现状 vs 寒霜

| 维度 | CodeWorld 现状 | 寒霜做法 |
|------|---------------|---------|
| 渲染流程 | tick 阶段二e：取RenderRequest -> 排序 -> 画 | 三阶段：Setup声明 -> Compile裁剪+分配 -> Execute绘制 |
| 资源管理 | 无（每tick直接画） | 瞬态资源 + 生命周期 + 内存别名 |
| 模块化 | 无（一层排序全画） | 独立渲染模块 + Blackboard 通信 |
| 条件渲染 | 无 | 子图裁剪（输出无人消费则pass被剔除） |
| 图层 | 四层固定优先级 + Y轴排序 | FrameGraph 节点依赖关系决定顺序 |
| 可视化 | 无 | 完整的图可视化+诊断工具 |

## 四、借鉴什么，不借鉴什么

### 借鉴（符合"只提供能力不规定玩法"）

| 借鉴项 | 寒霜来源 | 为什么符合 CodeWorld |
|--------|---------|---------------------|
| 三阶段管线 | FrameGraph Setup/Compile/Execute | 纯架构模式，不增加任何渲染规则 |
| 渲染图（DAG） | FrameGraph 有向无环图 | 把渲染操作组织成依赖图，玩家提交的渲染请求自动排序 |
| 子图裁剪 | FrameGraph 编译阶段裁剪 | 输出无人消费的 pass 自动剔除 = 省 CPU |
| 渲染模块化 | Frostbite Features 模块 | 每个 render pass 是自包含单元，符合"瘦模块"原则 |
| Blackboard | Frostbite 模块间通信 | 渲染模块间通过 blackboard 共享数据，不直接耦合 |
| 资源句柄 | FrameGraph resource handle | 玩家代码拿到的是句柄不是真实资源，安全隔离 |

### 不借鉴（违反 CodeWorld 哲学或 MVP 不需要）

| 不借鉴项 | 原因 |
|---------|------|
| 瞬态资源+内存别名 | GPU 内存优化，MVP 是 ASCII 终端渲染，没有 GPU |
| 异步计算 | 需要 GPU compute queue，MVP 没有 |
| 资源屏障/barrier | Vulkan/DX12 概念，MVP 终端渲染不需要 |
| PSO缓存/根签名 | GPU 管线状态对象，MVP 不需要 |
| 每帧从零构建 | MVP 渲染操作固定（4层排序+画），不需要动态构建 |

## 五、新架构设计

### 5.1 渲染管线（借鉴寒霜三阶段，适配 ASCII 终端）

```
tick 阶段二e（现有）
 └─ renderer.render(render_queue, world)
     │
     ├─ ① Setup（声明阶段）          ← 新增
     │   遍历 render_queue 中所有 RenderRequest
     │   每个 request 声明：
     │   - 输出：一个 RenderOutput（字符+位置+图层）
     │   - 读取：视口信息（可见实体的外观）
     │   - 图层：Background/Entity/Effect/UI
     │   构建 DAG：节点=渲染操作，边=依赖关系
     │
     ├─ ② Compile（编译阶段）         ← 新增
     │   a. 拓扑排序：按图层优先级 + Y轴排序 + priority
     │      Background(0) -> Entity(1) -> Effect(2) -> UI(3)
     │      层内按 y_sort_value 降序（Y大的后画=近的遮远的）
     │      同层同Y按 priority 微调
     │   b. 子图裁剪：输出不在视口内的节点被剔除
     │   c. 分配输出缓冲区位置（屏幕坐标 -> 终端行列）
     │
     └─ ③ Execute（执行阶段）         ← 保留现有绘制逻辑
         按拓扑序遍历 DAG
         每个节点执行：
         - 取 Appearance.glyph + color
         - 写入终端缓冲区对应位置
         - 后写的覆盖先写的（画家算法）
         最终 flush 到终端
```

### 5.2 渲染图数据结构

```rust
/// 渲染图节点（一个渲染操作 = 画一个实体）
struct RenderNode {
    /// 节点ID
    id: RenderNodeId,
    /// 源渲染请求
    request: RenderRequest,
    /// 声明的图层
    layer: RenderLayer,
    /// Y轴排序值
    y_sort: f64,
    /// 同层同Y微调优先级
    priority: i32,
    /// 屏幕坐标（compile 阶段计算）
    screen_pos: Option<(usize, usize)>,
}

/// 渲染图
struct RenderGraph {
    /// 所有节点（按声明顺序）
    nodes: Vec<RenderNode>,
}

impl RenderGraph {
    /// ① Setup：从渲染队列构建图
    fn setup(&mut self, queue: &[RenderRequest], viewport: &Viewport) {
        self.nodes.clear();
        for req in queue {
            self.nodes.push(RenderNode {
                id: RenderNodeId::new(),
                request: req.clone(),
                layer: req.layer,
                y_sort: req.y_sort_value,
                priority: req.priority,
                screen_pos: None, // compile 阶段填
            });
        }
    }

    /// ② Compile：拓扑排序 + 子图裁剪 + 坐标计算
    fn compile(&mut self, viewport: &Viewport) {
        // a. 裁剪：视口外的节点标记为剔除
        self.nodes.retain(|n| {
            viewport.contains(n.request.pos.0, n.request.pos.1)
        });

        // b. 排序：图层 -> Y轴(降序) -> priority
        self.nodes.sort_by(|a, b| {
            a.layer.cmp(&b.layer)               // 图层升序
                .then_with(|| b.y_sort.partial_cmp(&a.y_sort).unwrap_or(Equal)  // Y降序
                .then_with(|| a.priority.cmp(&b.priority))                       // priority升序
        });

        // c. 计算屏幕坐标
        for node in &mut self.nodes {
            node.screen_pos = viewport.world_to_screen(node.request.pos);
        }
    }

    /// ③ Execute：按序写入终端缓冲区
    fn execute(&self, buffer: &mut TerminalBuffer) {
        for node in &self.nodes {
            if let Some((row, col)) = node.screen_pos {
                let glyph = &node.request.appearance.glyph;
                let color = &node.request.appearance.color;
                buffer.write(row, col, glyph, color);
            }
        }
    }
}
```

### 5.3 视口系统（借鉴寒霜 WorldRenderer 的 view 概念）

```rust
/// 视口（世界坐标 -> 屏幕坐标的映射）
struct Viewport {
    /// 视口中心（世界坐标）
    center: (f64, f64),
    /// 视口半宽半高（世界坐标）
    half_width: f64,
    half_height: f64,
    /// 终端尺寸（行列）
    term_rows: usize,
    term_cols: usize,
}

impl Viewport {
    /// 世界坐标是否在视口内
    fn contains(&self, x: f64, y: f64) -> bool {
        x >= self.center.0 - self.half_width
            && x <= self.center.0 + self.half_width
            && y >= self.center.1 - self.half_height
            && y <= self.center.1 + self.half_height
    }

    /// 世界坐标 -> 终端行列
    fn world_to_screen(&self, world: (f64, f64)) -> Option<(usize, usize)> {
        if !self.contains(world.0, world.1) {
            return None;
        }
        let dx = world.0 - (self.center.0 - self.half_width);
        let dy = world.1 - (self.center.1 - self.half_height);
        let col = (dx / (self.half_width * 2.0) * self.term_cols as f64) as usize;
        let row = (dy / (self.half_height * 2.0) * self.term_rows as f64) as usize;
        Some((row.min(self.term_rows - 1), col.min(self.term_cols - 1)))
    }
}
```

### 5.4 终端缓冲区

```rust
/// 终端缓冲区（双缓冲）
struct TerminalBuffer {
    rows: usize,
    cols: usize,
    /// 当前帧缓冲
    current: Vec<Vec<Cell>>,
    /// 上一帧缓冲（diff 用）
    previous: Vec<Vec<Cell>>,
}

/// 单个终端格子
struct Cell {
    glyph: String,
    color: String,
}

impl TerminalBuffer {
    fn new(rows: usize, cols: usize) -> Self { ... }

    /// 写入一个格子（后写覆盖先写 = 画家算法）
    fn write(&mut self, row: usize, col: usize, glyph: &str, color: &str) {
        if row < self.rows && col < self.cols {
            self.current[row][col] = Cell {
                glyph: glyph.to_string(),
                color: color.to_string(),
            };
        }
    }

    /// flush：只重绘变化的格子（diff 优化）
    fn flush(&mut self, output: &mut impl Write) {
        for row in 0..self.rows {
            for col in 0..self.cols {
                if self.current[row][col] != self.previous[row][col] {
                    // 移动光标 + 写字符 + 设置颜色
                    write!(output, "\x1b[{};{}H\x1b[{}m{}", row + 1, col + 1,
                           color_to_ansi(&self.current[row][col].color),
                           self.current[row][col].glyph).unwrap();
                }
            }
        }
        // swap 缓冲
        std::mem::swap(&mut self.current, &mut self.previous);
        // 清空 current 供下一帧使用
        for row in &mut self.current {
            for cell in row.iter_mut() {
                *cell = Cell::default();
            }
        }
    }
}
```

### 5.5 渲染模块化（借鉴寒霜 Features 模块概念）

寒霜把渲染功能拆成独立模块（Meshes/Terrain/Sky/Particles...），通过 blackboard 通信。

CodeWorld MVP 只有一种渲染操作（画 ASCII 字符），但架构预留模块化扩展：

```rust
/// 渲染模块 trait（借鉴寒霜渲染模块概念）
/// MVP 只有一个实现：AsciiRenderModule
/// v2 可以有：SpriteRenderModule / EffectRenderModule / UIRenderModule
trait RenderModule {
    /// Setup：声明本模块的渲染节点
    fn setup(&self, graph: &mut RenderGraph, world: &WorldState, viewport: &Viewport);

    /// 模块名（用于 blackboard 注册）
    fn name(&self) -> &str;
}

/// ASCII 渲染模块（MVP 默认）
struct AsciiRenderModule;

impl RenderModule for AsciiRenderModule {
    fn setup(&self, graph: &mut RenderGraph, world: &WorldState, viewport: &Viewport) {
        for entity in world.entities.values() {
            if !entity.is_alive() || entity.is_sleeping() {
                continue;
            }
            if let Some(req) = entity.render_request() {
                graph.add_node(req);
            }
        }
    }

    fn name(&self) -> &str { "ascii" }
}

/// 渲染器（编排器，借鉴寒霜 WorldRenderer 角色）
struct Renderer {
    modules: Vec<Box<dyn RenderModule>>,
    graph: RenderGraph,
    buffer: TerminalBuffer,
    viewport: Viewport,
}

impl Renderer {
    /// 完整渲染流程（三阶段）
    fn render(&mut self, world: &WorldState) {
        // ① Setup：每个模块声明渲染节点
        self.graph.clear();
        for module in &self.modules {
            module.setup(&mut self.graph, world, &self.viewport);
        }

        // ② Compile：排序 + 裁剪 + 坐标计算
        self.graph.compile(&self.viewport);

        // ③ Execute：写入终端
        self.graph.execute(&mut self.buffer);

        // Flush 到终端
        let stdout = std::io::stdout();
        self.buffer.flush(&mut stdout.lock());
    }
}
```

## 六、模块文件结构

```
crates/render/src/
├── lib.rs              Renderer 编排器 + 三阶段入口
├── graph.rs            RenderGraph + RenderNode（渲染图 DAG）
├── buffer.rs           TerminalBuffer + Cell（终端缓冲区+diff flush）
├── viewport.rs         Viewport（世界坐标<->屏幕坐标映射）
├── module.rs           RenderModule trait + AsciiRenderModule
└── layer.rs            RenderLayer 枚举（从 core 移入或保留引用）
```

## 七、对 core 模块的改动

RenderRequest / RenderLayer / Appearance 已在 core 中定义（架构文档 2.4 节），render crate 直接引用，不需要改 core。

新增需要的 core 类型：

```rust
// crates/core/src/render.rs（如果不存在则新建）
/// 渲染请求ID（用于图节点追踪）
pub struct RenderNodeId(pub Uuid);

/// 视口定义（世界坐标范围）
pub struct Viewport {
    pub center: (f64, f64),
    pub half_width: f64,
    pub half_height: f64,
}
```

## 八、设计哲学对照

| CodeWorld 铁律 | 本设计如何遵守 |
|---------------|--------------|
| 框架只提供能力不规定玩法 | 三阶段管线/渲染图是"能力管道"，不规定画什么 |
| 传统引擎模块胖，CodeWorld模块瘦 | 不引入瞬态资源/GPU内存别名/异步计算，只取架构模式 |
| 能力vs规则 | 图层排序是能力，画什么字符是玩家代码通过Appearance决定 |
| 改=可被attack | 渲染不影响物理/沙箱，不涉及attack |
| 不提前抽象 | RenderModule trait 预留但 MVP 只有一个实现 |
| 渲染只画结果不参与规则 | 三阶段纯渲染，不碰物理/逻辑 |

## 九、实施计划

### MVP（现在做这个）

1. **viewport.rs**：Viewport 世界坐标<->屏幕坐标映射 + 视口裁剪
2. **buffer.rs**：TerminalBuffer 双缓冲 + diff flush（只重绘变化格子）
3. **graph.rs**：RenderGraph 三阶段（Setup/Compile/Execute）+ 拓扑排序
4. **module.rs**：RenderModule trait + AsciiRenderModule（遍历实体提交渲染请求）
5. **lib.rs**：Renderer 编排器（调用模块 -> 编译图 -> 执行绘制 -> flush）

测试策略：
- viewport：世界坐标映射、视口裁剪边界
- buffer：写入覆盖、diff 检测、清空
- graph：三阶段流程、排序正确性（图层/Y轴/priority）
- module：实体遍历、跳过死亡/休眠实体
- 集成：完整 render() 流程，多实体多图层排序后正确输出

### Phase 2（后续升级）

- Blackboard 模块间通信
- 多 RenderModule（Effect/UI）
- 渲染图可视化工具（dot 输出）
- Canvas/WebSocket 渲染后端
