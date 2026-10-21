//! D-017 收图通道：头像资产登记表 + 上传落盘（内存+磁盘双登记）
//!
//! 分工（卡面定案）：资产是**静态文件非世界状态**——不进 SQLite 快照管线，
//! 上传即写盘（assets/avatars/{asset_id}.png + 伴生 .json 元数据），启动扫目录
//! 重建登记表。掩码查询走内存登记表（avatar_assets 消息全量下发）。
//! 角色进场（players 目录 .wat 机制）与收图解耦：本卡=资产层，不挂实体外观。

use codeworld_core::mask::{decode_png_to_mask, png_dimensions, validate_and_prune, AvatarMask, MaskError};
use serde::{Deserialize, Serialize};
use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::sync::RwLock;

/// PNG 文件大小护栏（2MB；常数入 game-menu 1.7 总账 MAX_AVATAR_PNG_BYTES）
pub(crate) const MAX_AVATAR_PNG_BYTES: usize = 2 * 1024 * 1024;

/// 原图 HTTP 取图路径前缀（net.rs 把资产目录挂 ServeDir 于 /assets；
/// axum nest_service 会剥 /assets 前缀，故 URL=/assets/{id}.png -> 目录内 {id}.png）
pub(crate) const AVATAR_URL_PREFIX: &str = "/assets/";

/// 单个头像资产（内存登记表条目；mask=128x128 渲染表）
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct AvatarAsset {
    pub asset_id: String,
    pub name: String,
    /// 世界高度格数（上传参数，域钳制 0.05..=25.0；本卡只登记不消费——挂实体刀用）
    pub height_world: f64,
    /// 原图尺寸（前端按纵横比渲染原图用）
    pub orig_w: u32,
    pub orig_h: u32,
    pub mask: AvatarMask,
    /// 登记拍数。世界拍钟不跨线程共享（ws 线程无 world 句柄），资产与拍解耦
    /// （本卡=资产层），新上传恒 0；挂实体刀需要时再接钟
    pub created_tick: u64,
}

/// 伴生元数据（磁盘 .json；与 AvatarAsset 字段一一对应，掩码不入盘——
/// 掩码由 PNG 现算，避免双份真相漂移）
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct AvatarMeta {
    pub asset_id: String,
    pub name: String,
    pub height_world: f64,
    pub w: u32,
    pub h: u32,
    pub created_tick: u64,
}

/// 上传拒绝原因（HTTP 响应 reason 字段，全 ASCII 小写；net-protocol v2.2 写死）
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RejectReason {
    /// multipart 字段缺失/字段值非法（缺 file、name 非法、height_world 非数值）
    BadJson,
    /// PNG 文件超 2MB
    Oversize,
    /// 原图边长超 1024（MAX_AVATAR_PNG_DIM；恰等放行的另一侧）
    OversizeDim,
    /// PNG 解码失败/不支持格式
    DecodeFailed,
    /// 连通性校验失败（断件图：多大连通域或全空）
    IsolateFail,
}

impl RejectReason {
    /// 响应线名（ASCII 小写）
    pub fn wire(self) -> &'static str {
        match self {
            RejectReason::BadJson => "bad_json",
            RejectReason::Oversize => "oversize",
            RejectReason::OversizeDim => "oversize_dim",
            RejectReason::DecodeFailed => "decode_failed",
            RejectReason::IsolateFail => "isolate_fail",
        }
    }
}

/// 掩码管线错误 -> 上传拒绝原因（单一映射点，net-protocol 文档锚）
impl From<MaskError> for RejectReason {
    fn from(e: MaskError) -> Self {
        match e {
            MaskError::Decode(_) => RejectReason::DecodeFailed,
            MaskError::TooLarge { .. } => RejectReason::OversizeDim,
            MaskError::Isolate => RejectReason::IsolateFail,
            // BadDims=内部不变量被破（管线恒产 128 表），防御分支归 decode_failed
            MaskError::BadDims { .. } => RejectReason::DecodeFailed,
        }
    }
}

