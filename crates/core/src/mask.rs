//! D-017 头号需求「生图即角色」先行刀：PNG 角色图 -> 128x128 位图掩码
//!
//! 三步管线（dev-plan D-017 收图三护栏）：PNG 解码 -> alpha 阈值化（原图分辨率）
//! -> area-average 降采样到 128x128 布尔表；连通性校验（4-连通）独立函数。
//!
//! 能力 vs 规则（项目宪法）：本模块是世界能力（把图抄成实/空对照表），
//! 不解释图像内容——掩码只是一张"哪格实心"的表，怎么用归玩家/后续刀
//! （D-013 挂形状物理；本卡掩码只进快照供渲染调试）。
//!
//! 尺寸哲学：「恰等放行」——边长 1024 放行、1025 拒绝；小于 128 的图
//! 忠实放大（最近邻语义，放大不丢信息），128 恰等 1:1。

/// 掩码表边长（物理/渲染表定案尺寸，dev-plan D-017"如 128×128"）
pub const MASK_SIZE: usize = 128;

/// alpha 阈值：> 127（≈>50%）算实心。50% 界由 127.5 拆开——
/// alpha=128 起实心，127 及以下空心（恰 50% 边界测试锚 ALPHA=128/127）
pub const MASK_ALPHA_THRESHOLD: u8 = 127;

/// 降采样阈值：输出格对应原图块内实心占比 **严格大于** 0.5 才算实心
/// （与 MASK_ALPHA_THRESHOLD 同值 0.5 对称，两道注释互标——修改一处先读另一处）
pub const MASK_DOWNSAMPLE_THRESHOLD: f32 = 0.5;

/// PNG 原图边长上限（w 与 h 各自 ≤ 此值；恰等放行，超出拒绝）
pub const MAX_AVATAR_PNG_DIM: u32 = 1024;

/// TODO(D-017): 碎片阈值 N 待土豆拍板——先按 N=8 像素实现（临时默认），拍板后改常数。
/// 语义：非主件连通域面积 ≥ N = 断件图打回（isolate_fail）；< N = 小碎片
/// 静默丢弃收图。N 计数对象=降采样后 128x128 表上的格数（连通域在表上做）
pub const MASK_FRAG_DROP_N: usize = 8;

/// 位图掩码：128x128 布尔表，行主序（solid[y * w + x]，0=空心 1=实心）
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AvatarMask {
    /// 表宽（=MASK_SIZE，字段留给将来非方形表）
    pub w: u16,
    /// 表高（=MASK_SIZE）
    pub h: u16,
    /// 实/空对照表（w*h 长，值恒 0/1）
    pub solid: Vec<u8>,
}

impl AvatarMask {
    /// 全空表
    pub fn new(w: u16, h: u16) -> Self {
        Self {
            w,
            h,
            solid: vec![0; w as usize * h as usize],
        }
    }

    /// 读格（越界 panic 由调用方保证不越界——内部管线固定 128 域）
    pub fn get(&self, x: u16, y: u16) -> u8 {
        self.solid[y as usize * self.w as usize + x as usize]
    }

    /// 写格
    pub fn set(&mut self, x: u16, y: u16, v: u8) {
        self.solid[y as usize * self.w as usize + x as usize] = v;
    }
}

/// 掩码管线错误（reason 映射在 bin 上传层：Decode->decode_failed /
/// TooLarge->oversize_dim / Isolate->isolate_fail）
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum MaskError {
    /// PNG 解码失败（坏字节/截断/不支持的格式）
    #[error("PNG 解码失败或不支持的格式: {0}")]
    Decode(String),
    /// 原图边长超上限（MAX_AVATAR_PNG_DIM）
    #[error("PNG 尺寸超上限 {MAX_AVATAR_PNG_DIM}: {w}x{h}")]
    TooLarge { w: u32, h: u32 },
    /// 连通性校验失败：无实心内容或存在多个大连通域（断件图）
    #[error("连通性校验失败：断件图（多大连通域或全空）")]
    Isolate,
    /// 掩码表尺寸非 MASK_SIZE（公开 API 防御边界——连通域坐标换算按 128 硬约束）
    #[error("掩码表尺寸非 {MASK_SIZE}: {w}x{h}")]
    BadDims { w: u16, h: u16 },
}

