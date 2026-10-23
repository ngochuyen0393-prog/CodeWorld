# 物理引擎 V2 设计：借鉴 PhysX 5 & Chaos 架构

> 状态：设计提案（2026-07-23）
> 目标：将物理模块从"玩具级"升级为"工业级"，保持 CodeWorld 设计哲学

## 一、现状问题

### 当前物理模块（crates/physics，28测试）的硬伤

| 问题 | 现状 | 工业引擎做法 |
|------|------|-------------|
| 碰撞检测 | 坐标完全相等才算碰撞 | AABB 重叠 -> 形状精确相交 |
| 碰撞解决 | ~~后创建实体挤到 x+1~~ ✅ D-010 已落地（2026-10-23）：CCD 截断处冲量解算=法向反弹（e=组合 max）+切向摩擦（f=组合 √，简化库仑）——#021 切割声明（撞完就分非持续推挤），已退役的"挤到 x+1"不再回来 | 冲量/位置投影解算 |
| 性能 | O(n²) 双重循环遍历所有实体对 | 宽相加速结构（SAP/AABB树/空间哈希）|
| 管线 | 一坨 step() 全干完 | 分阶段：broadphase -> narrowphase -> island -> solve -> integrate |
| 数据布局 | HashMap<EntityId, Entity> | SOA（Structure of Arrays），SIMD 友好 |
| 持久接触 | 无，每帧重新检测 | PCM（Persistent Contact Manifold），跨帧保持接触点 |
| 休眠 | 无，每 tick 全跑 | 速度阈值检测 + 唤醒机制 |

## 二、借鉴什么，不借鉴什么

### 借鉴（符合"只提供能力不规定玩法"）

| 借鉴项 | 来源 | 为什么符合 CodeWorld |
|--------|------|---------------------|
| 管线分阶段 | PhysX ScPipeline | 把一坨拆成步骤，不增加任何规则，只明确"能力在哪个阶段生效" |
| 宽相加速结构 | PhysX BpBroadPhase SAP/AABB树 | 纯优化，更快找到候选对，不改碰撞判定逻辑 |
| 碰撞形状 | PhysX GuGeometryQuery / Chaos Sphere/Box/Capsule | 框架提供形状定义（能力），怎么用是玩家的事（规则）|
| AABB 包围盒 | 两个引擎都有 | 所有实体有 AABB，宽相用 AABB 做初筛，窄相再做精确检测 |
| 休眠机制 | PhysX DySleep / Chaos 材料阈值 | 纯优化，不动的实体不计算，被碰撞时唤醒 |
| 持久接触流形 | PhysX GuPersistentContactManifold | 跨帧保持接触点，省大量计算，不改碰撞规则 |
| 岛管理 | PhysX IG::IslandSim | 连通分量分组，岛间可并行。CodeWorld 的覆盖规则冲突域 = 岛 |

### 不借鉴（违反 CodeWorld 哲学）

| 不借鉴项 | 来源 | 为什么不借 |
|---------|------|-----------|
| PGS/TGS 求解器 | PhysX DyPGS/DyTGS | 这是"怎么求解物理"的具体算法 = 规则。CodeWorld 的 last-write-wins 就是自己的求解器 |
| PBD 求解器 | Chaos FPBDRigidsEvolutionGBF | 同上，PBD 是另一种求解方法 = 规则 |
| GPU/CUDA | PhysX 500+ CUDA kernel | CodeWorld 跑 WASM 沙箱，规模不大，不需要 GPU |
| 关节/铰链约束 | PhysX DyArticulation / Chaos FConstraint | 具体物理规则，应该是玩家代码实现 |
| 破碎/布料/流体 | Chaos FClusterUnionManager / Cloth / PBD粒子 | 玩家代码的事，框架不内置 |
| 序列化/PVD | PhysX Pvd / OmniPvd | 调试工具，MVP 不需要 |

## 三、新架构设计

### 3.1 物理管线（借鉴 PhysX ScPipeline 分阶段模式）