/// 头像资产登记表：磁盘目录 + 内存条目（Arc 共享给 ws 线程与上传路由）。
/// RwLock 只保护 Vec 本身，锁内不做 IO（读快照=clone，资产数预期少）
pub(crate) struct AvatarRegistry {
    dir: PathBuf,
    assets: RwLock<Vec<AvatarAsset>>,
}

impl AvatarRegistry {
    /// 启动扫目录重建登记表：{id}.png + {id}.json 成对才收；坏文件 warn 跳过
    /// 不拖垮世界启动（资产是静态文件，坏了不致命）
    pub fn load(dir: &Path) -> Self {
        let reg = Self {
            dir: dir.to_path_buf(),
            assets: RwLock::new(Vec::new()),
        };
        // 目录不存在则建（冷启动首次上传的落盘前提；失败只 warn 不崩）
        if let Err(e) = std::fs::create_dir_all(dir) {
            tracing::warn!(dir = %dir.display(), error = %e, "[avatars] 资产目录创建失败（上传将不可用）");
        }
        let Ok(entries) = std::fs::read_dir(dir) else {
            // 目录不存在 = 零资产冷启动（首次运行常态，debug 即可不刷屏）
            tracing::debug!(dir = %dir.display(), "[avatars] 资产目录不存在，零资产启动");
            return reg;
        };
        let mut loaded = 0usize;
        let mut skipped = 0usize;
        let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
        let mut metas: Vec<(PathBuf, AvatarMeta)> = Vec::new();
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            let Ok(text) = std::fs::read_to_string(&path) else {
                tracing::warn!(file = %path.display(), "[avatars] 元数据不可读，跳过");
                skipped += 1;
                continue;
            };
            match serde_json::from_str::<AvatarMeta>(&text) {
                Ok(meta) => {
                    // 查重（R1 审查）：多份 json 声明同一 asset_id 只收先到者
                    if !seen.insert(meta.asset_id.clone()) {
                        tracing::warn!(
                            file = %path.display(),
                            asset_id = %meta.asset_id,
                            "[avatars] asset_id 重复（多份 json 同 id），跳过"
                        );
                        skipped += 1;
                    } else if is_canonical_asset_id(&meta.asset_id) {
                        metas.push((path, meta));
                    } else {
                        tracing::warn!(
                            file = %path.display(),
                            asset_id = %meta.asset_id,
                            "[avatars] asset_id 不合规范（av_+16hex），跳过（路径安全不变量）"
                        );
                        skipped += 1;
                    }
                }
                Err(e) => {
                    tracing::warn!(file = %path.display(), error = %e, "[avatars] 元数据损坏，跳过");
                    skipped += 1;
                }
            }
        }
        metas.sort_by(|a, b| a.1.asset_id.cmp(&b.1.asset_id)); // 重建序稳定（确定性）
        for (_meta_path, meta) in metas {
            let png_path = dir.join(format!("{}.png", meta.asset_id));
            let Ok(mut file) = std::fs::File::open(&png_path) else {
                tracing::warn!(asset_id = %meta.asset_id, "[avatars] PNG 缺失（孤儿元数据），跳过");
                skipped += 1;
                continue;
            };
            let mut bytes = Vec::new();
            if file.read_to_end(&mut bytes).is_err() {
                tracing::warn!(asset_id = %meta.asset_id, "[avatars] PNG 不可读，跳过");
                skipped += 1;
                continue;
            };
            // 重建时重跑掩码管线+连通校验（手改盘上文件不合法就拒收——磁盘不合法
            // 的资产不进内存表，掩码与 PNG 永不漂移）
            let (orig_w, orig_h) = match png_dimensions(&bytes) {
                Ok(d) => d,
                Err(e) => {
                    tracing::warn!(asset_id = %meta.asset_id, error = %e, "[avatars] PNG 尺寸不可读，跳过");
                    skipped += 1;
                    continue;
                }
            };
            let mut mask = match decode_png_to_mask(&bytes) {
                Ok(m) => m,
                Err(e) => {
                    tracing::warn!(asset_id = %meta.asset_id, error = %e, "[avatars] PNG 掩码生成失败，跳过");
                    skipped += 1;
                    continue;
                }
            };
            if validate_and_prune(&mut mask).is_err() {
                tracing::warn!(asset_id = %meta.asset_id, "[avatars] PNG 连通性校验失败，跳过");
                skipped += 1;
                continue;
            }
            reg.assets.write().expect("登记表锁不应中毒").push(AvatarAsset {
                asset_id: meta.asset_id,
                name: meta.name,
                height_world: meta.height_world,
                orig_w,
                orig_h,
                mask,
                created_tick: meta.created_tick,
            });
            loaded += 1;
        }
        tracing::info!(
            dir = %dir.display(),
            loaded,
            skipped,
            "[avatars] 资产登记表重建完成"
        );
        reg
    }

    /// 登记新资产（上传成功后调用）
    pub fn push(&self, asset: AvatarAsset) {
        tracing::info!(
            asset_id = %asset.asset_id,
            name = %asset.name,
            "[avatars] 新资产登记（内存+磁盘已落）"
        );
        self.assets.write().expect("登记表锁不应中毒").push(asset);
    }

    /// 读快照（clone 全量；资产数预期少，掩码 16KB/个可接受——资产多时改增量，注释锚）
    pub fn list(&self) -> Vec<AvatarAsset> {
        self.assets.read().expect("登记表锁不应中毒").clone()
    }

    /// 资产目录（ServeDir 挂载点用）
    pub fn dir(&self) -> &Path {
        &self.dir
    }
}