/// PNG 原图尺寸（只读头不解码像素；上传元数据 w/h 用）。
/// 日志豁免：trivial helper——成功路径由调用方（store_upload 收图日志）携带尺寸
pub fn png_dimensions(png_bytes: &[u8]) -> Result<(u32, u32), MaskError> {
    let decoder = png::Decoder::new(std::io::Cursor::new(png_bytes));
    let reader = decoder
        .read_info()
        .map_err(|e| MaskError::Decode(e.to_string()))?;
    Ok((reader.info().width, reader.info().height))
}

/// PNG -> 位图掩码（解码 -> alpha 阈值化 -> area-average 降采样 128x128）。
/// 不做连通性校验（validate_and_prune 独立函数，调用方按需组合）
pub fn decode_png_to_mask(png_bytes: &[u8]) -> Result<AvatarMask, MaskError> {
    let mut decoder = png::Decoder::new(std::io::Cursor::new(png_bytes));
    // EXPAND：调色板/低位深灰度归一成 8 位真彩（生图工具多产调色板透明底，
    // 拒收调色板=误伤）；STRIP_16：16 位深度降 8 位。归一后只剩 4 种输出色型
    decoder.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
    let mut reader = decoder
        .read_info()
        .map_err(|e| MaskError::Decode(e.to_string()))?;
    // 尺寸护栏在读头后、分配解码缓冲前拦截（防超大头声明骗大内存分配）
    let (w, h) = (reader.info().width, reader.info().height);
    if w > MAX_AVATAR_PNG_DIM || h > MAX_AVATAR_PNG_DIM {
        return Err(MaskError::TooLarge { w, h });
    }
    let mut buf = vec![0u8; reader.output_buffer_size()];
    let out = reader
        .next_frame(&mut buf)
        .map_err(|e| MaskError::Decode(e.to_string()))?;
    // alpha 阈值化（原图分辨率）：EXPAND 归一后 4 色型全覆盖；每像素字节数按色型取
    let bytes_pp = match out.color_type {
        png::ColorType::Rgba => 4usize,
        png::ColorType::Rgb => 3,
        png::ColorType::GrayscaleAlpha => 2,
        png::ColorType::Grayscale => 1,
        // EXPAND 后不会再出现调色板（防御性收口，正常不可达）
        png::ColorType::Indexed => {
            return Err(MaskError::Decode("调色板未展开（内部不变量）".to_string()))
        }
    };
    let src_w = out.width as usize;
    let solid_src: Vec<u8> = buf[..out.buffer_size()]
        .chunks_exact(bytes_pp)
        .map(|px| match out.color_type {
            png::ColorType::Rgba => u8::from(px[3] > MASK_ALPHA_THRESHOLD),
            png::ColorType::GrayscaleAlpha => u8::from(px[1] > MASK_ALPHA_THRESHOLD),
            // 无 alpha 通道的色型（Rgb/Grayscale）= 全不透明 = 全实心
            _ => 1,
        })
        .collect();
    if solid_src.len() != src_w * out.height as usize {
        return Err(MaskError::Decode("像素数与尺寸不符".to_string()));
    }
    tracing::debug!(
        w = out.width,
        h = out.height,
        color = ?out.color_type,
        "[mask] PNG 解码+阈值化完成，进入降采样"
    );
    Ok(downsample_to_mask(out.width, out.height, &solid_src))
}

/// area-average 降采样到 128x128：输出格 = 原图对应块内实心占比
/// **严格大于** MASK_DOWNSAMPLE_THRESHOLD（0.5，与 MASK_ALPHA_THRESHOLD 同值对称）。
/// 块边界 = floor(i*W/128) 精确划分（每原图像素恰属一输出格，面积平均语义）；
/// 块小于 1 像素（小图放大）时钳到 1 像素 = 最近邻语义（放大不丢信息）。
/// 纯整数边界+占比比较，同输入同输出（确定性宪法）
fn downsample_to_mask(src_w: u32, src_h: u32, solid_src: &[u8]) -> AvatarMask {
    let mut mask = AvatarMask::new(MASK_SIZE as u16, MASK_SIZE as u16);
    let (sw, sh) = (src_w as u64, src_h as u64);
    let n = MASK_SIZE as u64;
    for oy in 0..MASK_SIZE {
        let y0 = (oy as u64 * sh / n) as u32;
        let mut y1 = ((oy as u64 + 1) * sh / n) as u32;
        if y1 <= y0 {
            y1 = y0 + 1; // 放大轴：每输出格至少取 1 像素（最近邻）
        }
        for ox in 0..MASK_SIZE {
            let x0 = (ox as u64 * sw / n) as u32;
            let mut x1 = ((ox as u64 + 1) * sw / n) as u32;
            if x1 <= x0 {
                x1 = x0 + 1;
            }
            let (x1, y1) = (x1.min(src_w), y1.min(src_h)); // 边缘块按实际像素算
            let mut solid_count = 0u32;
            for y in y0..y1 {
                for x in x0..x1 {
                    solid_count += solid_src[(y * src_w + x) as usize] as u32;
                }
            }
            let total = (y1 - y0) * (x1 - x0);
            // 占比 > 0.5（f32 精确表示 0.5，n/2n 除法结果精确，无浮点噪声）
            if solid_count as f32 / total as f32 > MASK_DOWNSAMPLE_THRESHOLD {
                mask.set(ox as u16, oy as u16, 1);
            }
        }
    }
    tracing::debug!(src_w, src_h, "[mask] 降采样完成 128x128");
    mask
}