```
tick 阶段二（现有）
 └─ physics.step(world)
     │
     ├─ ① 宽相 BroadPhase            ← 新增
     │   输入：所有实体的 AABB
     │   输出：AABB 重叠的候选对列表
     │   算法：Sweep and Prune（SAP）
     │   能力：快速排除不可能碰撞的对
     │
     ├─ ② 窄相 NarrowPhase           ← 新增
     │   输入：候选对 + 实体形状
     │   输出：接触对（ContactPair，含接触点/穿透深度/法线）
     │   算法：按形状组合分派（Circle-Circle / Circle-Rect / Rect-Rect）
     │   能力：精确判定是否真的碰撞 + 接触信息
     │
     ├─ ③ 默认规则 DefaultRules      ← 保留，从 step() 中拆出
     │   重力 -> 地面约束 -> 速度限制
     │   能力：框架自带的厚版本物理
     │
     ├─ ④ 覆盖规则 OverrideRules     ← 保留，增加阶段参数
     │   按优先级排序执行 WASM 规则
     │   能力：玩家自定义物理规则
     │
     ├─ ⑤ 冲突解决 ConflictResolve   ← 保留，不变
     │   last-write-wins
     │   能力：多规则冲突时的合并策略
     │
     ├─ ⑥ 碰撞解决 CollisionResolve  ← 新增，从 step() 中拆出升级
     │   输入：接触对
     │   输出：位置修正（基于 AABB 最小平移向量，不再 x+1）
     │   能力：解决穿透，把实体推开
     │
     ├─ ⑦ 岛分组 IslandGen           ← 新增（Phase 2）
     │   按接触关系分组，岛间可并行
     │   能力：为多线程求解做准备
     │
     └─ ⑧ 积分 Integrate             ← 新增，明确分离
         应用最终 PhysicsChange 到实体坐标
         能力：把物理计算结果写入世界状态
```

### 3.2 碰撞形状系统（借鉴 PhysX Gu + Chaos 形状定义）

```rust
/// 碰撞形状（框架提供，复用 Appearance 的 shape 类型）
/// 借鉴 PhysX GuGeometryQuery 的形状分派模式
enum ColliderShape {
    /// 圆形（半径，世界单位）
    Circle { radius: f64 },
    /// 矩形（宽高，世界单位）
    Rect { width: f64, height: f64 },
    /// 点（无体积，MVP 默认，等价于旧的坐标相等碰撞）
    Point,
}

/// 实体的碰撞体（绑定到 Entity 上）
struct Collider {
    shape: ColliderShape,
    /// AABB 缓存（每 tick 更新，宽相用）
    aabb: AABB,
}

/// AABB 轴对齐包围盒
#[derive(Debug, Clone, Copy, PartialEq)]
struct AABB {
    min: (f64, f64),
    max: (f64, f64),
}

impl AABB {
    /// 两个 AABB 是否重叠（宽相判定）
    fn overlaps(&self, other: &AABB) -> bool {
        self.min.0 <= other.max.0
            && self.max.0 >= other.min.0
            && self.min.1 <= other.max.1
            && self.max.1 >= other.min.1
    }

    /// 从形状和位置计算 AABB
    fn from_shape(shape: &ColliderShape, pos: (f64, f64)) -> Self {
        match shape {
            ColliderShape::Circle { radius } => AABB {
                min: (pos.0 - radius, pos.1 - radius),
                max: (pos.0 + radius, pos.1 + radius),
            },
            ColliderShape::Rect { width, height } => AABB {
                min: (pos.0 - width / 2.0, pos.1 - height / 2.0),
                max: (pos.0 + width / 2.0, pos.1 + height / 2.0),
            },
            ColliderShape::Point => AABB {
                min: pos,
                max: pos,
            },
        }
    }
}
```

### 3.3 窄相碰撞检测（借鉴 PhysX GuContact 分派模式）

PhysX 的窄相是按形状对分派的：GuContactSphereSphere.cpp、GuContactBoxBox.cpp 等。CodeWorld 用 trait + match 实现同样的分派：