/// 上传入参（路由层解析 multipart 后的归一形态）
pub(crate) struct UploadInput<'a> {
    pub png: &'a [u8],
    pub name: &'a str,
    /// multipart 缺字段 = None（默认 2.0）
    pub height_world: Option<f64>,
}

/// 上传主流程（校验->掩码->落盘->登记）：任何一步失败即整体拒绝；落盘半途
/// 失败清理已写文件（无孤儿垃圾），落盘成功才登记（磁盘与内存一致）
pub(crate) fn store_upload(
    reg: &AvatarRegistry,
    input: UploadInput,
) -> Result<AvatarAsset, RejectReason> {
    // ① 大小护栏（先拦字节量再解码——拒绝路径零解码成本）
    if input.png.len() > MAX_AVATAR_PNG_BYTES {
        tracing::info!(bytes = input.png.len(), "[avatars] 上传拒绝：超 2MB（oversize）");
        return Err(RejectReason::Oversize);
    }
    // ①b name 校验（R1 双审）：非空且 ≤64 字节（UTF-8 合法字节数，中文角色名合法）
    if input.name.is_empty() || input.name.len() > 64 {
        tracing::info!(name_len = input.name.len(), "[avatars] 上传拒绝：name 空或超 64 字节（bad_json）");
        return Err(RejectReason::BadJson);
    }
    // ② 原图尺寸（元数据 w/h 用）与掩码生成（解码+阈值化+降采样）
    let (orig_w, orig_h) = png_dimensions(input.png).map_err(RejectReason::from)?;
    let mut mask = decode_png_to_mask(input.png).map_err(RejectReason::from)?;
    // ③ 连通性校验 + 碎片修剪（断件图打回；小碎片静默丢弃，core 内已记日志）
    validate_and_prune(&mut mask).map_err(RejectReason::from)?;
    // ④ 落盘（PNG + 伴生元数据 json；写盘失败=整体拒绝不登记）
    let asset_id = new_asset_id();
    let png_path = reg.dir.join(format!("{asset_id}.png"));
    std::fs::write(&png_path, input.png).map_err(|e| {
        tracing::error!(path = %png_path.display(), error = %e, "[avatars] PNG 落盘失败");
        RejectReason::BadJson // IO 失败无专属 reason，归 bad_json 并 error 级日志留痕
    })?;
    let meta = AvatarMeta {
        asset_id: asset_id.clone(),
        name: input.name.to_string(),
        height_world: clamp_height(input.height_world),
        w: orig_w,
        h: orig_h,
        created_tick: 0, // 世界拍钟不跨线程共享，资产与拍解耦（挂实体刀再接钟）
    };
    let meta_path = reg.dir.join(format!("{asset_id}.json"));
    std::fs::write(
        &meta_path,
        serde_json::to_string_pretty(&meta).expect("元数据应可序列化"),
    )
    .map_err(|e| {
        tracing::error!(path = %meta_path.display(), error = %e, "[avatars] 元数据落盘失败");
        cleanup_partial_write(reg.dir(), &asset_id); // 清孤儿 png（不留垃圾）
        RejectReason::BadJson // IO 失败无专属 reason，归 bad_json + error 日志（协议文档注记）
    })?;
    tracing::info!(
        asset_id = %asset_id,
        name = %input.name,
        orig_w,
        orig_h,
        bytes = input.png.len(),
        "[avatars] 上传收图成功（PNG+元数据落盘）"
    );
    // ⑤ 登记（内存）
    let asset = AvatarAsset {
        asset_id,
        name: meta.name,
        height_world: meta.height_world,
        orig_w,
        orig_h,
        mask,
        created_tick: 0,
    };
    reg.push(asset.clone());
    Ok(asset)
}

