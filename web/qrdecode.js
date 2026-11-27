// QR decoder for the phone scanner. No dependencies.
// Handles versions 1–10 (enough for links), all four error-correction levels,
// numeric / alphanumeric / byte modes. Used when the browser has no BarcodeDetector.
(function () {
  "use strict";
  const EXP = new Uint8Array(512), LOG = new Uint8Array(256);
  for (let i = 0, x = 1; i < 255; i++) { EXP[i] = x; LOG[x] = i; x <<= 1; if (x & 256) x ^= 0x11d; }
  for (let i = 255; i < 512; i++) EXP[i] = EXP[i - 255];
  const mul = (a, b) => (a && b ? EXP[(LOG[a] + LOG[b]) % 255] : 0);

  // [ec, blocks1, data1, blocks2, data2] for levels L M Q H, versions 1–10
  const EC = [
    null,
    [[7, 1, 19, 0, 0], [10, 1, 16, 0, 0], [13, 1, 13, 0, 0], [17, 1, 9, 0, 0]],
    [[10, 1, 34, 0, 0], [16, 1, 28, 0, 0], [22, 1, 22, 0, 0], [28, 1, 16, 0, 0]],
    [[15, 1, 55, 0, 0], [26, 1, 44, 0, 0], [18, 2, 17, 0, 0], [22, 2, 13, 0, 0]],
    [[20, 1, 80, 0, 0], [18, 2, 32, 0, 0], [26, 2, 24, 0, 0], [16, 4, 9, 0, 0]],
    [[26, 1, 108, 0, 0], [24, 2, 43, 0, 0], [18, 2, 15, 2, 16], [22, 2, 11, 2, 12]],
    [[18, 2, 68, 0, 0], [16, 4, 27, 0, 0], [24, 4, 19, 0, 0], [28, 4, 15, 0, 0]],
    [[20, 2, 78, 0, 0], [18, 4, 31, 0, 0], [18, 2, 14, 4, 15], [26, 4, 13, 1, 14]],
    [[24, 2, 97, 0, 0], [22, 2, 38, 2, 39], [22, 4, 18, 2, 19], [26, 4, 14, 2, 15]],
    [[30, 2, 116, 0, 0], [22, 3, 36, 2, 37], [20, 4, 16, 4, 17], [24, 4, 12, 4, 13]],
    [[18, 2, 68, 2, 69], [26, 4, 43, 1, 44], [24, 6, 19, 2, 20], [28, 6, 15, 2, 16]],
  ];
  const ALIGN = [null, [], [6, 18], [6, 22], [6, 26], [6, 30], [6, 34], [6, 22, 38], [6, 24, 42], [6, 26, 46], [6, 28, 50]];
  const MASKS = [
    (r, c) => (r + c) % 2 === 0, (r) => r % 2 === 0, (r, c) => c % 3 === 0, (r, c) => (r + c) % 3 === 0,
    (r, c) => (Math.floor(r / 2) + Math.floor(c / 3)) % 2 === 0,
    (r, c) => ((r * c) % 2) + ((r * c) % 3) === 0,
    (r, c) => (((r * c) % 2) + ((r * c) % 3)) % 2 === 0,
    (r, c) => (((r + c) % 2) + ((r * c) % 3)) % 2 === 0,
  ];
  const ALNUM = "0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZ $%*+-./:";

  function bchFormat(data) {
    let v = data << 10;
    for (let i = 14; i >= 10; i--) if ((v >> i) & 1) v ^= 0x537 << (i - 10);
    return ((data << 10) | (v & 0x3ff)) ^ 0x5412;
  }
  const FORMATS = [];
  for (let i = 0; i < 32; i++) FORMATS.push(bchFormat(i));

  function hamming(a, b) {
    let x = a ^ b, n = 0;
    while (x) { n += x & 1; x >>= 1; }
    return n;
  }

  function syndromes(msg, nsym) {
    const s = [];
    for (let i = 0; i < nsym; i++) {
      let v = 0;
      for (const c of msg) v = mul(v, EXP[i]) ^ c;
      s.push(v);
    }
    return s;
  }

  // Correct up to nsym/2 errors. msg is mutated. Returns false if it cannot.
  function rsFix(msg, nsym) {
    const syn = syndromes(msg, nsym);
    if (syn.every(x => x === 0)) return true;
    let C = [1], B = [1], L = 0, m = 1, b = 1;
    for (let n = 0; n < nsym; n++) {
      let delta = syn[n];
      for (let i = 1; i <= L; i++) delta ^= mul(C[i] || 0, syn[n - i]);
      if (delta === 0) { m++; continue; }
      const T = C.slice();
      const coef = mul(delta, EXP[(255 - LOG[b]) % 255]);
      while (C.length < B.length + m) C.push(0);
      for (let i = 0; i < B.length; i++) C[i + m] ^= mul(coef, B[i]);
      if (2 * L <= n) { L = n + 1 - L; B = T; b = delta; m = 1; }
      else m++;
    }
    if (L > nsym / 2) return false;
    const err = [];
    for (let i = 0; i < msg.length; i++) {
      let v = C[0];
      for (let j = 1; j < C.length; j++) v ^= mul(C[j] || 0, EXP[(j * (msg.length - 1 - i)) % 255]);
      if (v === 0) err.push(i);
    }
    if (!err.length || err.length !== L) return false;
    const omega = [];
    for (let i = 0; i < nsym; i++) {
      let v = 0;
      for (let j = 0; j <= i && j < C.length; j++) v ^= mul(C[j] || 0, syn[i - j]);
      omega.push(v);
    }
    for (const pos of err) {
      const xi = EXP[(msg.length - 1 - pos) % 255];
      const xiInv = EXP[(255 - LOG[xi]) % 255];
      let num = 0;
      for (let i = 0; i < omega.length; i++) num ^= mul(omega[i], EXP[(i * LOG[xiInv]) % 255]);
      let den = 0;
      for (let i = 1; i < C.length; i += 2) den ^= mul(C[i], EXP[((i - 1) * LOG[xiInv]) % 255]);
      if (!den) return false;
      msg[pos] ^= mul(num, EXP[(255 - LOG[den]) % 255]);
    }
    return syndromes(msg, nsym).every(x => x === 0);
  }

  function isFunction(r, c, n, v) {
    const box = (r0, c0) => r >= r0 - 1 && r <= r0 + 7 && c >= c0 - 1 && c <= c0 + 7;
    if (box(0, 0) || box(0, n - 7) || box(n - 7, 0)) return true;
    if (r === 6 || c === 6) return true;
    const al = ALIGN[v];
    for (const ar of al) for (const ac of al) {
      if ((ar === 6 && ac === 6) || (ar === 6 && ac === al[al.length - 1]) || (ar === al[al.length - 1] && ac === 6)) continue;
      if (Math.abs(r - ar) <= 2 && Math.abs(c - ac) <= 2) return true;
    }
    if ((r === 8 && (c <= 8 || c >= n - 8)) || (c === 8 && (r <= 8 || r >= n - 8))) return true;
    if (v >= 7 && ((r < 6 && c >= n - 11) || (c < 6 && r >= n - 11))) return true;
    return false;
  }

  function readFormat(m) {
    const n = m.length;
    const grab = (pairs) => {
      let bits = 0;
      pairs.forEach(([r, c], i) => { if (m[r][c]) bits |= 1 << i; });
      return bits;
    };
    const a = [[0, 8], [1, 8], [2, 8], [3, 8], [4, 8], [5, 8], [7, 8], [8, 8], [8, 7], [8, 5], [8, 4], [8, 3], [8, 2], [8, 1], [8, 0]];
    const b = [];
    for (let i = 0; i < 8; i++) b.push([8, n - 1 - i]);
    for (let i = 0; i < 7; i++) b.push([n - 7 + i, 8]);
    let best = null, bestD = 4;
    for (const raw of [grab(a), grab(b)]) {
      for (let i = 0; i < 32; i++) {
        const d = hamming(raw, FORMATS[i]);
        if (d < bestD) { bestD = d; best = i; }
      }
    }
    return best;
  }

  function extract(m, v, mask) {
    const n = m.length, bits = [];
    let up = true;
    for (let col = n - 1; col > 0; col -= 2) {
      if (col === 6) col--;
      for (let t = 0; t < n; t++) {
        const r = up ? n - 1 - t : t;
        for (let dc = 0; dc < 2; dc++) {
          const c = col - dc;
          if (isFunction(r, c, n, v)) continue;
          let bit = m[r][c] ? 1 : 0;
          if (MASKS[mask](r, c)) bit ^= 1;
          bits.push(bit);
        }
      }
      up = !up;
    }
    const bytes = [];
    for (let i = 0; i + 8 <= bits.length; i += 8) {
      let x = 0;
      for (let b = 0; b < 8; b++) x = (x << 1) | bits[i + b];
      bytes.push(x);
    }
    return bytes;
  }

  function deinterleave(bytes, spec) {
    const [ec, b1, d1, b2, d2] = spec;
    const lens = [];
    for (let i = 0; i < b1; i++) lens.push(d1);
    for (let i = 0; i < b2; i++) lens.push(d2);
    const data = lens.map(d => new Array(d));
    const eccs = lens.map(() => new Array(ec));
    let k = 0;
    const maxD = Math.max(d1, d2);
    for (let i = 0; i < maxD; i++) for (let b = 0; b < lens.length; b++) if (i < lens[b]) data[b][i] = bytes[k++];
    for (let i = 0; i < ec; i++) for (let b = 0; b < lens.length; b++) eccs[b][i] = bytes[k++];
    const out = [];
    for (let b = 0; b < lens.length; b++) {
      const msg = data[b].concat(eccs[b]);
      if (!rsFix(msg, ec)) return null;
      out.push(...msg.slice(0, lens[b]));
    }
    return out;
  }

  function readBits(bytes) {
    const bits = [];
    for (const x of bytes) for (let i = 7; i >= 0; i--) bits.push((x >> i) & 1);
    let p = 0;
    const take = n => { let v = 0; for (let i = 0; i < n; i++) v = (v << 1) | (bits[p++] || 0); return v; };
    return { take };
  }

  function parsePayload(data, v) {
    const { take } = readBits(data);
    let out = "";
    for (;;) {
      const mode = take(4);
      if (mode === 0) break;
      if (mode === 1) {
        const n = take(v < 10 ? 10 : 12);
        let left = n;
        while (left >= 3) { const x = take(10); out += String(x).padStart(3, "0"); left -= 3; }
        if (left === 2) out += String(take(7)).padStart(2, "0");
        else if (left === 1) out += String(take(4));
      } else if (mode === 2) {
        const n = take(v < 10 ? 9 : 11);
        for (let i = 0; i + 1 < n; i += 2) { const x = take(11); out += ALNUM[Math.floor(x / 45)] + ALNUM[x % 45]; }
        if (n % 2) out += ALNUM[take(6)];
      } else if (mode === 4) {
        const n = take(v < 10 ? 8 : 16);
        const raw = [];
        for (let i = 0; i < n; i++) raw.push(take(8));
        out += new TextDecoder().decode(new Uint8Array(raw));
      } else return null;
    }
    return out || null;
  }

  function decodeMatrix(m) {
    const n = m.length;
    const ver = (n - 17) / 4;
    if (ver < 1 || ver > 10 || ver !== (ver | 0)) return null;
    const fmt = readFormat(m);
    if (fmt == null) return null;
    const level = fmt >> 3, mask = fmt & 7;
    const bytes = extract(m, ver, mask);
    const data = deinterleave(bytes, EC[ver][level]);
    if (!data) return null;
    return parsePayload(data, ver);
  }

  function finders(bin, w, h) {
    const hits = [];
    const rowCounts = (y, x0) => {
      const runs = [];
      let x = x0, bit = bin[y * w + x];
      while (x < w && runs.length < 5) {
        let n = 0;
        while (x < w && bin[y * w + x] === bit) { n++; x++; }
        runs.push(n); bit ^= 1;
      }
      return runs.length === 5 ? runs : null;
    };
    for (let y = 0; y < h; y++) {
      let x = 0;
      while (x < w) {
        if (!bin[y * w + x]) { x++; continue; }
        const runs = rowCounts(y, x);
        if (!runs) { x++; continue; }
        const total = runs[0] + runs[1] + runs[2] + runs[3] + runs[4];
        const mod = total / 7;
        const ok = [1, 1, 3, 1, 1].every((e, i) => Math.abs(runs[i] / mod - e) < 0.7);
        const cx = x + runs[0] + runs[1] + runs[2] / 2;
        if (ok && cx < w) {
          let up = 0, yy = y;
          while (yy >= 0 && bin[yy * w + (cx | 0)]) { up++; yy--; }
          let down = 0; yy = y + 1;
          while (yy < h && bin[yy * w + (cx | 0)]) { down++; yy++; }
          if (Math.abs(up + down - runs[2]) < mod) hits.push({ x: cx, y, mod });
        }
        x += Math.max(1, runs[0]);
      }
    }
    const groups = [];
    for (const h1 of hits) {
      const g = groups.find(g => Math.hypot(g.x - h1.x, g.y - h1.y) < h1.mod * 3);
      if (g) { g.x = (g.x * g.n + h1.x) / (g.n + 1); g.y = (g.y * g.n + h1.y) / (g.n + 1); g.mod = (g.mod * g.n + h1.mod) / (g.n + 1); g.n++; }
      else groups.push({ x: h1.x, y: h1.y, mod: h1.mod, n: 1 });
    }
    return groups.filter(g => g.n >= 2).sort((a, b) => b.n - a.n).slice(0, 3);
  }

  function sample(bin, w, h, pts, ver) {
    const n = 17 + 4 * ver;
    const [tl, tr, bl] = pts;
    const span = n - 7;
    const m = [];
    for (let r = 0; r < n; r++) {
      const row = [];
      for (let c = 0; c < n; c++) {
        const u = (c - 3) / span, v = (r - 3) / span;
        const x = tl.x + (tr.x - tl.x) * u + (bl.x - tl.x) * v;
        const y = tl.y + (tr.y - tl.y) * u + (bl.y - tl.y) * v;
        const xi = Math.round(x), yi = Math.round(y);
        row.push(xi >= 0 && yi >= 0 && xi < w && yi < h && bin[yi * w + xi] ? 1 : 0);
      }
      m.push(row);
    }
    return m;
  }

  function order(groups) {
    const d01 = Math.hypot(groups[0].x - groups[1].x, groups[0].y - groups[1].y);
    const d12 = Math.hypot(groups[1].x - groups[2].x, groups[1].y - groups[2].y);
    const d02 = Math.hypot(groups[0].x - groups[2].x, groups[0].y - groups[2].y);
    let tl, a, b;
    if (d01 >= d12 && d01 >= d02) { tl = groups[2]; a = groups[0]; b = groups[1]; }
    else if (d12 >= d02) { tl = groups[0]; a = groups[1]; b = groups[2]; }
    else { tl = groups[1]; a = groups[0]; b = groups[2]; }
    const cross = (a.x - tl.x) * (b.y - tl.y) - (a.y - tl.y) * (b.x - tl.x);
    return cross > 0 ? [tl, a, b] : [tl, b, a];
  }

  function tryBin(bin, w, h) {
    const g = finders(bin, w, h);
    if (g.length < 3) return null;
    const pts = order(g);
    const mod = (pts[0].mod + pts[1].mod + pts[2].mod) / 3;
    const dist = Math.hypot(pts[1].x - pts[0].x, pts[1].y - pts[0].y);
    let ver = Math.round((dist / mod - 10) / 4);
    if (ver < 1) ver = 1;
    if (ver > 10) ver = 10;
    for (const v of [ver, ver - 1, ver + 1]) {
      if (v < 1 || v > 10) continue;
      const text = decodeMatrix(sample(bin, w, h, pts, v));
      if (text) return text;
    }
    return null;
  }

  function decodeQrImage(img) {
    const { width: w, height: h, data } = img;
    const gray = new Uint8Array(w * h);
    let sum = 0;
    for (let i = 0, p = 0; i < gray.length; i++, p += 4) {
      gray[i] = (data[p] * 3 + data[p + 1] * 6 + data[p + 2]) / 10;
      sum += gray[i];
    }
    const mean = sum / gray.length;
    for (const t of [mean, mean * 0.85, mean * 1.15]) {
      const bin = new Uint8Array(gray.length);
      for (let i = 0; i < gray.length; i++) bin[i] = gray[i] < t ? 1 : 0;
      const text = tryBin(bin, w, h);
      if (text) return text;
    }
    return null;
  }

  window.decodeQrImage = decodeQrImage;
  window.decodeQrMatrix = decodeMatrix;
})();