```rust
/// 接触对（窄相输出）
struct ContactPair {
    a: EntityId,
    b: EntityId,
    /// 接触点
    contact_point: (f64, f64),
    /// 穿透深度
    penetration: f64,
    /// 最小平移向量（MTV，把 a 推出 b 的最小位移）
    mtv: (f64, f64),
}

/// 窄相碰撞检测器
struct NarrowPhase;

impl NarrowPhase {
    /// 检测两个碰撞体是否真的相交
    /// 按形状组合分派（借鉴 PhysX GuContact* 分派模式）
    fn detect(a: &Collider, b: &Collider) -> Option<ContactPair> {
        use ColliderShape::*;
        match (&a.shape, &b.shape) {
            (Circle { .. }, Circle { .. }) => Self::circle_circle(a, b),
            (Circle { .. }, Rect { .. }) => Self::circle_rect(a, b),
            (Rect { .. }, Circle { .. }) => Self::rect_circle(a, b),
            (Rect { .. }, Rect { .. }) => Self::rect_rect(a, b),
            (Point, Point) => Self::point_point(a, b),
            (Point, Circle { .. }) => Self::point_circle(a, b),
            (Circle { .. }, Point) => Self::circle_point(a, b),
            (Point, Rect { .. }) => Self::point_rect(a, b),
            (Rect { .. }, Point) => Self::rect_point(a, b),
        }
    }

    fn circle_circle(a: &Collider, b: &Collider) -> Option<ContactPair> { ... }
    fn circle_rect(a: &Collider, b: &Collider) -> Option<ContactPair> { ... }
    // ... 每种组合一个函数，借鉴 PhysX GuContact*.cpp 的分离实现
}
```

### 3.4 宽相加速结构（借鉴 PhysX BpBroadPhaseSap）

PhysX 有三种宽相算法（SAP/MBP/ABP）。CodeWorld MVP 用 SAP（Sweep and Prune），最简单且对小规模场景足够：

```rust
/// 宽相：Sweep and Prune
/// 借鉴 PhysX BpBroadPhaseSap.cpp
struct BroadPhase {
    /// 按 X 轴排序的实体 ID 列表（每 tick 重新排序）
    sorted_x: Vec<EntityId>,
}

impl BroadPhase {
    /// 找出所有 AABB 重叠的候选对
    fn find_pairs(&mut self, colliders: &HashMap<EntityId, Collider>) -> Vec<(EntityId, EntityId)> {
        // 1. 按 AABB.min.x 排序
        self.sorted_x.sort_by_key(|id| {
            colliders.get(id).map(|c| c.aabb.min.0).unwrap_or(f64::MAX)
        });

        // 2. 扫描：对每个实体，只检查 X 轴上重叠的后续实体
        let mut pairs = Vec::new();
        for i in 0..self.sorted_x.len() {
            let id_a = self.sorted_x[i];
            let aabb_a = match colliders.get(&id_a) { Some(c) => &c.aabb, None => continue };

            for j in (i + 1)..self.sorted_x.len() {
                let id_b = self.sorted_x[j];
                let aabb_b = match colliders.get(&id_b) { Some(c) => &c.aabb, None => continue };

                // X 轴不重叠则跳出（已排序，后续更远）
                if aabb_a.max.0 < aabb_b.min.0 {
                    break;
                }

                // X 轴重叠，检查 Y 轴
                if aabb_a.overlaps(aabb_b) {
                    pairs.push((id_a, id_b));
                }
            }
        }
        pairs
    }
}
```

复杂度：排序 O(n log n) + 扫描 O(n + k)（k=重叠对数），远优于 O(n²)。

### 3.5 碰撞解决（借鉴 PhysX 接触求解 + MTV）

现在的"挤到 x+1"太粗糙。借鉴 PhysX 的最小平移向量（MTV）思路：

```rust
/// 碰撞解决：用窄相算出的 MTV 把实体推开
/// 借鉴 PhysX PxsContactManager 的位置修正
struct CollisionResolver;

impl CollisionResolver {
    fn resolve(
        contacts: Vec<ContactPair>,
        entities: &mut HashMap<EntityId, Entity>,
    ) -> Vec<PhysicsChange> {
        let mut changes = Vec::new();
        for contact in contacts {
            // MVP 策略：后创建的实体被推开 MTV 的一半
            // （Phase 2：按质量比分配推力，借鉴 PhysX SolverBody 的逆质量）
            let to_move = Self::pick_mover(&contact, entities);
            if let Some(entity) = entities.get_mut(&to_move) {
                let (x, y) = entity.pos();
                let mtv = contact.mtv;
                let new_pos = (x + mtv.0, y + mtv.1);
                entity.set_pos(new_pos);
                changes.push(PhysicsChange::new(
                    to_move, new_pos, PhysicsReason::Collision, None,
                ));
            }
        }
        changes
    }
}
```

### 3.6 休眠机制（借鉴 PhysX DySleep）

