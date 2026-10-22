// D-018 physics 测试区整体替换脚本：1076 行（弹道测试区起点）到文件尾
const fs = require('fs');
const p = 'D:/projects/CodeWorld/.cc-wt-d018/crates/physics/src/default.rs';
const L = fs.readFileSync(p, 'utf8').split('\n');
const idx = L.findIndex(l => l.includes('=== #034 弹道系统'));
if (idx < 0) throw new Error('marker not found');

const head = L.slice(0, idx);
const tail = ['}']; // mod tests 闭合

const body = `    // === D-018 通用弹道：一切有速度实体的物理（#045 弹丸退役后的通用化管线）===
    //
    // 退役前的 step_projectiles（弹丸专属：半隐式欧拉 + CCD 扫掠 + 寿命超时）整段
    // 并入 step 的冲量路径（重力→阻力→极速钳→中点位移→CCD 扫掠截断→落地清零）。
    // 本区测试全部以"dynamic 造物 + 出生初速"同态构造（原 push 弹丸构造 → 造物
    // set_velocity），断言语义保留、数字按中点口径重标（积分器双轨注记见 step：
    // 半隐式轨随弹丸专属管线退役；中点位移 = 匀加速一拍的解析解，弹道手感锚已
    // 按中点口径换算重标——bin solve_launch_vector 镜像同步）。

    use codeworld_core::collision::PROJECTILE_CODE;

    /// 构造旧弹丸实体（历史兼容测试用：D-018 后世界不再产生 PROJECTILE_CODE
    /// 实体，存量走通用路径照飞照落；本构造器服务 find_nearest 排除/step 通用
    /// 路径兼容锚）
    fn make_projectile(id_u: u128, pos: (f64, f64), vel: (f64, f64), created_tick: u64) -> Entity {
        let mut e = Entity::new(
            EntityId(Uuid::from_u128(id_u)),
            codeworld_core::entity::PlayerId(Uuid::from_u128(7)),
            PROJECTILE_CODE.to_string(),
            pos,
            Appearance::new(".".to_string(), "red".to_string(), 1.0),
            created_tick,
            1_000_000,
        );
        e.set_radius(codeworld_core::collision::PROJECTILE_RADIUS);
        e.set_mass(1.0);
        e.set_velocity(vel);
        e
    }

    /// 构造带初速的 dynamic 造物弹（D-018 造物弹时代的标准构造：spawn 第 10/11/12
    /// 参落库后的世界形态——code_hash=creation + motion=1 + radius/mass/velocity）
    fn make_shell(id_u: u128, pos: (f64, f64), vel: (f64, f64), radius: f64, mass: f64) -> Entity {
        let mut e = make_dyn_creation(id_u, pos.0, pos.1);
        e.set_radius(radius);
        e.set_mass(mass);
        e.set_velocity(vel);
        e
    }

    #[test]
    fn drag_law_scales_by_one_minus_drag_over_mass() {
        // 阻力定律（#034 立法第 3 条）：v = v - drag*v/mass（方向不变模衰减，线性模型）
        // mass=1.0, drag=0.05：每拍衰 5%
        let v = crate::default::apply_drag((4.0, -3.0), 1.0, 0.05);
        assert!((v.0 - 3.8).abs() < 1e-9 && (v.1 + 2.85).abs() < 1e-9);
    }

    #[test]
    fn drag_law_heavy_decays_less_light_decays_more() {
        // 质量为分母：重弹衰减少（弹道平直远），轻弹衰减多（短距弯垂）
        let heavy = crate::default::apply_drag((10.0, 0.0), 10.0, 0.05); // 衰 0.5%
        let light = crate::default::apply_drag((10.0, 0.0), 0.5, 0.05); // 衰 10%
        assert!(heavy.0 > 9.9);
        assert!(light.0 < 9.05);
        // 方向都不变（模衰减不是反向）
        assert!(heavy.0 > 0.0 && light.0 > 0.0);
    }

    #[test]
    fn drag_law_zero_clamped_when_decay_exceeds_velocity() {
        // 极端参数（质量低于阻力）：衰减会越过零点——钳到 0，方向永不翻转
        // mass=0.05, drag=0.1：factor = 1-2 = -1，裸公式会反向
        let v = crate::default::apply_drag((4.0, -3.0), 0.05, 0.1);
        assert_eq!(v, (0.0, 0.0), "衰减超过速度模长时钳到零，不反向");
    }

    #[test]
    fn ballistic_shell_parabola_two_ticks() {
        // 造物弹抛物线（原弹丸测试同态改写，数字按中点口径重标）：
        // 固定初速 (4, 9.8) mass 1.0 @ (0,10)：重力→阻力→极速钳→中点位移。
        // tick1: vy=9.8-9.8=0 -> 阻力 x0.95 -> v=(3.8,0)；中点位移=((4+3.8)/2,(9.8+0)/2)
        //        -> pos=(3.9,14.9)
        // tick2: vy=-9.8 -> x0.95 -> v=(3.61,-9.31)；中点位移+=(3.705,-4.655)
        //        -> pos=(7.605,10.245) 仍在飞
        let engine = DefaultPhysicsEngine::new();
        let mut world = WorldState::new();
        let p = make_shell(1, (0.0, 10.0), (4.0, 9.8), 0.15, 1.0);
        let pid = p.id;
        world.entities.insert(pid, p);
        engine.step(&mut world, &HashSet::new(), &[]);
        assert!(near(world.entities.get(&pid).unwrap().pos(), (3.9, 14.9)));
        engine.step(&mut world, &HashSet::new(), &[]);
        assert!(near(world.entities.get(&pid).unwrap().pos(), (7.605, 10.245)));
    }

    #[test]
    fn ballistic_shell_lands_and_stops() {
        // 造物弹落地 = CCD 触地截断（落点贴地形面 ground+r）+ 速度清零。
        // 与退役弹丸的"Landed 事件+消亡"不同：造物弹是造物，落地停驻是物理事实，
        // 消亡归寿命自管（代码自杀/燃料尽，#045 替代链）
        let engine = DefaultPhysicsEngine::new();
        let mut world = WorldState::new();
        let p = make_shell(1, (3.0, 0.2), (1.0, 0.0), 0.15, 1.0);
        let pid = p.id;
        world.entities.insert(pid, p);
        engine.step(&mut world, &HashSet::new(), &[]);
        let shell = world.entities.get(&pid).unwrap();
        assert!(
            (shell.pos().1 - 0.15).abs() < 1e-9,
            "落点应贴地面半径线 y=0.15，实际 {shell:?}"
        );
        assert_eq!(shell.velocity(), (0.0, 0.0), "触地截断速度清零");
        assert!(shell.is_alive(), "造物弹落地停驻不消亡（寿命自管）");
    }

    #[test]
    fn ballistic_shell_hits_body_stops_without_harming() {
        // 造物弹 CCD 撞体 = 位移截断到首触点 + 速度清零 + 存活；目标不动
        //（物理步不动目标，结算归接触通道 on_contact 代码攻——#045 替代链）。
        // 弹道（中点口径）：v0=(1.8,19.6) @ (0,10)：t1 中点位移 (1.755,14.455)；
        // t2 段 (1.755,14.455)->(3.42225,18.87725) 中点附近放目标必撞
        let engine = DefaultPhysicsEngine::new();
        let mut world = WorldState::new();
        let target = make_entity(2, 2.6, 16.7, 0);
        let tid = target.id;
        world.entities.insert(tid, target);
        let p = make_shell(1, (0.0, 10.0), (1.8, 19.6), 0.15, 1.0);
        let pid = p.id;
        world.entities.insert(pid, p);
        engine.step(&mut world, &HashSet::new(), &[]);
        assert!(
            near(world.entities.get(&pid).unwrap().pos(), (1.755, 14.455)),
            "第一拍无接触照常积分"
        );
        engine.step(&mut world, &HashSet::new(), &[]);
        let shell = world.entities.get(&pid).unwrap();
        let (tx, ty) = world.entities.get(&tid).unwrap().pos();
        let dx = shell.pos().0 - tx;
        let dy = shell.pos().1 - ty;
        let dist = (dx * dx + dy * dy).sqrt();
        assert!(
            dist <= 0.65 + 1e-9,
            "撞体截断应停在目标接触窗口（0.65）内，实际距离 {dist}"
        );
        assert_eq!(shell.velocity(), (0.0, 0.0), "撞体截断速度清零");
        assert!(shell.is_alive(), "造物弹撞体停驻不消亡（结算归接触通道）");
        assert!(world.entities.get(&tid).unwrap().is_alive(), "物理步不动目标");
    }

    #[test]
    fn ballistic_shell_muzzle_clearance_no_self_stop() {
        // 发射点外推出膛不撞发射者（投掷器模板第一课的教学锚）：出生点沿发射
        // 方向前移 (r_self + r_bullet + 0.1)（发射者 r0.5 + 弹 r0.15 + 余量 0.1
        // = 外推 0.75），首拍路径向外飞离——CCD 不与发射者截断（旧弹丸的 shooter
        // 豁免退役：造物弹无特权，外推就是全部答案）
        let engine = DefaultPhysicsEngine::new();
        let mut world = WorldState::new();
        let shooter = make_entity(9, 10.0, 10.0, 0);
        let sid = shooter.id;
        world.entities.insert(sid, shooter);
        let p = make_shell(1, (10.75, 10.0), (2.0, 0.0), 0.15, 1.0);
        let pid = p.id;
        world.entities.insert(pid, p);
        engine.step(&mut world, &HashSet::new(), &[]);
        let shell = world.entities.get(&pid).unwrap();
        assert!(
            shell.pos().0 > 10.75,
            "出膛即飞离发射者（无自截断），实际 {:?}",
            shell.pos()
        );
        assert!(shell.is_alive(), "出膛弹存活");
    }

    #[test]
    fn ballistic_shell_range_calibration_default_drag() {
        // 射程调参判据（阻力默认值的标定锚，中点口径重标）：默认参数（drag=0.05）
        // 下，标准上抛造物弹（vx=4, vy=+9.8, mass=1.0 地面发射）水平射程。
        // 退役前弹丸半隐式口径 ≈3.9 格；中点位移（=匀加速解析解，重力响应提前
        // 半拍）口径实测 ≈7.67 格——锚区间 [7.0, 8.5]。
        // 手算（factor 0.95）：t1 mid=(3.9,5.4)；t2 mid+=(3.705,-4.655)->(7.605,0.745)；
        // t3 vy=(-9.31-9.8)*0.95=-18.1545，路径触地 t=(0.745-0.5)/13.732=0.0178，
        // 落点 x=7.605+3.51975*0.0178≈7.668
        let engine = DefaultPhysicsEngine::new();
        let mut world = WorldState::new();
        let x0 = 50.0; // 远离边界，排除 viewport 干扰
        let p = make_shell(1, (x0, 0.5), (4.0, 9.8), 0.5, 1.0);
        let pid = p.id;
        world.entities.insert(pid, p);
        let mut range = None;
        for _ in 0..60 {
            engine.step(&mut world, &HashSet::new(), &[]);
            let shell = world.entities.get(&pid).unwrap();
            if shell.velocity() == (0.0, 0.0) && shell.pos().1 <= 0.5 + 1e-9 {
                range = Some(shell.pos().0 - x0);
                break;
            }
        }
        let range = range.expect("60 拍内必落地");
        assert!(
            (7.0..=8.5).contains(&range),
            "标准上抛造物弹射程应 7~8.5 格（中点口径，精确 ≈7.67），实际 {range}"
        );
    }

    fn near(a: (f64, f64), b: (f64, f64)) -> bool {
        (a.0 - b.0).abs() < 1e-9 && (a.1 - b.1).abs() < 1e-9
    }

    // === 世界极速·飞行中钳制（#034 修订一，双审 P1-A 修复；D-018 通用化到造物弹）===

    fn zero_drag_engine() -> DefaultPhysicsEngine {
        // 无阻力引擎：隔离重力累积效应（阻力会抵消重力，掩盖钳制是否生效）
        DefaultPhysicsEngine::new_with_params(DefaultPhysics {
            drag_coefficient: 0.0,
            ..DefaultPhysics::default_values()
        })
    }

    #[test]
    fn shell_flight_speed_capped_at_world_max_speed() {
        // P1-A 复现锚（造物弹版）：mass=100 自由落体（阻力 0 可忽略），重力逐拍
        // 累积；速度模长封顶于世界极速 25
        let engine = zero_drag_engine();
        let mut world = WorldState::new();
        let p = make_shell(1, (50.0, 5000.0), (0.0, 0.0), 0.15, 100.0);
        let pid = p.id;
        world.entities.insert(pid, p);
        let mut peak = 0.0f64;
        for _ in 0..29 {
            engine.step(&mut world, &HashSet::new(), &[]);
            let (vx, vy) = world.entities.get(&pid).unwrap().velocity();
            let speed = (vx * vx + vy * vy).sqrt();
            peak = peak.max(speed);
            assert!(
                speed <= 25.0 + 1e-9,
                "速度应封顶世界极速 25，实际 {speed}"
            );
        }
        assert!(
            peak > 24.0,
            "重力应把速度推到接近极速（证明钳制真实在工作而非速度没涨），峰值 {peak}"
        );
    }

    #[test]
    fn shell_flight_displacement_per_tick_capped() {
        // 钳制在位移之前（重力+阻力后、位移前）：一拍位移不超过极速——
        // 视觉可读性常数的世界保证：任何飞行物一拍最多走 25 格
        let engine = zero_drag_engine();
        let mut world = WorldState::new();
        let p = make_shell(1, (50.0, 5000.0), (0.0, 0.0), 0.15, 100.0);
        let pid = p.id;
        world.entities.insert(pid, p);
        for _ in 0..29 {
            let before = world.entities.get(&pid).unwrap().pos();
            engine.step(&mut world, &HashSet::new(), &[]);
            let after = world.entities.get(&pid).unwrap().pos();
            let moved = ((after.0 - before.0).powi(2) + (after.1 - before.1).powi(2)).sqrt();
            assert!(
                moved <= 25.0 + 1e-9,
                "单拍位移应不超过极速 25，实际 {moved}"
            );
        }
    }

    #[test]
    fn shell_at_exact_max_speed_untouched() {
        // 边界：速度恰等于极速不钳（严格 > 判据）：无重力无阻力下 (25,0) 分毫不动
        let engine = DefaultPhysicsEngine::new_with_params(DefaultPhysics {
            gravity: 0.0,
            drag_coefficient: 0.0,
            ..DefaultPhysics::default_values()
        });
        let mut world = WorldState::new();
        let p = make_shell(1, (0.0, 10.0), (25.0, 0.0), 0.15, 1.0);
        let pid = p.id;
        world.entities.insert(pid, p);
        engine.step(&mut world, &HashSet::new(), &[]);
        assert_eq!(
            world.entities.get(&pid).unwrap().velocity(),
            (25.0, 0.0),
            "恰等于极速的速度不应被改动"
        );
        assert_eq!(
            world.entities.get(&pid).unwrap().pos(),
            (25.0, 10.0),
            "一拍位移恰为极速 25 格（中点=v0 因速度无变化）"
        );
    }

    // === CCD 路径检查（D-018 通用化：一切有速度实体的线段扫掠截断）===

    fn straight_engine() -> DefaultPhysicsEngine {
        // 直线弹道引擎（零重力零阻力）：CCD 几何断言不受抛物线干扰
        DefaultPhysicsEngine::new_with_params(DefaultPhysics {
            gravity: 0.0,
            drag_coefficient: 0.0,
            ..DefaultPhysics::default_values()
        })
    }

    #[test]
    fn ccd_sweep_truncates_midway_target_point_check_would_miss() {
        // CCD 锚定用例（原弹丸测试同态改写）：极速内高速弹一拍跨 20 格、目标在
        // 路径中点——位移截断停在首触点（旧点检查会隧穿；CCD 纯几何必拦）
        // 截断点 = (0 + 20*(10-0.65)/20, 20) = (9.35, 20)
        let engine = straight_engine();
        let mut world = WorldState::new();
        let target = make_entity(2, 10.0, 20.0, 0);
        let tid = target.id;
        world.entities.insert(tid, target);
        let p = make_shell(1, (0.0, 20.0), (20.0, 0.0), 0.15, 1.0);
        let pid = p.id;
        world.entities.insert(pid, p);
        engine.step(&mut world, &HashSet::new(), &[]);
        let shell = world.entities.get(&pid).unwrap();
        assert!(
            near(shell.pos(), (9.35, 20.0)),
            "应停在首触点 (9.35,20)，实际 {:?}",
            shell.pos()
        );
        assert_eq!(shell.velocity(), (0.0, 0.0), "撞体截断速度清零");
        assert!(shell.is_alive(), "造物弹截断停驻不消亡");
        assert!(world.entities.get(&tid).unwrap().is_alive(), "物理步不动目标");
    }

    #[test]
    fn ccd_sweep_multi_target_first_contact_wins() {
        // 多目标取线段参数 t 最小者（最先碰到）：路径上 x=5 与 x=15 两个目标 →
        // 截断在近目标首触点 (5-0.65, 20) = (4.35, 20)
        let engine = straight_engine();
        let mut world = WorldState::new();
        let far = make_entity(2, 15.0, 20.0, 0);
        let near_target = make_entity(3, 5.0, 20.0, 0);
        let fid = far.id;
        let nid = near_target.id;
        world.entities.insert(fid, far);
        world.entities.insert(nid, near_target);
        let p = make_shell(1, (0.0, 20.0), (20.0, 0.0), 0.15, 1.0);
        let pid = p.id;
        world.entities.insert(pid, p);
        engine.step(&mut world, &HashSet::new(), &[]);
        let shell = world.entities.get(&pid).unwrap();
        assert!(
            near(shell.pos(), (4.35, 20.0)),
            "应停在最先碰到的（t 最小）目标首触点，实际 {:?}",
            shell.pos()
        );
        let _ = nid;
    }

    #[test]
    fn ccd_landing_truncates_at_ground_crossing() {
        // 落地=路径与地面线的交点（CCD 触地截断，原弹丸同语义迁移）：
        // (0,1) 直线 vel(20,-2) 一拍末 (20,-1)，地面交点 t=(1-0.15)/2=0.425
        // → 落点 (8.5, 0.15)。旧"穿地后拉回拍末 x"口径在退役前已修，此处锚定
        // 通用路径延续该口径
        let engine = straight_engine();
        let mut world = WorldState::new();
        let p = make_shell(1, (0.0, 1.0), (20.0, -2.0), 0.15, 1.0);
        let pid = p.id;
        world.entities.insert(pid, p);
        engine.step(&mut world, &HashSet::new(), &[]);
        let shell = world.entities.get(&pid).unwrap();
        assert!(near(shell.pos(), (8.5, 0.15)), "落点应为地面交点 (8.5,0.15)，实际 {:?}", shell.pos());
        assert_eq!(shell.velocity(), (0.0, 0.0), "触地截断速度清零");
        assert!(shell.is_alive(), "造物弹落地停驻（尸体坐标=落点，异态不留世界）");
    }

    #[test]
    fn ccd_body_hit_before_ground_wins_by_contact_t() {
        // 同拍路径既穿身体又触地：按 t 定序（真实先后，不再靠检查顺序）。
        // (0,0.3) vel(10,-1)：地面 t=0.15（x=1.5）；身体 (1,0.29) r0.5 首触
        // t≈0.0925 → 撞体先：截断点在身体接触窗口内且未到地面
        let engine = straight_engine();
        let mut world = WorldState::new();
        let target = make_entity(2, 1.0, 0.29, 0);
        let tid = target.id;
        world.entities.insert(tid, target);
        let p = make_shell(1, (0.0, 0.3), (10.0, -1.0), 0.15, 1.0);
        let pid = p.id;
        world.entities.insert(pid, p);
        engine.step(&mut world, &HashSet::new(), &[]);
        let shell = world.entities.get(&pid).unwrap();
        let (tx, ty) = world.entities.get(&tid).unwrap().pos();
        let dist = ((shell.pos().0 - tx).powi(2) + (shell.pos().1 - ty).powi(2)).sqrt();
        assert!(
            dist <= 0.65 + 1e-9,
            "应撞体截断（先触身体按 t 定序），实际距目标 {dist}"
        );
        assert!(
            shell.pos().1 > 0.15,
            "撞体截断先于触地（未落地面线），实际 {:?}",
            shell.pos()
        );
        assert_eq!(shell.velocity(), (0.0, 0.0), "截断速度清零");
    }

    // ===== #044 子步宪法：确定性测试锚（D-015 法典化；D-018 通用化到 step 冲量路径）=====
    //
    // 宪法要求（decisions #044）：N=世界状态的纯函数——同状态必然同 N 同结算；
    // 红线=绝不用墙钟/CPU/帧率选 N。CCD 扫掠并入 step 冲量路径后，锚形态不变：
    // 同状态两跑 step 末态逐位一致。

    #[test]
    fn ballistic_step_same_state_same_result() {
        // #044 纯函数锚：同一初始状态（实体集合+速度+造物弹 CCD 截断混合场景）
        // 跑两遍 step——末态（坐标/速度/存亡）与物理变更序列必须逐位一致。
        // 场景：一条平射造物弹撞中途目标（CCD 撞体截断）+ 一条上抛造物弹落地
        //（CCD 触地截断），覆盖两个截断分支。
        let engine = DefaultPhysicsEngine::new();
        let run_world = || -> (Vec<_>, WorldState) {
            let mut world = WorldState::new();
            for (id, pos, vel) in [
                (1u128, (0.0, 20.0), (12.0, 9.8)),  // 平射→撞 (6,20) 目标
                (2u128, (30.0, 1.0), (3.0, 6.0)),   // 上抛→落地
            ] {
                let s = make_shell(id, pos, vel, 0.15, 1.0);
                world.entities.insert(s.id, s);
            }
            for (id, pos) in [(3u128, (6.0, 20.0)), (4u128, (50.0, 20.0))] {
                let t = make_entity(id, pos.0, pos.1, 0);
                world.entities.insert(t.id, t);
            }
            let changes = engine.step(&mut world, &HashSet::new(), &[]);
            (changes, world)
        };
        let (ch_a, wa) = run_world();
        let (ch_b, wb) = run_world();
        assert_eq!(ch_a, ch_b, "同状态必然同物理变更序列（#044 纯函数要求）");
        let mut ids: Vec<EntityId> = wa.entities.keys().copied().collect();
        ids.sort_by_key(|i| i.0);
        for id in ids {
            let (a, b) = (wa.entities.get(&id).unwrap(), wb.entities.get(&id).unwrap());
            assert_eq!(a.is_alive(), b.is_alive(), "存亡必须一致");
            assert_eq!(a.pos(), b.pos(), "坐标必须逐位一致（#044）");
            assert_eq!(a.velocity(), b.velocity(), "速度必须逐位一致（#044）");
        }
        // 场景自检：两条造物弹都应被截断（速度清零），锚不空转
        for id in [1u128, 2u128] {
            let sid = EntityId(Uuid::from_u128(id));
            assert_eq!(
                wa.entities.get(&sid).unwrap().velocity(),
                (0.0, 0.0),
                "造物弹 {id} 应被 CCD 截断（撞体/触地其一）"
            );
        }
    }

    #[test]
    fn ballistic_step_result_independent_of_hashmap_iteration_order() {
        // #044 机器数据红线锚（对照组）：同一场景构造序反过来——先插目标后插
        // 造物弹、id 收集后统一排序——结算必须与正序构造完全一致。
        // step 内部对实体按 EntityId 升序定序（确定序），CCD 目标扫描同序：
        // 结算与 HashMap 遍历序无关，即"N 的决定规则只读世界状态"在实现层成立。
        let engine = DefaultPhysicsEngine::new();
        let run = |insert_targets_first: bool| -> Vec<(EntityId, (f64, f64), (f64, f64), bool)> {
            let mut world = WorldState::new();
            let targets: Vec<Entity> = [(3u128, 6.0, 20.0), (4u128, 50.0, 20.0)]
                .into_iter()
                .map(|(id, x, y)| make_entity(id, x, y, 0))
                .collect();
            let shells: Vec<Entity> = [
                (1u128, (0.0, 20.0), (12.0, 9.8)),
                (2u128, (30.0, 1.0), (3.0, 6.0)),
            ]
            .into_iter()
            .map(|(id, pos, vel)| make_shell(id, pos, vel, 0.15, 1.0))
            .collect();
            if insert_targets_first {
                for e in targets {
                    world.entities.insert(e.id, e);
                }
                for e in shells {
                    world.entities.insert(e.id, e);
                }
            } else {
                for e in shells {
                    world.entities.insert(e.id, e);
                }
                for e in targets {
                    world.entities.insert(e.id, e);
                }
            }
            engine.step(&mut world, &HashSet::new(), &[]);
            let mut fin: Vec<(EntityId, (f64, f64), (f64, f64), bool)> = world
                .entities
                .iter()
                .map(|(id, e)| (*id, e.pos(), e.velocity(), e.is_alive()))
                .collect();
            fin.sort_by_key(|(id, _, _, _)| id.0);
            fin
        };
        let fin1 = run(false);
        let fin2 = run(true);
        assert_eq!(fin1, fin2, "末态与构造序无关（EntityId 定序，#044：结算只读状态）");
    }

    #[test]
    fn ccd_sweep_check_pure_geometry_no_wall_clock() {
        // #044 红线锚：CCD 扫掠判据=纯几何——同一（位置，速度）对在极速内
        // 取任意两档速度，截断判定只由几何决定且单调（路径穿过目标必截断，
        // 与跑多少次/何时跑无关）。高速 20 格/拍与慢速 4 格/拍都截断在目标
        // 接触窗口（旧点检查在 20 格/拍会漏——隧穿）。
        let engine = straight_engine();
        for speed in [4.0f64, 20.0f64] {
            let mut world = WorldState::new();
            let target = make_entity(2, 2.0, 20.0, 0);
            let tid = target.id;
            world.entities.insert(tid, target);
            let p = make_shell(1, (0.0, 20.0), (speed, 0.0), 0.15, 1.0);
            let pid = p.id;
            world.entities.insert(pid, p);
            engine.step(&mut world, &HashSet::new(), &[]);
            let shell = world.entities.get(&pid).unwrap();
            let (tx, _) = world.entities.get(&tid).unwrap().pos();
            let dist = (shell.pos().0 - tx).abs();
            assert!(
                dist <= 0.65 + 1e-9 && shell.pos().1 == 20.0,
                "速度 {speed} 格/拍：路径上的目标必截断（CCD 纯几何判据），实际 {dist}"
            );
        }
    }

    // === 历史弹丸实体兼容（D-018：PROJECTILE_CODE 实体退役后走通用路径）===

    #[test]
    fn step_legacy_projectile_now_uses_generic_ballistic_path() {
        // 兼容锚：旧 PROJECTILE_CODE 实体（历史存档，新世界不再产生）不再被
        // step 跳过——velocity≠0 走冲量路径照飞（弹道物理通用化覆盖一切实体）。
        // 排除语义保留在各消费点：find_hit_target 不可锁定/detect_touching 不接触/
        // CCD 目标过滤不撞弹丸
        let engine = DefaultPhysicsEngine::new();
        let mut world = WorldState::new();
        let p = make_projectile(1, (0.0, 10.0), (3.0, 0.0), 0);
        let pid = p.id;
        world.entities.insert(pid, p);
        engine.step(&mut world, &HashSet::new(), &[]);
        let (vx, vy) = world.entities.get(&pid).unwrap().velocity();
        assert!(
            (vx - 3.0 * 0.95).abs() < 1e-9 && (vy + 9.8 * 0.95).abs() < 1e-9,
            "旧弹丸实体应走通用冲量路径（重力+阻力），实际 ({vx},{vy})"
        );
        assert!(
            world.entities.get(&pid).unwrap().pos().0 > 0.0,
            "位置应随速度积分位移"
        );
    }

    // === D-016 地形起伏：物理四件套③（落地钳制/CCD 落点查表）===

    #[test]
    fn d016_step_entity_lands_on_hill() {
        // D-016：生物落在山地上——落点=地形面（ground(x)+贴面），不再钳 y=0 半径线
        let engine = DefaultPhysicsEngine::new();
        let mut world = WorldState::new();
        world.set_terrain(codeworld_core::terrain::Terrain::new(
            codeworld_core::terrain::TerrainPreset::Mountain,
            8,
        ));
        // 找最高点，从其上空 10 格垂直下落
        let t = &world.terrain;
        let (peak_i, &peak_h) = t
            .samples()
            .iter()
            .enumerate()
            .max_by(|a, b| a.1.partial_cmp(b.1).unwrap())
            .unwrap();
        let peak_x = codeworld_core::terrain::TERRAIN_X_MIN
            + peak_i as f64 * codeworld_core::terrain::TERRAIN_SPACING;
        let e = make_entity(1, peak_x, peak_h + 10.0, 0);
        let id = e.id;
        world.entities.insert(id, e);
        // 多拍下落到稳定（落点解析：落点=max(ground(x), r)——山顶尖上圆心=peak_h+r）
        for _ in 0..5 {
            engine.step(&mut world, &HashSet::new(), &[]);
        }
        let final_y = world.entities.get(&id).unwrap().pos().1;
        let expect = peak_h.max(0.5);
        assert!(
            (final_y - expect).abs() < 1e-9,
            "峰顶落点应为 ground+半径（{expect}），实际 {final_y}"
        );
    }

    #[test]
    fn d016_step_entity_rests_in_valley() {
        // D-016：坑负全合法——谷底生物落点=谷底地面（y=0 退位为基准线）
        let engine = DefaultPhysicsEngine::new();
        let mut world = WorldState::new();
        world.set_terrain(codeworld_core::terrain::Terrain::new(
            codeworld_core::terrain::TerrainPreset::Mountain,
            8,
        ));
        let t = &world.terrain;
        let (valley_i, &valley_h) = t
            .samples()
            .iter()
            .min_by(|a, b| a.1.partial_cmp(b.1).unwrap())
            .unwrap();
        let valley_x = codeworld_core::terrain::TERRAIN_X_MIN
            + valley_i as f64 * codeworld_core::terrain::TERRAIN_SPACING;
        assert!(valley_h < 0.0, "山地应有负高度坑（海平面语义），实际 {valley_h}");
        let e = make_entity(1, valley_x, valley_h + 5.0, 0);
        let id = e.id;
        world.entities.insert(id, e);
        for _ in 0..5 {
            engine.step(&mut world, &HashSet::new(), &[]);
        }
        let final_y = world.entities.get(&id).unwrap().pos().1;
        let expect = valley_h.max(0.5);
        assert!(
            (final_y - expect).abs() < 1e-9,
            "谷底落点应为 ground（{expect}），实际 {final_y}"
        );
    }

    #[test]
    fn d016_flat_terrain_physics_zero_regression() {
        // D-016 零回归锚：平地地形（全零数组）下物理行为与旧常数地面逐位一致
        let engine = DefaultPhysicsEngine::new();
        let mut world = WorldState::new(); // 默认 terrain=flat
        let e = make_entity(1, 5.0, 10.0, 0);
        let id = e.id;
        world.entities.insert(id, e);
        engine.step(&mut world, &HashSet::new(), &[]);
        assert_eq!(
            world.entities.get(&id).unwrap().pos(),
            (5.0, 0.5),
            "平地落点=半径线（旧口径零回归）"
        );
    }

    #[test]
    fn d016_shell_lands_on_mountain_slope() {
        // D-016 + D-018：造物弹 CCD 落点查表——撞山坡的弹截断贴地形面（y=ground+r）
        let engine = DefaultPhysicsEngine::new();
        let mut world = WorldState::new();
        world.set_terrain(codeworld_core::terrain::Terrain::new(
            codeworld_core::terrain::TerrainPreset::Mountain,
            8,
        ));
        let t = &world.terrain;
        let (peak_i, &peak_h) = t
            .samples()
            .iter()
            .enumerate()
            .max_by(|a, b| a.1.partial_cmp(b.1).unwrap())
            .unwrap();
        let peak_x = codeworld_core::terrain::TERRAIN_X_MIN
            + peak_i as f64 * codeworld_core::terrain::TERRAIN_SPACING;
        // 从峰左侧高处向右下斜射（必撞上山坡）
        let start = (peak_x - 4.0, peak_h + 6.0);
        let p = make_shell(1, start, (10.0, -12.0), 0.15, 1.0);
        let pid = p.id;
        let r = p.radius();
        world.entities.insert(pid, p);
        let mut landed = None;
        for _ in 0..30 {
            engine.step(&mut world, &HashSet::new(), &[]);
            let shell = world.entities.get(&pid).unwrap();
            if shell.velocity() == (0.0, 0.0) {
                landed = Some(shell.pos());
                break;
            }
        }
        let (lx, ly) = landed.expect("30 拍内必撞坡停驻");
        let ground = world.terrain.height(lx);
        assert!(
            (ly - ground - r).abs() < 1e-6,
            "停点弹心应贴地形面（ground+{r}），实际 ({lx},{ly}) ground={ground}"
        );
        assert!(lx < peak_x + 1.0, "应在山坡上停驻（未过峰），实际 lx={lx}");
    }

    #[test]
    fn d016_terrain_ground_over_matches_all_breakpoint_peak_reference() {
        // D-016 R84 复审返工锚：分段线性地面在区间内的最大值必落在 0.25 格断点
        // 或区间端点上——xa/中点/xb 三点采样漏检内部断点峰值（复审口径实测
        // 最大差 0.1741 格；本测试的 Hills/Mountain×seed 0..49 全扫描口径实测
        // 最大差 0.375 格=Mountain max_delta，static 造物 AABB 底边可悬空到
        // 该量级）。参考=直接扫高度数组的全断点独立实现，不调被测函数。
        use codeworld_core::terrain::{Terrain, TerrainPreset, TERRAIN_SPACING, TERRAIN_X_MIN};
        let all_breakpoint_peak = |t: &Terrain, xa: f64, xb: f64| -> f64 {
            let mut peak = t.height(xa).max(t.height(xb));
            for (i, &h) in t.samples().iter().enumerate() {
                let x = TERRAIN_X_MIN + i as f64 * TERRAIN_SPACING;
                if x >= xa && x <= xb {
                    peak = peak.max(h);
                }
            }
            peak
        };
        let mut world = WorldState::new();
        // 复审同口径全扫描：Hills/Mountain × seed 0..49 × 全部格对齐 1 格区间
        //（与 creation_aabb 生产口径一致：[floor(x), floor(x)+1]）
        let mut max_diff = 0.0f64;
        let mut worst_miss = 0.0f64;
        for preset in [TerrainPreset::Hills, TerrainPreset::Mountain] {
            for seed in 0..50u64 {
                world.set_terrain(Terrain::new(preset, seed));
                let t = &world.terrain;
                let n = t.sample_count();
                for i in 0..n - 4 {
                    let xa = TERRAIN_X_MIN + i as f64 * TERRAIN_SPACING;
                    let xb = xa + 1.0;
                    let exact = all_breakpoint_peak(t, xa, xb);
                    // 三点口径（旧实现）：内部断点成峰时必然低估
                    let three_pt = t
                        .height(xa)
                        .max(t.height((xa + xb) / 2.0))
                        .max(t.height(xb));
                    worst_miss = worst_miss.max(exact - three_pt);
                    let got = terrain_ground_over(&world, xa, xb);
                    max_diff = max_diff.max(exact - got);
                }
            }
        }
        // 负面场景真实性自检：扫描范围内确有内部断点成峰且被三点口径漏检
        //（复审实测 0.1741）——防锚测试扫到无峰地形变空转
        assert!(
            worst_miss > 0.1,
            "前提自检失败：扫描应存在内部断点峰值被三点口径漏检（实测 {worst_miss}）"
        );
        assert!(
            max_diff < 1e-12,
            "terrain_ground_over 应=全断点精确峰值（最大差 {max_diff}）"
        );
        // 边界：区间内无断点的亚格窄区间——极值只能在两端点（区间外断点
        // 不计入，计入即高估），与全断点参考仍须逐位一致
        world.set_terrain(Terrain::new(TerrainPreset::Mountain, 8));
        for k in 0..400i64 {
            let xa = TERRAIN_X_MIN + (k as f64 + 0.1) * TERRAIN_SPACING;
            let xb = xa + 0.1 * TERRAIN_SPACING; // 宽 0.025 格，恒在单一格内
            let exact = all_breakpoint_peak(&world.terrain, xa, xb);
            let got = terrain_ground_over(&world, xa, xb);
            assert!(
                (got - exact).abs() < 1e-12,
                "亚格窄区间 k={k} 应=精确峰值：got={got} exact={exact}"
            );
        }
    }

    #[test]
    fn d016_creation_lands_on_terrain_no_float() {
        // D-016：static 造物落在起伏地形上不悬空——落地稳定后，AABB 底边
        // 不高于占地格内地形峰值（塔脚总有着落点），且多拍稳定不抖动
        let engine = DefaultPhysicsEngine::new();
        let mut world = WorldState::new();
        world.set_terrain(codeworld_core::terrain::Terrain::new(
            codeworld_core::terrain::TerrainPreset::Hills,
            11,
        ));
        // 在有起伏处放造物（找一个左右高差 >0.3 的 x：占地格跨坡）
        let t = &world.terrain;
        let mut spot = None;
        for i in 1..t.sample_count() - 5 {
            let x = codeworld_core::terrain::TERRAIN_X_MIN
                + i as f64 * codeworld_core::terrain::TERRAIN_SPACING;
            let g0 = t.height(x);
            let g1 = t.height(x + 1.0);
            if (g1 - g0).abs() > 0.3 {
                spot = Some(x);
                break;
            }
        }
        let x = spot.expect("丘陵 0.25 采样必有跨坡格");
        let c = make_creation(1, x, 8.0);
        let cid = c.id;
        world.entities.insert(cid, c);
        let mut last = None;
        for _ in 0..8 {
            engine.step(&mut world, &HashSet::new(), &[]);
            last = Some(world.entities.get(&cid).unwrap().pos().1);
        }
        let final_y = last.unwrap();
        // 落地稳定（连续两拍同位）
        engine.step(&mut world, &HashSet::new(), &[]);
        let stable_y = world.entities.get(&cid).unwrap().pos().1;
        assert_eq!(final_y, stable_y, "造物落地应稳定不抖动");
        // 不悬空：AABB 底边 floor(y) 不高于占地格地形峰值
        //（分层锚定注记：此处 ground_peak 由被测函数 terrain_ground_over
        // 计算；其精确性的独立参考锚由
        // d016_terrain_ground_over_matches_all_breakpoint_peak_reference 覆盖）
        let (min, max) = creation_aabb((x, final_y));
        let ground_peak = terrain_ground_over(&world, min.0, max.0);
        assert!(
            min.1 <= ground_peak + 1e-9,
            "AABB 底边 {min_1} 应不高于地形峰值 {ground_peak}（塔脚有着落）",
            min_1 = min.1
        );
        assert!(final_y <= 0.0 + 1e-9 || min.1 >= ground_peak - 1.0 - 1e-9);
    }

    // === D-010a 冲量小刀：带速度实体的速度积分路径（跳跃弹道锚） ===

    /// 判断浮点近似相等（弹道数值断言用；半格/格级锚点用 1e-9 即可）
    fn approx(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-9
    }

    #[test]
    fn thrust_ballistic_jump_peak_within_three_grid_anchor() {
        // 跳跃锚点（D-010a Gate）：地面生物带冲量速度 (0, 7.7) 进 physics.step，
        // 拍 1 升空高度 ≈3 格（±0.5 容差）；次拍落地贴半径线、速度清零。
        // 积分次序锚定：冲量入速度（阶段二 thrust 应用）→ 重力入速度 → 中点位移
        // → 落地清零。中点位移 = v0 + g/2 = 7.7 - 4.9 = 2.8，落在锚区间内。
        // 【积分器双轨】此锚是 step 冲量路径保留中点位移的历史证据——
        // D-018 通用化后本路径就是世界唯一弹道管线，锚继续生效。
        let engine = DefaultPhysicsEngine::new();
        let mut world = WorldState::new();
        let mut e = make_entity(1, 0.0, 0.5, 0); // r=0.5 站半径线（地面 y=0）
        let id = e.id;
        e.set_velocity((0.0, 7.7)); // 模拟阶段二 thrust 应用后的速度字段
        world.entities.insert(id, e);
        // 拍 1：升空
        engine.step(&mut world, &HashSet::new(), &[]);
        let e = world.entities.get(&id).unwrap();
        let peak_height = e.pos().1 - 0.5; // 相对地面高度
        assert!(
            (2.5..=3.5).contains(&peak_height),
            "跳跃最高点应 ≈3 格（±0.5 容差），实测 {peak_height}"
        );
        assert!(
            approx(e.velocity().1, (7.7 - 9.8) * 0.95) && approx(e.velocity().0, 0.0),
            "拍 1 重力入速度后吃阻力（(7.7-9.8)×0.95），实测 {:?}",
            e.velocity()
        );
        // 拍 2：落地清零
        engine.step(&mut world, &HashSet::new(), &[]);
        let e = world.entities.get(&id).unwrap();
        assert!(
            approx(e.pos().1, 0.5) && approx(e.pos().0, 0.0),
            "落地应贴半径线 (0, 0.5)，实测 {:?}",
            e.pos()
        );
        assert_eq!(e.velocity(), (0.0, 0.0), "落地速度清零（vy 清零规则）");
    }

    #[test]
    fn ballistic_midair_accumulates_gravity_no_teleport() {
        // 空中不清（vy 清零规则另一半）：拍 1 升空后拍 2 空中 vy 继续累积重力
        //（-2.1 → -11.9）；全程逐拍位移 ≤ 世界极速 25（非瞬移）。
        let engine = DefaultPhysicsEngine::new();
        let mut world = WorldState::new();
        let mut e = make_entity(1, 0.0, 0.5, 0);
        let id = e.id;
        e.set_velocity((0.0, 7.7));
        world.entities.insert(id, e);
        // 拍 1
        engine.step(&mut world, &HashSet::new(), &[]);
        let (p0, v0) = {
            let e = world.entities.get(&id).unwrap();
            (e.pos(), e.velocity())
        };
        assert!(approx(v0.1, -1.995), "拍 1 vy 应为 (7.7-9.8)×0.95，实测 {v0:?}");
        // 拍 2（空中）：vy 累积 = -2.1 + -9.8 = -11.9
        engine.step(&mut world, &HashSet::new(), &[]);
        let (p1, v1) = {
            let e = world.entities.get(&id).unwrap();
            (e.pos(), e.velocity())
        };
        assert!(
            p1.1 < p0.1,
            "拍 2 在下落（弧线回落），实测 p0={p0:?} p1={p1:?}"
        );
        // 落地拍位移（中点 = -2.1 + -9.8)/2 = -7.0）与起跳拍位移（+2.8）都 ≤ 极速 25
        assert!(
            (p1.1 - p0.1).abs() <= 25.0 && (p0.1 - 0.5).abs() <= 25.0,
            "逐拍位移不得超过世界极速（非瞬移锚）"
        );
        assert!(
            approx(v1.0, 0.0) && approx(v1.1, 0.0),
            "拍 2 应已落地清零，实测 {v1:?}"
        );
    }

    #[test]
    fn ballistic_zero_velocity_entities_keep_positional_fall_regime() {
        // 回归锚：速度恒 (0,0) 的实体走原落点制（circle_landing_y，一拍落 9.8），
        // 不进冲量路径——空中静止实体（悬停丢失后自由落体）现状行为零变化。
        let engine = DefaultPhysicsEngine::new();
        let mut world = WorldState::new();
        let e = make_entity(1, 0.0, 5.0, 0);
        let id = e.id;
        world.entities.insert(id, e);
        engine.step(&mut world, &HashSet::new(), &[]);
        let e = world.entities.get(&id).unwrap();
        // 落点制：fall_to = 5.0 - 9.8 = -4.8 < 地面 0.5 → 落地半径线
        assert!(
            approx(e.pos().1, 0.5),
            "零速实体应走落点制落地，实测 {:?}",
            e.pos()
        );
        assert_eq!(e.velocity(), (0.0, 0.0), "落点制不动速度字段");
    }

    #[test]
    fn ballistic_dynamic_creation_with_velocity_now_integrates() {
        // D-018③ 先行半步：dynamic 造物出生初速此前只存不动（位置走落点制），
        // 冲量路径起造物带速度也走速度积分（被撞飞/抛射的地基）。
        let engine = DefaultPhysicsEngine::new();
        let mut world = WorldState::new();
        let mut c = make_dyn_creation(1, 0.0, 0.5);
        let id = c.id;
        c.set_velocity((7.7, 7.7));
        world.entities.insert(id, c);
        engine.step(&mut world, &HashSet::new(), &[]);
        let c = world.entities.get(&id).unwrap();
        assert!(
            c.pos().0 > 0.5,
            "dynamic 造物带速度应横向位移（速度积分生效），实测 {:?}",
            c.pos()
        );
    }

    // === D-012 力场：物理步接入（域内实体按改后参数被结算）===

    #[test]
    fn step_entity_in_zero_gravity_field_does_not_fall() {
        // 场影响生物落点：零重力场（scale=0）域内生物本拍不落（重力被场改写为 0）；
        // 对照组域外同况实体照常下落
        let engine = DefaultPhysicsEngine::new();
        let mut world = WorldState::new();
        let mut in_field = make_entity(1, 5.0, 10.0, 0);
        in_field.set_velocity((0.5, 0.0)); // 微速度走冲量路径（重力在速度上累积）
        let mut outside = make_entity(2, 50.0, 10.0, 0);
        outside.set_velocity((0.5, 0.0));
        let id_in = in_field.id;
        let id_out = outside.id;
        world.entities.insert(id_in, in_field);
        world.entities.insert(id_out, outside);
        let fields = [crate::fields::FieldEffect {
            source_id: EntityId(Uuid::from_u128(9)),
            pos: (5.0, 10.0),
            radius: 3.0,
            gravity_scale: 0.0,
            drag_delta: 0.0,
        }];
        engine.step(&mut world, &HashSet::new(), &fields);
        let (vx_in, vy_in) = world.entities.get(&id_in).unwrap().velocity();
        assert!(
            vy_in.abs() < 1e-12,
            "零重力场域内速度 vy 不应累积重力，实际 {vy_in}"
        );
        let (_, vy_out) = world.entities.get(&id_out).unwrap().velocity();
        assert!(
            (vy_out - (0.0 - 9.8) * 0.95).abs() < 1e-9,
            "域外实体照常吃世界重力（阻力后 -9.31），实际 {vy_out}"
        );
        let _ = vx_in;
    }

    #[test]
    fn step_entity_outside_field_unaffected_vs_baseline() {
        // 域外实体不受影响锚：与无场基线逐位一致（无场世界行为不变）
        let engine = DefaultPhysicsEngine::new();
        let mut base = WorldState::new();
        let mut plain = make_entity(1, 5.0, 10.0, 0);
        plain.set_velocity((1.0, 0.0));
        let id_p = plain.id;
        base.entities.insert(id_p, plain);
        engine.step(&mut base, &HashSet::new(), &[]);
        // 场世界：同一实体挪到远处的场外（域判定只看距离，同引擎同参数）
        let mut fworld = WorldState::new();
        let mut shadow = make_entity(1, 5.0, 10.0, 0);
        shadow.set_velocity((1.0, 0.0));
        let id_s = shadow.id;
        fworld.entities.insert(id_s, shadow);
        let fields = [crate::fields::FieldEffect {
            source_id: EntityId(Uuid::from_u128(9)),
            pos: (40.0, 40.0),
            radius: 2.0,
            gravity_scale: 0.0,
            drag_delta: 2.0,
        }];
        engine.step(&mut fworld, &HashSet::new(), &fields);
        assert_eq!(
            base.entities.get(&id_p).unwrap().pos(),
            fworld.entities.get(&id_s).unwrap().pos(),
            "域外实体位置应与无场基线逐位一致"
        );
        assert_eq!(
            base.entities.get(&id_p).unwrap().velocity(),
            fworld.entities.get(&id_s).unwrap().velocity(),
            "域外实体速度应与无场基线逐位一致"
        );
    }

    #[test]
    fn step_shell_in_zero_gravity_field_keeps_straight_trajectory() {
        // 弹道场第一消费者锚（D-018 造物弹版）：零重力场域内造物弹不吃重力
        //（vy 不累积，直线飞行），域外对照造物弹照常下坠——护弹道场的直接作用
        let engine = DefaultPhysicsEngine::new();
        let mut world = WorldState::new();
        let in_shell = make_shell(1, (5.0, 10.0), (3.0, 0.0), 0.15, 1.0);
        let out_shell = make_shell(2, (60.0, 10.0), (3.0, 0.0), 0.15, 1.0);
        let id_in = in_shell.id;
        let id_out = out_shell.id;
        world.entities.insert(id_in, in_shell);
        world.entities.insert(id_out, out_shell);
        let fields = [crate::fields::FieldEffect {
            source_id: EntityId(Uuid::from_u128(9)),
            pos: (5.0, 10.0),
            radius: 3.0,
            gravity_scale: 0.0,
            drag_delta: 0.0,
        }];
        engine.step(&mut world, &HashSet::new(), &fields);
        assert!(
            (world.entities.get(&id_in).unwrap().velocity().1).abs() < 1e-12,
            "域内造物弹 vy 不累积（零重力场）"
        );
        let vy_out = world.entities.get(&id_out).unwrap().velocity().1;
        assert!(
            (vy_out + 9.8 * 0.95).abs() < 1e-12,
            "域外造物弹照常吃重力（阻力后 -9.8×0.95），实际 {vy_out}"
        );
    }
`;

fs.writeFileSync(p, [...head, body, ...tail].join('\n'));
console.log('replaced from line', idx + 1, '; new total lines', head.length + body.split('\n').length + 1);