/// 身高参数钳制：缺省 2.0；域 0.05..=25.0（与 radius 域一致，卡面定案=钳制非拒绝）；
/// NaN/非有限值回落默认（f64::clamp 遇 NaN 返回 NaN，必须先拦）。
/// 日志豁免：trivial helper——结果随 store_upload 收图日志一并留痕
pub(crate) fn clamp_height(h: Option<f64>) -> f64 {
    const DEFAULT: f64 = 2.0;
    const MIN: f64 = 0.05;
    const MAX: f64 = 25.0;
    match h {
        None => DEFAULT,
        Some(v) if v.is_nan() => DEFAULT, // clamp 遇 NaN 返回 NaN，必须先拦
        Some(v) => v.clamp(MIN, MAX),     // ±inf 自然钳到域端
    }
}

/// 资产 ID：`av_` + UUID 前 16 hex（ASCII 路径安全；碰撞概率 2^-64 忽略）。
/// 日志豁免：trivial helper——id 随 store_upload/登记日志一并留痕
fn new_asset_id() -> String {
    let hex = uuid::Uuid::new_v4().simple().to_string();
    format!("av_{}", &hex[..16])
}

/// 半途失败清理：删除已落盘的 png（json 写失败时调用，不留孤儿垃圾）。
/// 文件不存在=静默幂等（首次上传失败路径 png 也可能没写成）
fn cleanup_partial_write(dir: &Path, asset_id: &str) {
    let png_path = dir.join(format!("{asset_id}.png"));
    match std::fs::remove_file(&png_path) {
        Ok(()) => tracing::warn!(path = %png_path.display(), "[avatars] 半途失败清理：孤儿 png 已删"),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {} // 没写过，静默
        Err(e) => tracing::error!(path = %png_path.display(), error = %e, "[avatars] 孤儿 png 清理失败（重启扫描会跳过，无害垃圾）"),
    }
}