/// 连通性校验 + 碎片修剪（原地清碎片）：返回被丢弃的碎片总格数。
/// 规则：主件=最大连通域（同面积取扫描序靠前者，确定性）；任意非主件
/// 连通域面积 >= MASK_FRAG_DROP_N -> Err(Isolate)（断件打回）；
/// 全部小碎片清零丢弃。全空图（0 实心格）= 无主件 -> Err(Isolate)
pub fn validate_and_prune(mask: &mut AvatarMask) -> Result<usize, MaskError> {
    // 尺寸硬约束（公开 API 防御边界）：连通域换算按 MASK_SIZE，非 128 表显式
    // 报错而非静默算错（R1 审查 P2：AvatarMask 字段 pub，防将来误传非 128 表）
    if mask.w != MASK_SIZE as u16 || mask.h != MASK_SIZE as u16 {
        return Err(MaskError::BadDims { w: mask.w, h: mask.h });
    }
    let n = mask.solid.len();
    // 只读源快照（flood fill 与修剪期间原表被改写，克隆 16KB 换借用干净）
    let solid = mask.solid.clone();
    // 连通域标记（4-连通；DFS 栈实现——连通性只看集合，栈/队列等价）
    let mut comp = vec![usize::MAX; n];
    let mut areas: Vec<usize> = Vec::new();
    for (start, &s) in solid.iter().enumerate() {
        if s != 1 || comp[start] != usize::MAX {
            continue;
        }
        let id = areas.len();
        let mut area = 0usize;
        let mut stack = vec![start];
        comp[start] = id;
        while let Some(cur) = stack.pop() {
            area += 1;
            let x = (cur % MASK_SIZE) as i64;
            let y = (cur / MASK_SIZE) as i64;
            for (dx, dy) in [(-1i64, 0i64), (1, 0), (0, -1), (0, 1)] {
                let (nx, ny) = (x + dx, y + dy);
                if nx < 0 || ny < 0 || nx >= MASK_SIZE as i64 || ny >= MASK_SIZE as i64 {
                    continue;
                }
                let ni = ny as usize * MASK_SIZE + nx as usize;
                if solid[ni] == 1 && comp[ni] == usize::MAX {
                    comp[ni] = id;
                    stack.push(ni);
                }
            }
        }
        areas.push(area);
    }
    // 全空图 = 无主件可保留 -> 打回
    let main_id = areas
        .iter()
        .enumerate()
        .max_by_key(|(id, &a)| (a, usize::MAX - id)) // 同面积取扫描序靠前（id 小者胜）
        .map(|(id, _)| id);
    let Some(main_id) = main_id else {
        tracing::info!("[mask] 连通性校验失败：全空掩码（0 实心格，无主件）");
        return Err(MaskError::Isolate);
    };
    // 非 main 且面积 >= N = 第二个大件 -> 断件打回
    for (id, &a) in areas.iter().enumerate() {
        if id != main_id && a >= MASK_FRAG_DROP_N {
            tracing::info!(
                main_area = areas[main_id],
                other_area = a,
                frag_n = MASK_FRAG_DROP_N,
                "[mask] 连通性校验失败：存在第二个大连通域（断件图）"
            );
            return Err(MaskError::Isolate);
        }
    }
    // 小碎片静默清零（碎片丢弃，日志记碎片像素数）
    let mut dropped = 0usize;
    for (i, &c) in comp.iter().enumerate() {
        if solid[i] == 1 && c != main_id {
            mask.solid[i] = 0;
            dropped += 1;
        }
    }
    tracing::info!(
        components = areas.len(),
        main_area = areas[main_id],
        dropped_fragments = dropped,
        frag_n = MASK_FRAG_DROP_N,
        "[mask] 连通性校验通过：主件保留，碎片丢弃"
    );
    Ok(dropped)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 测试助手：RGBA8 像素 -> PNG 字节（png crate 编码，测试自产 fixtures）
    fn encode_rgba_png(w: u32, h: u32, rgba: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        {
            let mut enc = png::Encoder::new(&mut out, w, h);
            enc.set_color(png::ColorType::Rgba);
            enc.set_depth(png::BitDepth::Eight);
            let mut writer = enc.write_header().expect("PNG 头应可写");
            writer.write_image_data(rgba).expect("PNG 像素应可写");
        }
        out
    }

    /// 测试助手：全图同 alpha 的纯色 PNG
    fn solid_png(w: u32, h: u32, alpha: u8) -> Vec<u8> {
        let px = vec![[255u8, 0, 0, alpha].as_slice().to_vec(); (w * h) as usize].concat();
        encode_rgba_png(w, h, &px)
    }

    #[test]
    fn alpha_threshold_128_solid_127_hollow() {
        // 规则：恰 50% 边界——alpha=128（>50%）实心，alpha=127（≤50%）空心；
        // 2x1 图一行两个像素分别验证
        let mut px = Vec::new();
        px.extend_from_slice(&[255, 255, 255, 128]);
        px.extend_from_slice(&[255, 255, 255, 127]);
        let png = encode_rgba_png(2, 1, &px);
        let mask = decode_png_to_mask(&png).expect("合法 PNG 应可解码");
        // 2 宽图放大：cell 0..63 映射源像素 0、cell 64..127 映射源像素 1
        //（floor(i*W/128) 边界——放大即最近邻摊格）
        assert_eq!(mask.get(0, 0), 1, "alpha=128 实心（>127）");
        assert_eq!(mask.get(63, 0), 1, "cell 63 仍映射像素 0（实心）");
        assert_eq!(mask.get(64, 0), 0, "cell 64 起映射像素 1：alpha=127 空心（<=127）");
        assert_eq!(mask.get(127, 0), 0, "末格同像素 1 空心");
        assert_eq!((mask.w, mask.h), (128, 128), "输出表恒 128x128");
    }

    #[test]
    fn fully_transparent_image_rejected_as_isolate() {
        // 规则：全透明图 = 0 实心格 = 无主件可保留 -> Err(Isolate)（打回）
        let png = solid_png(64, 64, 0);
        let mut mask = decode_png_to_mask(&png).expect("解码应成功");
        assert_eq!(mask.solid.iter().map(|&v| v as u32).sum::<u32>(), 0, "全透明图阈值化后全空");
        assert_eq!(
            validate_and_prune(&mut mask),
            Err(MaskError::Isolate),
            "无实心内容打回"
        );
    }

    #[test]
    fn fully_solid_image_single_component_accepted() {
        // 规则：全实心图 = 单一连通域 = 主件覆盖全表 -> Ok(0 碎片)
        let png = solid_png(64, 64, 255);
        let mut mask = decode_png_to_mask(&png).expect("解码应成功");
        let dropped = validate_and_prune(&mut mask).expect("全实心应通过校验");
        assert_eq!(dropped, 0, "无碎片");
        assert_eq!(mask.solid.iter().map(|&v| v as u32).sum::<u32>(), 128 * 128, "全表实心");
    }

    #[test]
    fn downsample_1024_exact_left_half_solid() {
        // 规则：1024 恰等放行（8x8 块/格）；左半实心右半透明 -> 输出左 64 列实心
        let w = 1024u32;
        let h = 1024u32;
        let mut px = vec![0u8; (w * h * 4) as usize];
        for y in 0..h {
            for x in 0..w {
                let a = if x < 512 { 255 } else { 0 };
                let i = ((y * w + x) * 4) as usize;
                px[i] = 255;
                px[i + 3] = a;
            }
        }
        let png = encode_rgba_png(w, h, &px);
        let mask = decode_png_to_mask(&png).expect("1024 恰等应放行");
        for y in [0u16, 64, 127] {
            assert_eq!(mask.get(0, y), 1, "左列实心 y={y}");
            assert_eq!(mask.get(63, y), 1, "第 63 列实心（块 504..512 全实心）");
            assert_eq!(mask.get(64, y), 0, "第 64 列空心（块 512..520 全透明）");
            assert_eq!(mask.get(127, y), 0, "右列空心");
        }
    }

    #[test]
    fn downsample_128_exact_passthrough() {
        // 规则：128 恰等 1:1 透传（块=单像素）；单点图 -> 单格实心
        let mut px = vec![0u8; 128 * 128 * 4];
        let i = (5 * 128 + 9) * 4;
        px[i] = 255;
        px[i + 3] = 255;
        let png = encode_rgba_png(128, 128, &px);
        let mask = decode_png_to_mask(&png).expect("128 恰等应放行");
        assert_eq!(mask.get(9, 5), 1, "实心像素落在同格");
        assert_eq!(
            mask.solid.iter().map(|&v| v as u32).sum::<u32>(),
            1,
            "整表恰一格实心（1:1 透传）"
        );
    }

    #[test]
    fn downsample_block_exact_half_not_solid() {
        // 规则：块内实心占比恰 50% = 不算实心（严格大于；与 alpha 阈值同值对称）
        // 256x256 -> 每输出格 2x2 块；格 (0,0) 块内恰 2/4 实心 -> 空
        let w = 256u32;
        let h = 256u32;
        let mut px = vec![0u8; (w * h * 4) as usize];
        for y in 0..2u32 {
            for x in 0..2u32 {
                let a = if x == 0 { 255 } else { 0 }; // 竖半分：恰 2/4
                let i = ((y * w + x) * 4) as usize;
                px[i] = 255;
                px[i + 3] = a;
            }
        }
        let png = encode_rgba_png(w, h, &px);
        let mask = decode_png_to_mask(&png).expect("解码应成功");
        assert_eq!(mask.get(0, 0), 0, "块内恰 50% 不算实心（严格大于）");
    }

    #[test]
    fn oversized_1025_rejected() {
        // 规则：边长 1025 > 1024 上限 -> Err(TooLarge)（恰等放行的另一侧）
        let png = solid_png(1025, 4, 255);
        assert_eq!(
            decode_png_to_mask(&png),
            Err(MaskError::TooLarge { w: 1025, h: 4 })
        );
        let png = solid_png(4, 1025, 255);
        assert!(matches!(
            decode_png_to_mask(&png),
            Err(MaskError::TooLarge { .. })
        ), "高度超限同样拒绝");
    }

    #[test]
    fn small_png_upscaled_faithfully() {
        // 规则：< 128 忠实放大（最近邻语义，放大不丢信息）
        // 127 全实心 -> 128 全实心（0.992 倍放大）
        let png = solid_png(127, 127, 255);
        let mask = decode_png_to_mask(&png).expect("127 应放行（小于 128 放大）");
        assert_eq!(mask.solid.iter().map(|&v| v as u32).sum::<u32>(), 128 * 128, "全实心小图放大仍全实心");
        // 1x1 白点 -> 全表实心（每格都映射到唯一像素）
        let png = solid_png(1, 1, 255);
        let mask = decode_png_to_mask(&png).expect("1x1 应可解码");
        assert_eq!(mask.solid.iter().map(|&v| v as u32).sum::<u32>(), 128 * 128, "1x1 实心放大到全表");
    }

    #[test]
    fn non_square_image_downsamples_per_axis() {
        // 规则：非方形逐轴独立换算——64x128（宽放大 2 倍/高原样）
        // 左半列实心 -> 输出左 64 列实心
        let w = 64u32;
        let h = 128u32;
        let mut px = vec![0u8; (w * h * 4) as usize];
        for y in 0..h {
            for x in 0..w {
                let a = if x < 32 { 255 } else { 0 };
                let i = ((y * w + x) * 4) as usize;
                px[i] = 255;
                px[i + 3] = a;
            }
        }
        let png = encode_rgba_png(w, h, &px);
        let mask = decode_png_to_mask(&png).expect("解码应成功");
        // 64 宽放大：cell i 映射源像素 floor(i/2)；源 x<32 实心 = 表左 64 列实心
        assert_eq!(mask.get(0, 64), 1, "左列实心");
        assert_eq!(mask.get(63, 64), 1, "cell 63 映射源像素 31（实心）");
        assert_eq!(mask.get(64, 64), 0, "cell 64 映射源像素 32（透明起）");
        assert_eq!(mask.get(127, 64), 0, "右列空心");
    }

    #[test]
    fn two_large_components_rejected() {
        // 规则：两个面积 >= N 的大连通域 = 断件图 -> Err(Isolate)
        // 128x128：上半实心 + 下半实心（中间断开）= 两个巨型连通域
        let mut px = vec![0u8; 128 * 128 * 4];
        for y in 0..128u32 {
            let a = if y < 60 || y >= 68 { 255 } else { 0 };
            for x in 0..128u32 {
                let i = ((y * 128 + x) * 4) as usize;
                px[i] = 255;
                px[i + 3] = a;
            }
        }
        let png = encode_rgba_png(128, 128, &px);
        let mut mask = decode_png_to_mask(&png).expect("解码应成功");
        assert_eq!(
            validate_and_prune(&mut mask),
            Err(MaskError::Isolate),
            "两个大连通域打回"
        );
    }

    #[test]
    fn main_plus_tiny_fragment_pruned_and_counted() {
        // 规则：主件 + 2 格小碎片（< N=8）= 收图成功，碎片静默清零并计数
        let mut px = vec![0u8; 128 * 128 * 4];
        // 主件：左上 10x10 实心块
        for y in 0..10u32 {
            for x in 0..10u32 {
                let i = ((y * 128 + x) * 4) as usize;
                px[i] = 255;
                px[i + 3] = 255;
            }
        }
        // 碎片：右下 2 格竖条（彼此 4-连通成 1 个 2 格碎片）
        for dy in 0..2u32 {
            let i = (((100 + dy) * 128 + 100) * 4) as usize;
            px[i] = 255;
            px[i + 3] = 255;
        }
        let png = encode_rgba_png(128, 128, &px);
        let mut mask = decode_png_to_mask(&png).expect("解码应成功");
        let dropped = validate_and_prune(&mut mask).expect("主件+小碎片应收图");
        assert_eq!(dropped, 2, "恰好丢弃 2 格碎片");
        assert_eq!(mask.get(100, 100), 0, "碎片格已清零");
        assert_eq!(mask.get(100, 101), 0, "碎片格已清零");
        assert_eq!(mask.get(0, 0), 1, "主件保留");
        assert_eq!(mask.get(9, 9), 1, "主件保留");
    }

    #[test]
    fn fragment_exactly_threshold_n_rejected() {
        // 规则：碎片面积恰 = N=8 -> >= N 按断件打回（< N 才丢弃）
        let mut px = vec![0u8; 128 * 128 * 4];
        for y in 0..10u32 {
            for x in 0..10u32 {
                let i = ((y * 128 + x) * 4) as usize;
                px[i] = 255;
                px[i + 3] = 255;
            }
        }
        for dy in 0..8u32 {
            let i = (((100 + dy) * 128 + 100) * 4) as usize;
            px[i] = 255;
            px[i + 3] = 255;
        }
        let png = encode_rgba_png(128, 128, &px);
        let mut mask = decode_png_to_mask(&png).expect("解码应成功");
        assert_eq!(
            validate_and_prune(&mut mask),
            Err(MaskError::Isolate),
            "8 格碎片 = 面积达到 N，按断件打回"
        );
    }

    #[test]
    fn palette_png_expanded_and_decoded() {
        // 规则：真彩/调色板皆可（EXPAND 归一化）——透明底生图工具多产调色板图
        // 4x1 调色板：2 色（不透明红 + 全透明），tRNS 提供 alpha
        let mut out = Vec::new();
        {
            let mut enc = png::Encoder::new(&mut out, 4, 1);
            enc.set_color(png::ColorType::Indexed);
            enc.set_depth(png::BitDepth::Eight);
            enc.set_palette(vec![255, 0, 0, 0, 255, 0]); // 红/绿两色
            enc.set_trns(vec![255, 0]); // 色 0 不透明、色 1 全透明
            let mut writer = enc.write_header().expect("PNG 头应可写");
            let indexed = vec![0u8, 0u8, 1u8, 1u8];
            writer.write_image_data(&indexed).expect("索引数据应可写");
        }
        let mask = decode_png_to_mask(&out).expect("调色板图应可解码");
        // 4 宽图放大：cell i 映射源像素 floor(i/32)（px0/px1 不透明、px2/px3 透明）
        assert_eq!(mask.get(0, 0), 1, "cell 0 映射不透明索引色 0");
        assert_eq!(mask.get(63, 0), 1, "cell 63 仍映射索引色 1（不透明）");
        assert_eq!(mask.get(64, 0), 0, "cell 64 起映射索引色 2（tRNS 全透明）");
        assert_eq!(mask.get(127, 0), 0, "末格映射索引色 3（全透明）");
    }

    #[test]
    fn sixteen_bit_png_stripped_and_decoded() {
        // 规则：16 位深度自动降 8 位（STRIP_16）——高位深 PNG 不拒收
        // 像素 0 = (65535,0,0,65535) 实心；像素 1 = 全透明
        let mut raw = Vec::new();
        for (r, g, b, a) in [(65535u16, 0u16, 0u16, 65535u16), (0, 0, 0, 0)] {
            raw.extend_from_slice(&r.to_be_bytes());
            raw.extend_from_slice(&g.to_be_bytes());
            raw.extend_from_slice(&b.to_be_bytes());
            raw.extend_from_slice(&a.to_be_bytes());
        }
        let mut out = Vec::new();
        {
            let mut enc = png::Encoder::new(&mut out, 2, 1);
            enc.set_color(png::ColorType::Rgba);
            enc.set_depth(png::BitDepth::Sixteen);
            let mut writer = enc.write_header().expect("PNG 头应可写");
            writer.write_image_data(&raw).expect("16 位像素应可写");
        }
        let mask = decode_png_to_mask(&out).expect("16 位图应可解码");
        // 2 宽图放大：cell 0..63 映射像素 0（不透明）、cell 64..127 映射像素 1（透明）
        assert_eq!(mask.get(0, 0), 1, "不透明 16 位像素实心");
        assert_eq!(mask.get(63, 0), 1, "cell 63 仍映射像素 0（实心）");
        assert_eq!(mask.get(64, 0), 0, "cell 64 起映射透明 16 位像素空心");
    }

    #[test]
    fn corrupt_bytes_rejected_as_decode_error() {
        // 规则：坏字节/非 PNG 数据 -> Err(Decode)（bin 层映射 reason=decode_failed）
        assert!(matches!(
            decode_png_to_mask(b"not a png at all"),
            Err(MaskError::Decode(_))
        ));
        // PNG 头正确但数据截断
        let good = solid_png(8, 8, 255);
        assert!(matches!(
            decode_png_to_mask(&good[..good.len() / 2]),
            Err(MaskError::Decode(_))
        ));
    }

    #[test]
    fn png_dimensions_reports_original_size() {
        // 规则：元数据 w/h = 原图尺寸（非 128 表尺寸）
        let png = solid_png(300, 200, 255);
        assert_eq!(png_dimensions(&png).unwrap(), (300, 200));
        assert!(matches!(
            png_dimensions(b"garbage"),
            Err(MaskError::Decode(_))
        ), "坏数据同样报 Decode");
    }

    #[test]
    fn validate_prune_rejects_non_128_table() {
        // 规则（R1 双审 P2）：连通域坐标换算按 MASK_SIZE 硬约束——非 128 表显式
        // Err(BadDims) 而非静默算错（公开 API + pub 字段的防御边界）
        let mut m = AvatarMask::new(64, 64);
        m.set(0, 0, 1);
        assert_eq!(
            validate_and_prune(&mut m),
            Err(MaskError::BadDims { w: 64, h: 64 })
        );
    }

    #[test]
    fn validate_prune_tie_breaks_deterministically() {
        // 规则：两连通域同面积 -> 主件取扫描序靠前者（行主序先遇到者），
        // 其余清零——同输入同结果（确定性宪法）
        let mut mask = AvatarMask::new(128, 128);
        // 域 A：行 0 的两格（扫描序先遇到）
        mask.set(0, 0, 1);
        mask.set(1, 0, 1);
        // 域 B：行 50 的两格（同面积，扫描序靠后 -> 被当碎片清零）
        mask.set(10, 50, 1);
        mask.set(11, 50, 1);
        let dropped = validate_and_prune(&mut mask).expect("同面积两域各 2 格均 < N：主件+小碎片应收图");
        assert_eq!(dropped, 2, "靠后域按碎片清零");
        assert_eq!(mask.get(0, 0), 1, "扫描序靠前者为主件保留");
        assert_eq!(mask.get(10, 50), 0, "靠后者清零");
    }
}
