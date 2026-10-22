#!/usr/bin/env node
// compare-render.js —— 视觉回归对比工具（node 零依赖）
// 用法：node tools/compare-render.js <a.png> <b.png> [分块=16] [阈值=8]
// 输出：尺寸交集、逐像素最大差/平均差、差异>阈值占比、16x16 分块平均色差
// 排行（差异集中区定位）。PNG 支持任意色深 8bit RGB/RGBA/灰度，filter 0-4。
'use strict';
const fs = require('fs');
const zlib = require('zlib');

/* ---------- PNG 解码（最小实现：8bit，colorType 0/2/6，filter 0-4） ---------- */
function decodePNG(file) {
  const buf = fs.readFileSync(file);
  if (buf.readUInt32BE(0) !== 0x89504e47) throw new Error(file + ' 不是 PNG');
  let pos = 8, width = 0, height = 0, bitDepth = 0, colorType = 0;
  const idat = [];
  while (pos < buf.length) {
    const len = buf.readUInt32BE(pos);
    const type = buf.toString('ascii', pos + 4, pos + 8);
    const data = buf.subarray(pos + 8, pos + 8 + len);
    if (type === 'IHDR') {
      width = data.readUInt32BE(0);
      height = data.readUInt32BE(4);
      bitDepth = data[8];
      colorType = data[9];
      if (bitDepth !== 8) throw new Error('仅支持 8bit PNG，得到 ' + bitDepth);
      if (colorType !== 0 && colorType !== 2 && colorType !== 6)
        throw new Error('仅支持灰度0/RGB2/RGBA6，得到 ' + colorType);
    } else if (type === 'IDAT') {
      idat.push(data);
    } else if (type === 'IEND') break;
    pos += 12 + len;
  }
  const ch = colorType === 0 ? 1 : colorType === 2 ? 3 : 4;
  const raw = zlib.inflateSync(Buffer.concat(idat));
  const stride = width * ch;
  const out = Buffer.alloc(width * height * ch);
  // 反滤波（None/Sub/Up/Average/Paeth）
  let p = 0;
  const paeth = (a, b, c) => {
    const pp = a + b - c, pa = Math.abs(pp - a), pb = Math.abs(pp - b), pc = Math.abs(pp - c);
    return pa <= pb && pa <= pc ? a : (pb <= pc ? b : c);
  };
  for (let y = 0; y < height; y++) {
    const filter = raw[p++];
    const row = raw.subarray(p, p + stride);
    const prev = y > 0 ? out.subarray((y - 1) * stride, y * stride) : null;
    const cur = out.subarray(y * stride, (y + 1) * stride);
    for (let x = 0; x < stride; x++) {
      const left = x >= ch ? cur[x - ch] : 0;
      const up = prev ? prev[x] : 0;
      const ul = prev && x >= ch ? prev[x - ch] : 0;
      let v = row[x];
      if (filter === 1) v += left;
      else if (filter === 2) v += up;
      else if (filter === 3) v += (left + up) >> 1;
      else if (filter === 4) v += paeth(left, up, ul);
      cur[x] = v & 255;
    }
    p += stride;
  }
  return { width, height, ch, data: out };
}

/* 转灰度辅助：取 RGB 平均（供分块色差） */
function lum(px, ch) { return (px[0] + px[1] + px[2]) / 3; }

function main() {
  const [, , aPath, bPath, blockArg, thrArg, regionArg] = process.argv;
  if (!aPath || !bPath) {
    console.log('用法: node compare-render.js <a.png> <b.png> [分块=16] [阈值=8] [区域=x,y,w,h]');
    process.exit(2);
  }
  const A = decodePNG(aPath), B = decodePNG(bPath);
  const W = Math.min(A.width, B.width), H = Math.min(A.height, B.height);
  console.log(`A: ${A.width}x${A.height}(${A.ch}ch)  B: ${B.width}x${B.height}(${B.ch}ch)  对比交集: ${W}x${H}`);
  if (A.width !== B.width || A.height !== B.height)
    console.log('⚠ 两图尺寸不同，取交集对比（绝对值仍可参考，热区行列号按交集坐标系）');

  // 区域对比（可选）：背景不同（我们无 Sky）时只对比模型包围盒，数字才有意义
  let RX = 0, RY = 0, RW = W, RH = H;
  if (regionArg) {
    const [x, y, w, h] = regionArg.split(',').map((n) => parseInt(n, 10));
    RX = Math.max(0, x); RY = Math.max(0, y);
    RW = Math.min(w, W - RX); RH = Math.min(h, H - RY);
    console.log(`区域对比: (${RX},${RY}) ${RW}x${RH}`);
  }

  const BLOCK = parseInt(blockArg, 10) || 16;
  const THR = parseInt(thrArg, 10) || 8;
  let maxDiff = 0, sumDiff = 0, over = 0, n = 0;
  const bw = Math.ceil(RW / BLOCK), bh = Math.ceil(RH / BLOCK);
  const blockSum = new Float64Array(bw * bh);
  for (let y = RY; y < RY + RH; y++) {
    for (let x = RX; x < RX + RW; x++) {
      const ia = (y * A.width + x) * A.ch, ib = (y * B.width + x) * B.ch;
      const d = Math.abs(A.data[ia] - B.data[ib])
        + Math.abs(A.data[ia + 1] - B.data[ib + 1])
        + Math.abs(A.data[ia + 2] - B.data[ib + 2]);
      if (d > maxDiff) maxDiff = d;
      sumDiff += d;
      if (d > THR * 3) over++;
      n++;
      blockSum[Math.floor((y - RY) / BLOCK) * bw + Math.floor((x - RX) / BLOCK)] += d;
    }
  }
  console.log(`最大像素差(R+G+B): ${maxDiff}/765`);
  console.log(`平均像素差(R+G+B): ${(sumDiff / n).toFixed(2)} (约 ${(sumDiff / n / 3).toFixed(2)}/通道)`);
  console.log(`差异>阈值(${THR}/通道)占比: ${(over / n * 100).toFixed(2)}%`);
  // 分块热区 Top10（排除边缘 2 块，聚焦画面主体；供人工看图定位差异源）
  const blocks = [];
  for (let by = 0; by < bh; by++)
    for (let bx = 0; bx < bw; bx++)
      blocks.push({ bx, by, avg: blockSum[by * bw + bx] / (BLOCK * BLOCK) });
  blocks.sort((a, b) => b.avg - a.avg);
  console.log(`分块(${BLOCK}px)平均差 Top10（块内平均 R+G+B，排除区域边缘块）:`);
  let shown = 0;
  for (const b of blocks) {
    if (b.bx < 1 || b.by < 1 || b.bx >= bw - 1 || b.by >= bh - 1) continue;
    console.log(`  块(${b.bx},${b.by}) px(${RX + b.bx * BLOCK},${RY + b.by * BLOCK}) 平均差 ${b.avg.toFixed(1)}`);
    if (++shown >= 10) break;
  }
}
main();