```rust
/// 休眠管理器
/// 借鉴 PhysX DySleep.cpp 的速度阈值 + 计数器模式
struct SleepManager {
    /// 休眠阈值：速度低于此值开始计休眠计数
    sleep_threshold: f64,
    /// 需要连续多少 tick 低速才休眠
    sleep_ticks: u32,
    /// 每个实体的低速计数
    low_speed_counters: HashMap<EntityId, u32>,
}

impl SleepManager {
    /// 检查并更新休眠状态
    fn update(&mut self, entities: &mut HashMap<EntityId, Entity>) {
        for entity in entities.values_mut() {
            if !entity.is_alive() {
                continue;
            }
            let speed = entity.velocity().0.hypot(entity.velocity().1);
            if speed < self.sleep_threshold {
                let counter = self.low_speed_counters.entry(entity.id).or_insert(0);
                *counter += 1;
                if *counter >= self.sleep_ticks {
                    entity.set_sleeping(true);
                }
            } else {
                self.low_speed_counters.insert(entity.id, 0);
                entity.set_sleeping(false);
            }
        }
    }

    /// 被碰撞时唤醒
    fn wake_up(&mut self, entity: &mut Entity) {
        entity.set_sleeping(false);
        self.low_speed_counters.insert(entity.id, 0);
    }
}
```

### 3.7 持久接触流形（借鉴 PhysX GuPersistentContactManifold）

```rust
/// 持久接触流形：跨帧保持接触点
/// 借鉴 PhysX GuPersistentContactManifold.h
struct ContactManifold {
    /// 接触对（实体A, 实体B）
    pair: (EntityId, EntityId),
    /// 上次的接触信息
    last_contact: Option<ContactPair>,
    /// 连续接触的 tick 数（用于稳定性判断）
    contact_ticks: u32,
}

/// 接触缓存：跨帧保持
/// key = (min(EntityId), max(EntityId)) 保证确定性
struct ContactCache {
    manifolds: HashMap<(EntityId, EntityId), ContactManifold>,
}

impl ContactCache {
    /// 更新接触缓存，返回当前帧的活跃接触
    fn update(&mut self, current_contacts: Vec<ContactPair>) -> Vec<ContactPair> {
        // 1. 更新已有流形
        // 2. 添加新流形
        // 3. 移除不再接触的流形
        // 4. 返回活跃接触列表
    }
}
```

## 四、模块文件结构

### 4.1 新的 crates/physics/src/ 结构

```
crates/physics/src/
├── lib.rs              管线编排（PhysicsPipeline）
├── broadphase.rs       宽相：SAP 候选对查找        ← 新增
├── narrowphase.rs      窄相：形状精确碰撞检测       ← 新增
├── collider.rs         碰撞形状 + AABB 定义         ← 新增
├── default.rs          默认规则：重力/地面/速度     ← 保留，去掉碰撞部分
├── override.rs         覆盖规则管理                ← 保留，增加阶段参数
├── conflict.rs         冲突解决：last-write-wins   ← 保留，不变
├── collision.rs        碰撞解决：MTV 位置修正       ← 新增（从 default.rs 拆出）
├── sleep.rs            休眠管理                    ← 新增
├── manifold.rs         持久接触流形                ← 新增（Phase 2）
└── island.rs           岛分组                     ← 新增（Phase 2）
```

### 4.2 lib.rs 管线编排

