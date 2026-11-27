// Minimal QR code encoder (byte mode, error correction M, versions 1–10) → SVG.
// Enough for share links (~100 characters). No dependencies.
(function () {
  "use strict";
  // [data codewords per block group1, blocks g1, data per block g2, blocks g2, ecc per block] for level M
  const M = [null,
    [16, 1, 0, 0, 10], [28, 1, 0, 0, 16], [44, 1, 0, 0, 26], [32, 2, 0, 0, 18], [43, 2, 0, 0, 24],
    [27, 4, 0, 0, 16], [31, 4, 0, 0, 18], [38, 2, 39, 2, 22], [36, 3, 37, 2, 22], [43, 4, 44, 1, 26]];
  const ALIGN = [null, [], [6, 18], [6, 22], [6, 26], [6, 30], [6, 34], [6, 22, 38], [6, 24, 42], [6, 26, 46], [6, 28, 50]];

  // GF(256), primitive polynomial 0x11D
  const EXP = new Uint8Array(512), LOG = new Uint8Array(256);
  for (let i = 0, x = 1; i < 255; i++) { EXP[i] = x; LOG[x] = i; x <<= 1; if (x & 256) x ^= 0x11d; }
  for (let i = 255; i < 512; i++) EXP[i] = EXP[i - 255];
  const mul = (a, b) => (a && b ? EXP[LOG[a] + LOG[b]] : 0);

  function rsGenerator(n) {
    let g = [1];
    for (let i = 0; i < n; i++) {
      const next = new Array(g.length + 1).fill(0);
      for (let j = 0; j < g.length; j++) { next[j] ^= g[j]; next[j + 1] ^= mul(g[j], EXP[i]); }
      g = next;
    }
    return g;
  }
  function rsEncode(data, n) {
    const g = rsGenerator(n), res = new Array(n).fill(0);
    for (const d of data) {
      const f = d ^ res[0];
      res.shift(); res.push(0);
      for (let j = 0; j < n; j++) res[j] ^= mul(g[j + 1], f);
    }
    return res;
  }

  function dataCodewords(bytes, v) {
    const [d1, b1, d2, b2] = M[v];
    const cap = d1 * b1 + d2 * b2;
    const bits = [];
    const put = (val, len) => { for (let i = len - 1; i >= 0; i--) bits.push((val >> i) & 1); };
    put(4, 4); put(bytes.length, v < 10 ? 8 : 16);
    for (const b of bytes) put(b, 8);
    for (let i = 0; i < 4 && bits.length < cap * 8; i++) bits.push(0);
    while (bits.length % 8) bits.push(0);
    const out = [];
    for (let i = 0; i < bits.length; i += 8) out.push(parseInt(bits.slice(i, i + 8).join(""), 2));
    for (let p = 0; out.length < cap; p++) out.push(p % 2 ? 0x11 : 0xec);
    return out;
  }

  function interleave(data, v) {
    const [d1, b1, d2, b2, ec] = M[v];
    const blocks = [];
    let k = 0;
    for (let i = 0; i < b1; i++) { blocks.push(data.slice(k, k + d1)); k += d1; }
    for (let i = 0; i < b2; i++) { blocks.push(data.slice(k, k + d2)); k += d2; }
    const eccs = blocks.map(b => rsEncode(b, ec));
    const out = [];
    const maxD = Math.max(d1, d2);
    for (let i = 0; i < maxD; i++) for (const b of blocks) if (i < b.length) out.push(b[i]);
    for (let i = 0; i < ec; i++) for (const e of eccs) out.push(e[i]);
    return out;
  }

  function bch(value, poly, polyBits) {
    let v = value << (polyBits - 1);
    for (let i = 31; i >= polyBits - 1; i--) if ((v >> i) & 1) v ^= poly << (i - (polyBits - 1));
    return v;
  }

  function build(bytes, v, mask) {
    const n = 17 + 4 * v;
    const m = Array.from({ length: n }, () => new Int8Array(n).fill(-1)); // -1 = empty
    const fn = Array.from({ length: n }, () => new Uint8Array(n)); // function pattern?
    const set = (r, c, val) => { if (r >= 0 && c >= 0 && r < n && c < n) { m[r][c] = val ? 1 : 0; fn[r][c] = 1; } };
    const finder = (r, c) => {
      for (let i = -1; i <= 7; i++) for (let j = -1; j <= 7; j++) {
        const on = i >= 0 && i <= 6 && j >= 0 && j <= 6 && (i === 0 || i === 6 || j === 0 || j === 6 || (i >= 2 && i <= 4 && j >= 2 && j <= 4));
        set(r + i, c + j, on);
      }
    };
    finder(0, 0); finder(0, n - 7); finder(n - 7, 0);
    for (let i = 8; i < n - 8; i++) { set(6, i, i % 2 === 0); set(i, 6, i % 2 === 0); }
    const al = ALIGN[v];
    for (const r of al) for (const c of al) {
      if ((r === 6 && c === 6) || (r === 6 && c === al[al.length - 1]) || (r === al[al.length - 1] && c === 6)) continue;
      for (let i = -2; i <= 2; i++) for (let j = -2; j <= 2; j++) set(r + i, c + j, Math.max(Math.abs(i), Math.abs(j)) !== 1);
    }
    set(4 * v + 9, 8, 1); // dark module
    // reserve format areas
    for (let i = 0; i < 9; i++) { if (!fn[8][i]) set(8, i, 0); if (!fn[i][8]) set(i, 8, 0); }
    for (let i = 0; i < 8; i++) { set(8, n - 1 - i, 0); set(n - 1 - i, 8, 0); }
    set(4 * v + 9, 8, 1);
    if (v >= 7) {
      const vi = (v << 12) | bch(v, 0x1f25, 13);
      for (let i = 0; i < 18; i++) { const bit = (vi >> i) & 1, a = Math.floor(i / 3), b = (i % 3) + n - 11; set(a, b, bit); set(b, a, bit); }
    }
    // data
    const cw = interleave(dataCodewords(bytes, v), v);
    const bits = [];
    for (const c of cw) for (let i = 7; i >= 0; i--) bits.push((c >> i) & 1);
    const maskFn = [
      (r, c) => (r + c) % 2 === 0, (r) => r % 2 === 0, (r, c) => c % 3 === 0, (r, c) => (r + c) % 3 === 0,
      (r, c) => (Math.floor(r / 2) + Math.floor(c / 3)) % 2 === 0, (r, c) => ((r * c) % 2) + ((r * c) % 3) === 0,
      (r, c) => (((r * c) % 2) + ((r * c) % 3)) % 2 === 0, (r, c) => (((r + c) % 2) + ((r * c) % 3)) % 2 === 0][mask];
    let k = 0, up = true;
    for (let col = n - 1; col > 0; col -= 2) {
      if (col === 6) col--;
      for (let t = 0; t < n; t++) {
        const r = up ? n - 1 - t : t;
        for (let dc = 0; dc < 2; dc++) {
          const c = col - dc;
          if (fn[r][c]) continue;
          let bit = k < bits.length ? bits[k] : 0; k++;
          if (maskFn(r, c)) bit ^= 1;
          m[r][c] = bit;
        }
      }
      up = !up;
    }
    // format info (level M = 00)
    const fmtData = (0 << 3) | mask;
    const fmt = ((fmtData << 10) | bch(fmtData, 0x537, 11)) ^ 0x5412;
    const fb = i => (fmt >> i) & 1;
    // first copy, around the top-left finder
    for (let i = 0; i <= 5; i++) m[i][8] = fb(i);
    m[7][8] = fb(6);
    m[8][8] = fb(7);
    m[8][7] = fb(8);
    for (let i = 9; i < 15; i++) m[8][14 - i] = fb(i);
    // second copy, split between top-right and bottom-left
    for (let i = 0; i < 8; i++) m[8][n - 1 - i] = fb(i);
    for (let i = 8; i < 15; i++) m[n - 15 + i][8] = fb(i);
    m[n - 8][8] = 1; // dark module
    return m;
  }

  function penalty(m) {
    const n = m.length; let p = 0;
    for (let pass = 0; pass < 2; pass++) for (let i = 0; i < n; i++) {
      let run = 1;
      for (let j = 1; j < n; j++) {
        const a = pass ? m[j][i] : m[i][j], b = pass ? m[j - 1][i] : m[i][j - 1];
        if (a === b) { run++; if (run === 5) p += 3; else if (run > 5) p++; } else run = 1;
      }
    }
    for (let i = 0; i < n - 1; i++) for (let j = 0; j < n - 1; j++) { const s = m[i][j] + m[i + 1][j] + m[i][j + 1] + m[i + 1][j + 1]; if (s === 0 || s === 4) p += 3; }
    let dark = 0; for (const row of m) for (const x of row) dark += x;
    p += Math.floor(Math.abs(dark * 20 - n * n * 10) / (n * n)) * 10;
    return p;
  }

  function encode(text) {
    const bytes = Array.from(new TextEncoder().encode(text));
    let v = 1;
    for (; v <= 10; v++) {
      const [d1, b1, d2, b2] = M[v];
      if (bytes.length * 8 + 4 + (v < 10 ? 8 : 16) <= (d1 * b1 + d2 * b2) * 8) break;
    }
    if (v > 10) throw new Error("text too long for QR");
    let best = null, bestP = Infinity;
    for (let mask = 0; mask < 8; mask++) { const m = build(bytes, v, mask); const p = penalty(m); if (p < bestP) { best = m; bestP = p; } }
    return best;
  }

  window.qrSvg = function (text, px) {
    const m = encode(text), n = m.length, q = 4, size = n + 2 * q;
    let d = "";
    for (let r = 0; r < n; r++) for (let c = 0; c < n; c++) if (m[r][c] === 1) d += `M${c + q},${r + q}h1v1h-1z`;
    return `<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 ${size} ${size}" width="${px}" height="${px}" shape-rendering="crispEdges"><rect width="100%" height="100%" fill="#fff"/><path d="${d}" fill="#000"/></svg>`;
  };
  window.qrMatrix = encode;
})();