/// asset_id 规范校验：av_ + 16 hex（路径拼接安全不变量——加载/下发前先验，
/// 盘上手改的违规条目跳过不进内存表；"偶然安全"（穿越 id 拼不到 png）变"必然安全"）
fn is_canonical_asset_id(id: &str) -> bool {
    id.len() == 3 + 16
        && id.starts_with("av_")
        && id[3..].chars().all(|c| c.is_ascii_hexdigit())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 测试助手：RGBA8 像素 -> PNG 字节（png crate 编码，测试自产 fixtures）
    pub(crate) fn encode_rgba_png(w: u32, h: u32, rgba: &[u8]) -> Vec<u8> {
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

    /// 测试助手：全实心小 PNG（连通合法图）
    fn solid_png(w: u32, h: u32, alpha: u8) -> Vec<u8> {
        let mut px = Vec::new();
        for _ in 0..w * h {
            px.extend_from_slice(&[255, 0, 0, alpha]);
        }
        encode_rgba_png(w, h, &px)
    }

    /// 测试助手：临时资产目录（测后留存由 OS 清理，路径带进程号防并行撞）
    fn tmp_assets_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "codeworld-avatars-test-{tag}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir); // 上轮残留清掉（幂等）
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn good_upload(reg: &AvatarRegistry, name: &str) -> AvatarAsset {
        let png = solid_png(64, 64, 255);
        store_upload(
            reg,
            UploadInput {
                png: &png,
                name,
                height_world: None,
            },
        )
        .expect("合法上传应成功")
    }

    #[test]
    fn asset_id_format_is_av_plus_16_hex() {
        // 规则：av_ + 16 hex（路径安全 ASCII；两次生成不同）
        let a = new_asset_id();
        let b = new_asset_id();
        assert!(a.starts_with("av_"), "前缀 av_：{a}");
        assert_eq!(a.len(), 3 + 16, "16 hex：{a}");
        assert!(
            a[3..].chars().all(|c| c.is_ascii_hexdigit()),
            "全 hex：{a}"
        );
        assert_ne!(a, b, "两次生成应不同（随机性）");
    }

    #[test]
    fn upload_happy_path_writes_files_and_registers() {
        // 规则：合法上传 -> PNG+元数据落盘 + 登记表可查（掩码 128x128、原图尺寸携带）
        let dir = tmp_assets_dir("happy");
        let reg = AvatarRegistry::load(&dir);
        let asset = good_upload(&reg, "pioneer");
        // 文件落盘
        assert!(
            dir.join(format!("{}.png", asset.asset_id)).exists(),
            "PNG 应落盘"
        );
        assert!(
            dir.join(format!("{}.json", asset.asset_id)).exists(),
            "元数据 json 应落盘"
        );
        // 登记表可查
        let list = reg.list();
        assert_eq!(list.len(), 1, "恰一条登记");
        assert_eq!(list[0].asset_id, asset.asset_id);
        assert_eq!(list[0].name, "pioneer");
        assert_eq!(list[0].orig_w, 64, "原图尺寸入档");
        assert_eq!(list[0].orig_h, 64);
        assert_eq!(
            (list[0].mask.w, list[0].mask.h),
            (128, 128),
            "掩码恒 128x128"
        );
        assert_eq!(
            list[0].mask.solid.iter().map(|&v| v as u32).sum::<u32>(),
            128 * 128,
            "全实心小图 -> 全表实心"
        );
        // 元数据 json 内容可读回
        let meta: AvatarMeta = serde_json::from_str(
            &std::fs::read_to_string(dir.join(format!("{}.json", asset.asset_id))).unwrap(),
        )
        .unwrap();
        assert_eq!(meta.asset_id, asset.asset_id);
        assert_eq!(meta.name, "pioneer");
        assert_eq!((meta.w, meta.h), (64, 64));
        assert_eq!(meta.height_world, 2.0, "缺省身高 2.0");
    }

    #[test]
    fn upload_rejects_oversize_bytes() {
        // 规则：> 2MB 拒绝（reason=oversize）；不落盘不登记
        let dir = tmp_assets_dir("oversize");
        let reg = AvatarRegistry::load(&dir);
        let big = vec![0u8; MAX_AVATAR_PNG_BYTES + 1];
        assert_eq!(
            store_upload(&reg, UploadInput { png: &big, name: "x", height_world: None }),
            Err(RejectReason::Oversize)
        );
        assert!(reg.list().is_empty(), "拒绝的上传不登记");
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 0, "拒绝的上传不落盘");
    }

    #[test]
    fn upload_rejects_bad_png_bytes() {
        // 规则：坏字节 -> DecodeFailed
        let dir = tmp_assets_dir("badpng");
        let reg = AvatarRegistry::load(&dir);
        assert_eq!(
            store_upload(&reg, UploadInput { png: b"garbage", name: "x", height_world: None }),
            Err(RejectReason::DecodeFailed)
        );
    }

    #[test]
    fn upload_rejects_oversize_dim() {
        // 规则：边长 1025 > 1024 -> OversizeDim（恰等放行另一侧）
        let dir = tmp_assets_dir("bigdim");
        let reg = AvatarRegistry::load(&dir);
        let png = solid_png(4, 1025, 255);
        assert_eq!(
            store_upload(&reg, UploadInput { png: &png, name: "x", height_world: None }),
            Err(RejectReason::OversizeDim)
        );
    }

    #[test]
    fn upload_rejects_disconnected_image() {
        // 规则：断件图（两个大连通域）-> IsolateFail
        let dir = tmp_assets_dir("isolate");
        let reg = AvatarRegistry::load(&dir);
        // 128x128 上下两半实心（中间断开）= 两个巨型连通域
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
        assert_eq!(
            store_upload(&reg, UploadInput { png: &png, name: "x", height_world: None }),
            Err(RejectReason::IsolateFail)
        );
    }

    #[test]
    fn clamp_height_bounds_and_default() {
        // 规则：缺省 2.0；域钳制 0.05..=25.0（钳非拒）；NaN 回默认
        assert_eq!(clamp_height(None), 2.0, "缺省 2.0");
        assert_eq!(clamp_height(Some(1.5)), 1.5, "域内原样");
        assert_eq!(clamp_height(Some(100.0)), 25.0, "超上限钳到 25");
        assert_eq!(clamp_height(Some(0.01)), 0.05, "超下限钳到 0.05");
        assert_eq!(clamp_height(Some(f64::NAN)), 2.0, "NaN 回默认（clamp 防御）");
        assert_eq!(clamp_height(Some(f64::INFINITY)), 25.0, "+inf 钳到上限");
    }

    #[test]
    fn registry_load_dedupes_same_asset_id() {
        // 规则（R1 质量审 P1-1 残留子项）：两个 json 声明同一 asset_id = 重复注册——
        // 扫目录加载按 asset_id 查重，后到者跳过（先到者优先，日志留痕）
        let dir = tmp_assets_dir("dup");
        let reg = AvatarRegistry::load(&dir);
        let a = good_upload(&reg, "first");
        // 手造同 id 第二份 png+json（文件名不同、内部 asset_id 相同）
        let png = solid_png(8, 8, 255);
        let other_name = format!("{}.png", uuid::Uuid::new_v4().simple());
        std::fs::write(dir.join(&other_name), &png).unwrap();
        std::fs::write(
            dir.join(other_name.replace(".png", ".json")),
            format!(
                r#"{{"asset_id":"{}","name":"dup","height_world":2.0,"w":8,"h":8,"created_tick":0}}"#,
                a.asset_id
            ),
        )
        .unwrap();
        let reg2 = AvatarRegistry::load(&dir);
        assert_eq!(reg2.list().len(), 1, "同 id 只注册一次");
    }

    #[test]
    fn upload_rejects_empty_or_oversize_name() {
        // 规则（R1 双审 P2 合并）：name 非空且 ≤64 字节（UTF-8；中文角色名合法，
        // 协议文档不设 ASCII 限定）——空串/超长按 bad_json 拒绝不落盘
        let dir = tmp_assets_dir("name");
        let reg = AvatarRegistry::load(&dir);
        let png = solid_png(8, 8, 255);
        let bads: Vec<String> = vec![String::new(), "x".repeat(65)];
        for bad in &bads {
            assert_eq!(
                store_upload(&reg, UploadInput { png: &png, name: bad, height_world: None }),
                Err(RejectReason::BadJson),
                "非法 name 应拒绝：{:?}",
                &bad[..bad.len().min(8)]
            );
        }
        // 边界内合法：恰 64 字节 + 中文名
        let ok64 = "x".repeat(64);
        assert!(store_upload(&reg, UploadInput { png: &png, name: &ok64, height_world: None }).is_ok());
        assert!(store_upload(&reg, UploadInput { png: &png, name: "皮蛋", height_world: None }).is_ok());
    }

    #[test]
    fn cleanup_partial_write_removes_stray_png() {
        // 规则（R1 质量审 P2-1）：json 落盘失败时清理已写的 png（不留孤儿垃圾）——
        // 清理函数单元行为：目标文件存在则删、不存在则静默
        let dir = tmp_assets_dir("cleanup");
        let stray = dir.join("av_0123456789abcdef.png");
        std::fs::write(&stray, b"png-bytes").unwrap();
        cleanup_partial_write(&dir, "av_0123456789abcdef");
        assert!(!stray.exists(), "孤儿 png 应被清理");
        // 不存在的文件静默（幂等）
        cleanup_partial_write(&dir, "av_0123456789abcdef");
    }

    #[test]
    fn registry_load_skips_asset_id_violating_format() {
        // 规则：盘上元数据的 asset_id 必须形如 av_+16hex（路径安全不变量）——
        // 格式违规的条目（如 av_short：能拼到同位 png 但不合规范）跳过不加载。
        // 注：穿越型 asset_id（../../）当前实现因 png 查找落空而偶然跳过，
        // 显式格式校验把"偶然安全"变"必然安全"
        let dir = tmp_assets_dir("evilid");
        let reg = AvatarRegistry::load(&dir);
        let a = good_upload(&reg, "ok-asset");
        let png = solid_png(8, 8, 255);
        // 短 id（av_+5 字符，非 16hex）：同位 png 存在，现状会加载 -> 期望跳过
        std::fs::write(dir.join("av_short.png"), &png).unwrap();
        std::fs::write(
            dir.join("av_short.json"),
            r#"{"asset_id":"av_short","name":"x","height_world":2.0,"w":8,"h":8,"created_tick":0}"#,
        )
        .unwrap();
        // 穿越 id：显式校验后必跳过（现状偶然跳过——png 落空）
        std::fs::write(
            dir.join("av_deadbeefdeadbeef.json"),
            r#"{"asset_id":"../../evil","name":"x","height_world":2.0,"w":8,"h":8,"created_tick":0}"#,
        )
        .unwrap();
        let reg2 = AvatarRegistry::load(&dir);
        let list = reg2.list();
        assert_eq!(list.len(), 1, "只有合法资产在册");
        assert_eq!(list[0].asset_id, a.asset_id, "违规 asset_id 条目被跳过");
    }

    #[test]
    fn registry_load_rebuilds_and_skips_corrupt() {
        // 规则：启动扫目录重建——成对 png+json 才收；坏 json/孤儿 png warn 跳过不崩
        let dir = tmp_assets_dir("load");
        // 先上传两张到目录（真实落盘形态）
        let reg = AvatarRegistry::load(&dir);
        let a1 = good_upload(&reg, "alpha");
        let a2 = good_upload(&reg, "beta");
        assert_eq!(reg.list().len(), 2);
        // 手工制造坏文件：坏 json + 孤儿 png
        std::fs::write(dir.join("av_deadbeefdeadbeef.json"), "{not json").unwrap();
        let png = solid_png(8, 8, 255);
        std::fs::write(dir.join("av_deadbeefdeadbeef.png"), &png).unwrap();
        // 重建（新登记表对象 = 模拟重启）
        let reg2 = AvatarRegistry::load(&dir);
        let list = reg2.list();
        assert_eq!(list.len(), 2, "两张合法资产重建，坏文件跳过");
        let ids: Vec<&str> = list.iter().map(|a| a.asset_id.as_str()).collect();
        assert!(ids.contains(&a1.asset_id.as_str()), "资产 1 在册");
        assert!(ids.contains(&a2.asset_id.as_str()), "资产 2 在册");
        assert!(
            list.iter().all(|a| (a.mask.w, a.mask.h) == (128, 128)),
            "重建条目掩码尺寸齐整"
        );
    }

    #[test]
    fn mask_error_maps_to_reject_reason() {
        // 规则：MaskError 三变体 -> RejectReason 映射（decode_failed/oversize_dim/isolate_fail）
        assert_eq!(
            RejectReason::from(MaskError::Decode("x".into())),
            RejectReason::DecodeFailed
        );
        assert_eq!(
            RejectReason::from(MaskError::TooLarge { w: 1025, h: 4 }),
            RejectReason::OversizeDim
        );
        assert_eq!(RejectReason::from(MaskError::Isolate), RejectReason::IsolateFail);
    }

    #[test]
    #[ignore = "一次性 fixtures 生成器：跑 cargo test -p codeworld --release -- --ignored generate_avatar_fixture 生成 tests/fixtures/test-avatar.png"]
    fn generate_avatar_fixture() {
        // D-017 验收 fixtures：64x64 透明底角色图形态——实心圆主体（半径 22，
        // 模拟"生图角色"）+ 右上 1 像素小碎片（< N=8，上传收图时碎片静默丢弃）。
        // 注意坑：64x64 上采样到 128 表是 2 倍放大——每源像素变 2x2=4 表格，
        // 若用 2x1 碎片会变 8 格恰好 >= N=8 被当断件打回（N 待拍板前的既定语义）
        // 编码确定性（png crate 无随机），重复生成字节一致；生成后自验上传链路
        let w = 64u32;
        let h = 64u32;
        let mut px = vec![0u8; (w * h * 4) as usize];
        let (cx, cy, r) = (32.0f64, 32.0f64, 22.0f64);
        for y in 0..h {
            for x in 0..w {
                let dx = x as f64 - cx;
                let dy = y as f64 - cy;
                if dx * dx + dy * dy <= r * r {
                    let i = ((y * w + x) * 4) as usize;
                    px[i] = 200; // 淡红主体
                    px[i + 1] = 60;
                    px[i + 2] = 60;
                    px[i + 3] = 255;
                }
            }
        }
        // 1 像素小碎片（与主体断开；上采样后 4 格 < N=8）
        {
            let i = ((8 * w + 55) * 4) as usize;
            px[i] = 200;
            px[i + 1] = 60;
            px[i + 2] = 60;
            px[i + 3] = 255;
        }
        let png = encode_rgba_png(w, h, &px);
        // 自验：掩码管线应收图（主件保留+1 像素碎片上采样 4 格丢弃）
        let mut mask = decode_png_to_mask(&png).expect("fixture 应可解码");
        let dropped = validate_and_prune(&mut mask).expect("fixture 应通过连通校验");
        assert_eq!(dropped, 4, "恰丢弃 4 格碎片（1 源像素 x2 倍放大）");
        // 落盘到仓库 tests/fixtures/（cargo test CWD=bin/codeworld，向上两级到仓库根）
        let dir = PathBuf::from("../../tests/fixtures");
        std::fs::create_dir_all(&dir).expect("fixtures 目录应可建");
        std::fs::write(dir.join("test-avatar.png"), &png).expect("fixture 应可写");
        tracing::info!(bytes = png.len(), "[avatars] fixture 生成：tests/fixtures/test-avatar.png");
    }
}