```rust
/// 物理管线（借鉴 PhysX ScPipeline 分阶段编排）
pub struct PhysicsPipeline {
    broadphase: BroadPhase,
    narrowphase: NarrowPhase,
    default_engine: DefaultPhysicsEngine,
    override_manager: OverrideManager,
    conflict_resolver: ConflictResolver,
    collision_resolver: CollisionResolver,
    sleep_manager: SleepManager,
    // Phase 2:
    // contact_cache: ContactCache,
    // island_gen: IslandGenerator,
}

impl PhysicsPipeline {
    pub fn step(&mut self, world: &mut WorldState) -> Vec<PhysicsChange> {
        let mut changes = Vec::new();

        // ① 更新所有碰撞体的 AABB
        let colliders = self.build_colliders(world);

        // ② 宽相：找候选对
        let candidate_pairs = self.broadphase.find_pairs(&colliders);

        // ③ 窄相：精确碰撞检测
        let contacts = self.narrowphase.detect_all(&candidate_pairs, &colliders);

        // ④ 默认规则：重力 -> 地面 -> 速度限制（跳过休眠实体）
        for entity in world.entities.values_mut() {
            if entity.is_sleeping() { continue; }
            if let Some(c) = self.default_engine.apply_gravity(entity) { changes.push(c); }
            if let Some(c) = self.default_engine.apply_ground_constraint(entity) { changes.push(c); }
            self.default_engine.apply_velocity_limit(entity);
        }

        // ⑤ 覆盖规则
        let override_changes = self.run_overrides(world);
        changes.extend(override_changes);

        // ⑥ 冲突解决
        changes = self.conflict_resolver.resolve(changes);

        // ⑦ 碰撞解决（用 MTV 代替 x+1）
        let collision_changes = self.collision_resolver.resolve(contacts, &mut world.entities);
        changes.extend(collision_changes);

        // ⑧ 休眠更新
        self.sleep_manager.update(&mut world.entities);

        // ⑨ 唤醒被碰撞的实体
        for contact in &contacts {
            if let Some(e) = world.entities.get_mut(&contact.a) { self.sleep_manager.wake_up(e); }
            if let Some(e) = world.entities.get_mut(&contact.b) { self.sleep_manager.wake_up(e); }
        }

        changes
    }
}
```

## 五、对 core 模块的改动

需要在 Entity 上增加字段：

```rust
// crates/core/src/entity.rs 新增
impl Entity {
    // 已有：pos, velocity, alive, created_tick ...

    // 新增
    pub fn collider_shape(&self) -> &ColliderShape;
    pub fn set_collider_shape(&mut self, shape: ColliderShape);
    pub fn is_sleeping(&self) -> bool;
    pub fn set_sleeping(&mut self, sleeping: bool);
}
```

```rust
// crates/core/src/physics.rs 新增
/// 碰撞形状（从 physics crate 移到 core，因为 Entity 需要持有）
pub enum ColliderShape {
    Circle { radius: f64 },
    Rect { width: f64, height: f64 },
    Point,
}

/// AABB
pub struct AABB {
    pub min: (f64, f64),
    pub max: (f64, f64),
}

/// 接触对（窄相输出，碰撞解决输入）
pub struct ContactPair {
    pub a: EntityId,
    pub b: EntityId,
    pub contact_point: (f64, f64),
    pub penetration: f64,
    pub mtv: (f64, f64),
}
```

## 六、实施计划

### Phase 2A：管线拆分 + 形状系统（先做这个）

不影响现有 28 个测试的断言，只重构内部结构：

1. **新建 collider.rs**：ColliderShape + AABB + overlaps()
2. **新建 broadphase.rs**：SAP find_pairs()
3. **新建 narrowphase.rs**：按形状分派的 detect()
4. **新建 collision.rs**：从 default.rs 拆出碰撞解决，用 MTV 代替 x+1
5. **改 default.rs**：去掉碰撞检测和解决，只留重力/地面/速度
6. **新建 lib.rs PhysicsPipeline**：编排上述模块
7. **改 core/entity.rs**：增加 collider_shape + sleeping 字段
8. **改 core/physics.rs**：增加 ColliderShape + AABB + ContactPair

测试策略：
- 现有 28 个测试全部保留，断言不变
- 新增 collider/broadphase/narrowphase 各自的单元测试
- Point 形状等价于旧的"坐标相等"，保证向后兼容

### Phase 2B：休眠 + 持久接触（后做）

1. 新建 sleep.rs
2. 新建 manifold.rs
3. 管线集成

### Phase 2C：岛分组（最后做）

1. 新建 island.rs
2. 为多线程求解做准备

## 七、设计哲学对照

| CodeWorld 铁律 | 本设计如何遵守 |
|---------------|--------------|
| 框架只提供能力不规定玩法 | 宽相/窄相/碰撞形状是"能力"，怎么用是玩家的事 |
| 传统引擎模块胖，CodeWorld模块瘦 | 不引入 PGS/TGS/PBD 求解器，保持 last-write-wins |
| 能力vs规则 | 碰撞形状定义=能力，碰撞解决策略=规则（可被覆盖） |
| 改=可被attack | 物理覆盖规则仍然是 WASM 沙箱里跑，可被 probe/attack |
| 不提前抽象 | 岛分组标注 Phase 2C，MVP 不做 |
| 存在性原语只提供能力 | 物理管线各阶段是"能力管道"，不规定任何游戏数值 |
