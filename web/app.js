// Alex web (mobile). Same encrypted format as the Alex desktop app:
//   "ALXS1" | frames [u32 BE len][AES-256-GCM ciphertext+tag]
//   frame 0 = JSON {name,size,mime} (AAD "meta"); data frames of 1 MiB (AAD [1] on the last, else [0])
//   IV = 4 zero bytes + u64 BE frame index.
"use strict";
(function () {
  const CFG = window.ALEX_CONFIG || {};
  const API = String(CFG.API_BASE || "").replace(/\/+$/, "");
  const SITE = String(CFG.SITE_URL || location.origin).replace(/\/+$/, "");
  const CHUNK = 1 << 20;
  const $ = id => document.getElementById(id);
  const enc = new TextEncoder(), dec = new TextDecoder();

  // ------------------------------------------------------------ helpers
  const store = {
    get(k, d) { try { const v = localStorage.getItem(k); return v === null ? d : JSON.parse(v); } catch (_) { return d; } },
    set(k, v) { try { localStorage.setItem(k, JSON.stringify(v)); } catch (_) {} },
  };
  const human = n => n < 1024 ? n + " B" : n < 1048576 ? (n / 1024).toFixed(0) + " KB" : n < 1073741824 ? (n / 1048576).toFixed(1) + " MB" : (n / 1073741824).toFixed(2) + " GB";
  const b64u = bytes => btoa(String.fromCharCode(...bytes)).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "");
  const unb64u = s => Uint8Array.from(atob(s.replace(/-/g, "+").replace(/_/g, "/") + "===".slice((s.length + 3) % 4)), c => c.charCodeAt(0));
  const extOf = name => (name.includes(".") ? name.split(".").pop().slice(0, 4).toUpperCase() : "FILE");
  const esc = s => String(s).replace(/[&<>"']/g, c => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" }[c]));
  let toastT;
  function toast(t) { const el = $("toast"); el.textContent = t; el.classList.add("on"); clearTimeout(toastT); toastT = setTimeout(() => el.classList.remove("on"), 2200); }
  function msg(id, t, cls) { const el = $(id); el.textContent = t || ""; el.className = "msg " + (cls || ""); }
  function left(expires) {
    const s = Math.max(0, expires - Date.now() / 1000);
    if (s <= 0) return "đã hết hạn";
    return s >= 3600 ? "còn " + Math.floor(s / 3600) + " giờ " + Math.floor((s % 3600) / 60) + " phút" : "còn " + Math.max(1, Math.floor(s / 60)) + " phút";
  }
  async function copy(text) {
    try { await navigator.clipboard.writeText(text); toast("Đã chép"); }
    catch (_) { const t = document.createElement("textarea"); t.value = text; document.body.appendChild(t); t.select(); document.execCommand("copy"); t.remove(); toast("Đã chép"); }
  }
  function needSecure() {
    if (!window.isSecureContext || !crypto.subtle) throw new Error("Cần mở trang qua HTTPS để mã hoá / giải mã");
  }
  function iv(i) { const b = new Uint8Array(12); new DataView(b.buffer).setBigUint64(4, BigInt(i)); return b; }

  // ------------------------------------------------------------ navigation
  function show(page) {
    for (const p of ["send", "recv", "hist", "set"]) $("page-" + p).classList.toggle("hidden", p !== page);
    document.querySelectorAll("nav button").forEach(b => b.classList.toggle("on", b.dataset.page === page));
    if (page === "hist") renderHistory();
    window.scrollTo(0, 0);
  }
  document.querySelectorAll("nav button").forEach(b => b.addEventListener("click", () => show(b.dataset.page)));

  // ------------------------------------------------------------ connected PCs (sessions)
  // devices: [{id, name, token, online}]; one browser can be connected to several PCs
  let maxBytes = 0;
  let devices = store.get("devices", []);
  let account = store.get("account", null); // {user, token}
  try { localStorage.removeItem("token"); } catch (_) {} // old shared access token, no longer used
  const saveDevices = () => store.set("devices", devices.map(d => ({ id: d.id, name: d.name, token: d.token })));
  const saveAccount = () => store.set("account", account);
  const accountAuth = () => account ? { Authorization: "Account " + account.token, "Content-Type": "application/json" } : {};
  const prettyId = id => String(id).replace(/(\d{3})(?=\d)/g, "$1 ");
  const authOf = d => ({ Authorization: "Session " + d.token });

  async function refreshDevices() {
    try {
      const r = await fetch(API + "/api/ping");
      maxBytes = (await r.json()).max_bytes || 0;
    } catch (_) {
      $("dot").className = "dot bad"; $("statusText").textContent = "Mất kết nối"; return;
    }
    await Promise.all(devices.map(async d => {
      try {
        const r = await fetch(API + "/api/linked", { headers: authOf(d) });
        if (r.status === 401) { d.expired = true; d.online = false; return; }
        const j = await r.json(); d.name = j.name; d.online = j.online; d.expired = false;
      } catch (_) { d.online = false; }
    }));
    saveDevices();
    const on = devices.filter(d => d.online).length;
    $("dot").className = "dot " + (devices.length ? (on ? "ok" : "") : "");
    $("statusText").textContent = devices.length ? `${on}/${devices.length} máy đang bật` : "Chưa kết nối máy";
    renderDevices(); renderTargets();
  }

  function renderDevices() {
    const el = $("devices");
    if (!devices.length) { el.innerHTML = `<div class="empty">Chưa kết nối máy tính nào</div>`; return; }
    el.innerHTML = "";
    devices.forEach(d => {
      const row = document.createElement("div");
      row.className = "hitem";
      row.innerHTML = `<span class="dot ${d.online ? "ok" : ""}"></span>
        <div class="meta"><div class="name">${esc(d.name || "Máy tính")}</div>
        <div class="size">ID ${esc(prettyId(d.id))} · ${d.expired ? "cần kết nối lại (mật khẩu đã đổi)" : d.online ? "đang bật" : "đang tắt"}</div></div>
        <button class="btn ghost small">Ngắt</button>`;
      row.querySelector("button").onclick = async () => {
        if (!confirm(`Ngắt kết nối với “${d.name}”?`)) return;
        fetch(API + "/api/logout", { method: "POST", headers: authOf(d) }).catch(() => {});
        if (account) fetch(API + "/api/account/unlink", { method: "POST", headers: accountAuth(), body: JSON.stringify({ id: d.id }) }).catch(() => {});
        devices = devices.filter(x => x !== d); saveDevices(); refreshDevices();
      };
      el.appendChild(row);
    });
  }

  $("addDevice").onclick = () => { $("addForm").classList.toggle("hidden"); $("devId").focus(); };
  $("phoneName").value = store.get("phoneName", /iphone/i.test(navigator.userAgent) ? "iPhone" : /ipad/i.test(navigator.userAgent) ? "iPad" : /android/i.test(navigator.userAgent) ? "Điện thoại Android" : "Trình duyệt");
  $("devId").addEventListener("input", e => { const v = e.target.value.replace(/\D/g, "").slice(0, 9); e.target.value = prettyId(v); });
  $("connectBtn").onclick = async () => {
    const id = $("devId").value.replace(/\D/g, ""), password = $("devPw").value.trim(), name = $("phoneName").value.trim() || "Điện thoại";
    if (id.length !== 9 || !password) { msg("connectMsg", "Nhập đủ ID 9 số và mật khẩu", "bad"); return; }
    $("connectBtn").disabled = true; msg("connectMsg", "Đang kết nối…");
    try {
      const url = account ? "/api/account/link" : "/api/login";
      const headers = account ? accountAuth() : { "Content-Type": "application/json" };
      const r = await fetch(API + url, { method: "POST", headers, body: JSON.stringify({ id, password, name }) });
      const j = await r.json().catch(() => ({}));
      if (r.status === 401 && account && url.endsWith("/link")) throw new Error(j.error || "Tài khoản đã hết hạn, hãy đăng nhập lại");
      if (!r.ok) throw new Error(j.error || "Không kết nối được");
      devices = devices.filter(d => d.id !== j.device.id);
      devices.push({ id: j.device.id, name: j.device.name, token: j.token, online: j.device.online });
      saveDevices(); store.set("phoneName", name);
      $("devId").value = ""; $("devPw").value = ""; $("addForm").classList.add("hidden");
      msg("connectMsg", "");
      toast(`Đã kết nối ${j.device.name}`);
      target = j.device.id;
      refreshDevices();
    } catch (e) { msg("connectMsg", e.message, "bad"); }
    finally { $("connectBtn").disabled = false; }
  };

  function renderAccount() {
    const inn = !!account;
    $("accountOut").classList.toggle("hidden", inn);
    $("accountIn").classList.toggle("hidden", !inn);
    if (account) $("accName").textContent = account.user;
  }
  function takeDeviceRows(rows) {
    for (const row of rows || []) {
      const cur = devices.find(d => d.id === row.id);
      if (row.reauth) {
        if (cur) { cur.expired = true; cur.online = false; cur.name = row.name || cur.name; }
        else devices.push({ id: row.id, name: row.name, token: "", online: false, expired: true });
        continue;
      }
      if (row.token) {
        if (cur) { cur.token = row.token; cur.name = row.name; cur.expired = false; cur.online = !!row.online; }
        else devices.push({ id: row.id, name: row.name, token: row.token, online: !!row.online, expired: false });
      } else if (cur) {
        cur.name = row.name || cur.name;
        cur.online = !!row.online;
        cur.expired = false;
      }
    }
    saveDevices();
  }
  async function accountCall(path, body) {
    const r = await fetch(API + path, { method: "POST", headers: account ? accountAuth() : { "Content-Type": "application/json" }, body: JSON.stringify(body || {}) });
    const j = await r.json().catch(() => ({}));
    if (r.status === 401 && account && path !== "/api/account/login") {
      account = null; saveAccount(); renderAccount();
      throw new Error("Phiên tài khoản đã hết, hãy đăng nhập lại");
    }
    if (!r.ok) throw new Error(j.error || "Không thực hiện được");
    return j;
  }
  async function syncAccount() {
    if (!account) return;
    const have = devices.filter(d => d.token && !d.expired).map(d => d.id);
    const j = await accountCall("/api/account/sync", { have });
    takeDeviceRows(j.devices);
  }
  async function enterAccount(path) {
    const user = $("accUser").value.trim().toLowerCase();
    const password = $("accPw").value;
    if (!user || password.length < 8) { msg("accMsg", "Tên tài khoản và mật khẩu ít nhất 8 ký tự", "bad"); return; }
    $("accLogin").disabled = true; $("accReg").disabled = true; msg("accMsg", "Đang kiểm tra…");
    try {
      const kept = devices.filter(d => d.token && !d.expired).map(d => d.token);
      const j = await accountCall(path, { user, password });
      account = { user: j.user, token: j.token };
      saveAccount();
      takeDeviceRows(j.devices);
      if (kept.length) {
        try { await accountCall("/api/account/adopt", { tokens: kept }); } catch (_) {}
      }
      $("accPw").value = "";
      msg("accMsg", "");
      renderAccount();
      toast(path.endsWith("register") ? "Đã tạo tài khoản" : "Đã đăng nhập");
      refreshDevices();
    } catch (e) { msg("accMsg", e.message, "bad"); }
    finally { $("accLogin").disabled = false; $("accReg").disabled = false; }
  }
  $("accLogin").onclick = () => enterAccount("/api/account/login");
  $("accReg").onclick = () => enterAccount("/api/account/register");
  $("accLogout").onclick = async () => {
    const tok = account;
    account = null; saveAccount(); renderAccount();
    if (tok) fetch(API + "/api/account/logout", { method: "POST", headers: { Authorization: "Account " + tok.token } }).catch(() => {});
    toast("Đã đăng xuất. Máy trên điện thoại này vẫn kết nối");
  };
  renderAccount();

  // send target: a PC id, or "link" for link / 6-digit code
  let target = store.get("target", "");
  function renderTargets() {
    const live = devices.filter(d => !d.expired);
    if (target !== "link" && !live.some(d => d.id === target)) target = live.length ? live[0].id : "link";
    const el = $("targets");
    el.innerHTML = "";
    for (const d of live) {
      const c = document.createElement("button");
      c.className = "chip" + (target === d.id ? " on" : "");
      c.innerHTML = `<span class="dot ${d.online ? "ok" : ""}"></span>→ ${esc(d.name)}`;
      c.onclick = () => { target = d.id; store.set("target", target); renderTargets(); };
      el.appendChild(c);
    }
    const l = document.createElement("button");
    l.className = "chip" + (target === "link" ? " on" : "");
    l.textContent = "🔗 Link / mã";
    l.onclick = () => { target = "link"; store.set("target", target); renderTargets(); };
    el.appendChild(l);
    if (!live.length) {
      const a = document.createElement("button");
      a.className = "chip"; a.textContent = "+ Kết nối máy tính";
      a.onclick = () => { show("set"); $("addForm").classList.remove("hidden"); };
      el.appendChild(a);
    }
    $("codeSwitch").classList.toggle("hidden", target !== "link");
  }

  // ------------------------------------------------------------ crypto
  async function encryptFile(file, onProgress) {
    needSecure();
    const raw = crypto.getRandomValues(new Uint8Array(32));
    const key = await crypto.subtle.importKey("raw", raw, "AES-GCM", false, ["encrypt"]);
    const parts = [enc.encode("ALXS1")];
    const frame = async (plain, idx, aad) => {
      const ct = new Uint8Array(await crypto.subtle.encrypt({ name: "AES-GCM", iv: iv(idx), additionalData: aad, tagLength: 128 }, key, plain));
      const len = new Uint8Array(4); new DataView(len.buffer).setUint32(0, ct.length);
      parts.push(len, ct);
    };
    const meta = { name: file.name || "file", size: file.size, mime: file.type || "application/octet-stream" };
    await frame(enc.encode(JSON.stringify(meta)), 0, enc.encode("meta"));
    const frames = Math.max(1, Math.ceil(file.size / CHUNK));
    for (let i = 0; i < frames; i++) {
      const piece = new Uint8Array(await file.slice(i * CHUNK, Math.min(file.size, (i + 1) * CHUNK)).arrayBuffer());
      await frame(piece, i + 1, new Uint8Array([i === frames - 1 ? 1 : 0]));
      onProgress && onProgress((i + 1) / frames);
    }
    return { blob: new Blob(parts, { type: "application/octet-stream" }), key: b64u(raw) };
  }

  async function decryptBlob(buf, keyB64, onProgress) {
    needSecure();
    if (dec.decode(buf.subarray(0, 5)) !== "ALXS1") throw new Error("File không đúng định dạng Alex");
    const raw = unb64u(keyB64);
    if (raw.length !== 32) throw new Error("Khoá giải mã không đúng");
    const key = await crypto.subtle.importKey("raw", raw, "AES-GCM", false, ["decrypt"]);
    const dv = new DataView(buf.buffer, buf.byteOffset, buf.byteLength);
    let pos = 5, idx = 0, meta = null;
    const parts = [];
    while (pos < buf.length) {
      const len = dv.getUint32(pos), start = pos + 4, end = start + len;
      if (end > buf.length) throw new Error("File bị cắt cụt");
      const aad = idx === 0 ? enc.encode("meta") : new Uint8Array([end === buf.length ? 1 : 0]);
      let plain;
      try { plain = await crypto.subtle.decrypt({ name: "AES-GCM", iv: iv(idx), additionalData: aad, tagLength: 128 }, key, buf.subarray(start, end)); }
      catch (_) { throw new Error("Sai khoá hoặc file đã bị sửa đổi"); }
      if (idx === 0) meta = JSON.parse(dec.decode(plain)); else parts.push(plain);
      pos = end; idx++;
      onProgress && onProgress(pos / buf.length);
    }
    if (!meta || idx < 2) throw new Error("File không đầy đủ");
    const name = String(meta.name || "file").replace(/[\\/:*?"<>|]/g, "_");
    return new File(parts, name, { type: meta.mime || "application/octet-stream" });
  }

  // ------------------------------------------------------------ zip (store) for several files
  const CRC = (() => { const t = new Uint32Array(256); for (let n = 0; n < 256; n++) { let c = n; for (let k = 0; k < 8; k++) c = c & 1 ? 0xedb88320 ^ (c >>> 1) : c >>> 1; t[n] = c >>> 0; } return t; })();
  function crc32(u8, c = 0xffffffff) { for (let i = 0; i < u8.length; i++) c = CRC[(c ^ u8[i]) & 255] ^ (c >>> 8); return c; }
  async function zipFiles(files) {
    const out = [], central = [];
    let offset = 0;
    const used = new Set();
    for (const f of files) {
      let name = f.name || "file", n = 1;
      while (used.has(name)) { const d = (f.name || "file").lastIndexOf("."); name = d > 0 ? f.name.slice(0, d) + ` (${n})` + f.name.slice(d) : `${f.name} (${n})`; n++; }
      used.add(name);
      const data = new Uint8Array(await f.arrayBuffer());
      const crc = (crc32(data) ^ 0xffffffff) >>> 0;
      const nb = enc.encode(name);
      const hdr = (central_) => {
        const h = new DataView(new ArrayBuffer(central_ ? 46 : 30));
        let p = 0; const u32 = v => { h.setUint32(p, v, true); p += 4; }, u16 = v => { h.setUint16(p, v, true); p += 2; };
        u32(central_ ? 0x02014b50 : 0x04034b50); if (central_) u16(20); u16(20); u16(0x0800); u16(0); u16(0); u16(0x21);
        u32(crc); u32(data.length); u32(data.length); u16(nb.length); u16(0);
        if (central_) { u16(0); u16(0); u16(0); u32(0); u32(offset); }
        return new Uint8Array(h.buffer);
      };
      const local = hdr(false);
      central.push(hdr(true), nb);
      out.push(local, nb, data);
      offset += local.length + nb.length + data.length;
    }
    const cdSize = central.reduce((s, x) => s + x.length, 0);
    const end = new DataView(new ArrayBuffer(22));
    end.setUint32(0, 0x06054b50, true); end.setUint16(8, files.length, true); end.setUint16(10, files.length, true);
    end.setUint32(12, cdSize, true); end.setUint32(16, offset, true);
    return new File([...out, ...central, new Uint8Array(end.buffer)], `Alex ${files.length} tệp.zip`, { type: "application/zip" });
  }

  // ------------------------------------------------------------ send
  let queue = [];
  let textMode = false;
  function renderQueue() {
    const q = $("queue");
    q.innerHTML = "";
    queue.forEach((f, i) => {
      const row = document.createElement("div");
      row.className = "qitem";
      const thumb = f.type.startsWith("image/") ? `<img alt="" src="${URL.createObjectURL(f)}">` : `<div class="ext">${esc(extOf(f.name))}</div>`;
      row.innerHTML = `${thumb}<div class="meta"><div class="name">${esc(f.name)}</div><div class="size">${human(f.size)}</div></div><button class="x" aria-label="Bỏ">×</button>`;
      row.querySelector("button").onclick = () => { queue.splice(i, 1); renderQueue(); };
      q.appendChild(row);
    });
    updateSendBtn();
  }
  function updateSendBtn() {
    const has = queue.length > 0 || (textMode && $("textBox").value.trim());
    $("sendBtn").disabled = !has;
    $("sendBtn").textContent = queue.length > 1 ? `Tải lên ${queue.length} tệp` : "Tải lên";
  }
  for (const id of ["pickMedia", "pickFile", "pickCamera"]) {
    $(id).addEventListener("change", e => { queue.push(...e.target.files); e.target.value = ""; renderQueue(); });
  }
  $("pickText").addEventListener("click", () => { textMode = !textMode; $("textBox").classList.toggle("hidden", !textMode); if (textMode) $("textBox").focus(); updateSendBtn(); });
  $("textBox").addEventListener("input", updateSendBtn);
  $("withCode").checked = store.get("withCode", true);
  $("withCode").addEventListener("change", e => store.set("withCode", e.target.checked));

  function upload(blob, keyB64, opts, onProgress) {
    return new Promise((resolve, reject) => {
      const x = new XMLHttpRequest();
      x.open("POST", API + "/api/upload");
      x.setRequestHeader("Authorization", "Session " + opts.device.token);
      x.setRequestHeader("Content-Type", "application/octet-stream");
      if (opts.toPc) { x.setRequestHeader("X-Alex-To", "pc"); x.setRequestHeader("X-Alex-Label", encodeURIComponent(opts.name)); }
      if (opts.toPc || opts.withCode) x.setRequestHeader("X-Alex-Key", keyB64);
      x.upload.onprogress = e => e.lengthComputable && onProgress(e.loaded / e.total);
      x.onload = () => {
        let j = {};
        try { j = JSON.parse(x.responseText); } catch (_) {}
        if (x.status === 200) resolve(j); else reject(new Error(j.error || "Máy chủ trả lỗi " + x.status));
      };
      x.onerror = () => reject(new Error("Không kết nối được máy chủ"));
      x.send(blob);
    });
  }

  $("sendBtn").addEventListener("click", async () => {
    const live = devices.filter(d => !d.expired);
    if (!live.length) { toast("Kết nối một máy tính trước (để có quyền tải lên)"); show("set"); $("addForm").classList.remove("hidden"); return; }
    const toPc = target !== "link";
    const device = toPc ? live.find(d => d.id === target) : live[0];
    $("sendBtn").disabled = true;
    $("sendBar").classList.remove("hidden");
    const fill = p => ($("sendFill").style.width = (p * 100).toFixed(1) + "%");
    try {
      let file;
      if (queue.length > 1) { msg("sendMsg", "Đang gói các tệp…"); file = await zipFiles(queue); }
      else if (queue.length === 1) file = queue[0];
      else {
        const d = new Date(), p = n => String(n).padStart(2, "0");
        file = new File([$("textBox").value], `Ghi chu ${p(d.getHours())}${p(d.getMinutes())}.txt`, { type: "text/plain;charset=utf-8" });
      }
      if (maxBytes && file.size > maxBytes) throw new Error(`File quá lớn (tối đa ${human(maxBytes)})`);
      msg("sendMsg", "Đang mã hoá…");
      const { blob, key } = await encryptFile(file, p => fill(p * 0.3));
      msg("sendMsg", "Đang tải lên…");
      const withCode = $("withCode").checked;
      const r = await upload(blob, key, { device, toPc, withCode, name: file.name }, p => fill(0.3 + p * 0.7));
      if (toPc) {
        queue = []; textMode = false; $("textBox").value = ""; $("textBox").classList.add("hidden"); renderQueue();
        msg("sendMsg", "");
        $("sendForm").classList.add("hidden"); $("sentPc").classList.remove("hidden");
        $("sentPcTitle").textContent = `Đã gửi sang ${device.name}`;
        $("sentPcSub").textContent = `${file.name} · ${human(file.size)}` + (device.online ? " · Alex sẽ tự lưu vào Downloads\\Alex" : " · máy đang tắt, sẽ nhận khi bật Alex (trong 24 giờ)");
        return;
      }
      const item = { id: r.id, code: r.code, del: r.delete_token, expires: r.expires, name: file.name, size: file.size, link: `${SITE}/#s=${r.id}.${key}`, at: Date.now() / 1000 };
      const hist = store.get("history", []); hist.unshift(item); store.set("history", hist.slice(0, 50));
      queue = []; textMode = false; $("textBox").value = ""; $("textBox").classList.add("hidden"); renderQueue();
      msg("sendMsg", "");
      showResult(item);
    } catch (e) {
      msg("sendMsg", e.message || String(e), "bad");
    } finally {
      $("sendBar").classList.add("hidden"); fill(0); updateSendBtn();
    }
  });

  let current = null;
  function showResult(item) {
    current = item;
    $("sendForm").classList.add("hidden");
    $("sendResult").classList.remove("hidden");
    $("resTitle").textContent = item.name;
    $("resCodeWrap").classList.toggle("hidden", !item.code);
    $("resCode").textContent = item.code || "";
    try { $("resQr").innerHTML = window.qrSvg(item.link, 200); } catch (_) { $("resQr").innerHTML = ""; }
    $("resLink").textContent = item.link;
    $("resExpire").textContent = `${human(item.size)} · ${left(item.expires)}`;
    show("send");
    copy(item.link).catch(() => {});
  }
  $("copyLink").onclick = () => current && copy(current.link);
  $("shareLink").onclick = async () => {
    if (!current) return;
    const text = current.code ? `Nhận file “${current.name}”: mã ${current.code}` : `Nhận file “${current.name}”`;
    if (navigator.share) { try { await navigator.share({ title: "Alex", text, url: current.link }); } catch (_) {} }
    else copy(current.link);
  };
  $("sentPcMore").onclick = () => { $("sentPc").classList.add("hidden"); $("sendForm").classList.remove("hidden"); };
  $("sendMore").onclick = () => { $("sendResult").classList.add("hidden"); $("sendForm").classList.remove("hidden"); };
  $("deleteNow").onclick = async () => {
    if (!current || !confirm("Xoá file này khỏi máy chủ ngay? Link và mã sẽ không dùng được nữa.")) return;
    await deleteItem(current);
    $("sendMore").onclick();
  };
  async function deleteItem(item) {
    try {
      const r = await fetch(`${API}/api/blob/${item.id}`, { method: "DELETE", headers: { "X-Delete-Token": item.del } });
      if (!r.ok && r.status !== 404) throw new Error("Máy chủ trả lỗi " + r.status);
      store.set("history", store.get("history", []).filter(h => h.id !== item.id));
      toast("Đã xoá");
    } catch (e) { toast(e.message); }
  }

  // ------------------------------------------------------------ QR scan / send a link to a PC
  // A link travels like a tiny file "Link.alexurl" (mime text/x-alex-url) to the PC inbox;
  // Alex shows it on the island with an "Open" button instead of saving it.
  let qrStream = null, qrTimer = null, qrTarget = null;
  const isWeb = t => /^https?:\/\/\S+$/i.test(t.trim());
  function stopCam() {
    clearInterval(qrTimer); qrTimer = null;
    if (qrStream) { qrStream.getTracks().forEach(t => t.stop()); qrStream = null; }
  }
  function renderQrTargets() {
    const live = devices.filter(d => !d.expired);
    if (!live.some(d => d.id === qrTarget)) qrTarget = live.some(d => d.id === target) ? target : live[0] && live[0].id;
    const el = $("qrTargets");
    el.innerHTML = "";
    for (const d of live) {
      const c = document.createElement("button");
      c.className = "chip" + (qrTarget === d.id ? " on" : "");
      c.innerHTML = `<span class="dot ${d.online ? "ok" : ""}"></span>→ ${esc(d.name)}`;
      c.onclick = () => { qrTarget = d.id; renderQrTargets(); };
      el.appendChild(c);
    }
    $("qrSend").disabled = !live.length || !$("qrText").value.trim();
    $("qrSend").textContent = live.length ? "Gửi sang máy tính" : "Kết nối máy tính để gửi";
    $("qrOpen").disabled = !isWeb($("qrText").value);
  }
  async function openCamera() {
    const tries = [
      { video: { facingMode: { ideal: "environment" } }, audio: false },
      { video: true, audio: false },
    ];
    let last;
    for (const c of tries) {
      try { return await navigator.mediaDevices.getUserMedia(c); }
      catch (e) { last = e; }
    }
    throw last;
  }
  function camError(e) {
    if (!window.isSecureContext) return "Camera chỉ chạy trên HTTPS.";
    if (e && (e.name === "NotAllowedError" || e.name === "PermissionDeniedError")) return "Bạn chưa cho phép dùng camera.";
    if (e && e.name === "NotFoundError") return "Không thấy camera trên máy này.";
    return "Không mở được camera: " + ((e && e.message) || e || "");
  }
  function onQrText(text) {
    $("qrText").value = text;
    stopCam();
    $("qrCam").classList.add("hidden");
    if (navigator.vibrate) navigator.vibrate(40);
    msg("qrMsg", isWeb(text) ? "Đã quét được link" : "Đã quét được (không phải link web)", "ok");
    renderQrTargets();
  }
  async function openScanner() {
    $("qrSheet").classList.remove("hidden");
    $("qrText").value = ""; msg("qrMsg", "");
    renderQrTargets();
    const can = navigator.mediaDevices && navigator.mediaDevices.getUserMedia;
    $("qrCam").classList.toggle("hidden", !can);
    $("qrNoScan").classList.toggle("hidden", !!can);
    if (!can) return;
    try {
      const det = "BarcodeDetector" in window ? new BarcodeDetector({ formats: ["qr_code"] }) : null;
      qrStream = await openCamera();
      const v = $("qrVideo");
      v.srcObject = qrStream;
      v.muted = true;
      v.playsInline = true;
      v.setAttribute("playsinline", "");
      await v.play();
      const canvas = document.createElement("canvas");
      const ctx = canvas.getContext("2d", { willReadFrequently: true });
      qrTimer = setInterval(async () => {
        try {
          if (det) {
            const codes = await det.detect(v);
            if (codes.length && codes[0].rawValue) onQrText(codes[0].rawValue);
            return;
          }
          if (!window.decodeQrImage || !v.videoWidth) return;
          const scale = Math.min(1, 480 / Math.max(v.videoWidth, v.videoHeight));
          canvas.width = Math.max(1, Math.round(v.videoWidth * scale));
          canvas.height = Math.max(1, Math.round(v.videoHeight * scale));
          ctx.drawImage(v, 0, 0, canvas.width, canvas.height);
          const text = window.decodeQrImage(ctx.getImageData(0, 0, canvas.width, canvas.height));
          if (text) onQrText(text);
        } catch (_) {}
      }, 280);
    } catch (e) {
      $("qrCam").classList.add("hidden");
      $("qrNoScan").classList.remove("hidden");
      msg("qrMsg", camError(e), "bad");
    }
  }
  function closeScanner() { stopCam(); $("qrSheet").classList.add("hidden"); }
  $("pickQr").onclick = openScanner;
  $("qrClose").onclick = closeScanner;
  $("qrText").addEventListener("input", renderQrTargets);
  $("qrCopy").onclick = () => $("qrText").value.trim() && copy($("qrText").value.trim());
  $("qrOpen").onclick = () => { const t = $("qrText").value.trim(); if (isWeb(t)) window.open(t, "_blank", "noopener"); };
  $("qrSend").onclick = async () => {
    const text = $("qrText").value.trim();
    const device = devices.find(d => d.id === qrTarget && !d.expired);
    if (!text || !device) return;
    $("qrSend").disabled = true; msg("qrMsg", "Đang gửi…");
    try {
      const file = new File([text], "Link.alexurl", { type: "text/x-alex-url" });
      const { blob, key } = await encryptFile(file);
      await upload(blob, key, { device, toPc: true, name: file.name }, () => {});
      msg("qrMsg", `Đã gửi sang ${device.name}` + (device.online ? "" : " (máy đang tắt, sẽ hiện khi bật Alex)"), "ok");
      toast("Đã gửi link");
      setTimeout(closeScanner, 900);
    } catch (e) { msg("qrMsg", e.message || String(e), "bad"); }
    finally { renderQrTargets(); }
  };

  // ------------------------------------------------------------ history
  function renderHistory() {
    const now = Date.now() / 1000;
    const hist = store.get("history", []).filter(h => h.expires > now - 86400 * 3);
    store.set("history", hist);
    const el = $("hist");
    if (!hist.length) { el.innerHTML = `<div class="empty">Chưa gửi gì</div>`; return; }
    el.innerHTML = "";
    for (const h of hist) {
      const alive = h.expires > now;
      const row = document.createElement("div");
      row.className = "hitem";
      row.innerHTML = `<div class="ext" style="width:40px;height:40px;border-radius:10px;flex:none">${esc(extOf(h.name))}</div>
        <div class="meta"><div class="name">${esc(h.name)}</div><div class="size">${human(h.size)} · ${left(h.expires)}</div></div>
        ${alive && h.code ? `<div class="c">${esc(h.code)}</div>` : ""}`;
      if (alive) row.onclick = () => showResult(h);
      else row.style.opacity = ".5";
      el.appendChild(row);
    }
  }

  // ------------------------------------------------------------ receive
  async function download(id, onProgress) {
    const r = await fetch(`${API}/api/blob/${id}`);
    if (!r.ok) { let e = "Không tải được file"; try { e = (await r.json()).error || e; } catch (_) {} throw new Error(e); }
    const total = +r.headers.get("Content-Length") || 0;
    if (!r.body || !total) return new Uint8Array(await r.arrayBuffer());
    const buf = new Uint8Array(total);
    const reader = r.body.getReader();
    let got = 0;
    for (;;) {
      const { done, value } = await reader.read();
      if (done) break;
      buf.set(value, got); got += value.length;
      onProgress(got / total);
    }
    if (got !== total) throw new Error("Tải về không đủ dữ liệu");
    return buf;
  }

  function parseLink(s) {
    s = s.trim();
    let m = s.match(/#s=([A-Za-z0-9_-]{22})\.([A-Za-z0-9_-]{43})/); // this web
    if (m) return { id: m[1], key: m[2] };
    m = s.match(/\/s\/([A-Za-z0-9_-]{22})#([A-Za-z0-9_-]{43})/); // desktop / server page link
    if (m) return { id: m[1], key: m[2] };
    return null;
  }

  async function loadOutbox() {
    const live = devices.filter(d => !d.expired);
    const lists = await Promise.all(live.map(async d => {
      try {
        const r = await fetch(API + "/api/outbox", { headers: authOf(d) });
        if (!r.ok) return [];
        return (await r.json()).items.map(x => ({ ...x, from: d.name }));
      } catch (_) { return []; }
    }));
    const all = lists.flat().sort((a, b) => b.created - a.created);
    $("outboxCard").classList.toggle("hidden", !all.length);
    const el = $("outbox");
    el.innerHTML = "";
    for (const x of all) {
      const row = document.createElement("div");
      row.className = "hitem";
      row.innerHTML = `<div class="ext" style="width:40px;height:40px;border-radius:10px;flex:none">${esc(extOf(x.label))}</div>
        <div class="meta"><div class="name">${esc(x.label)}</div><div class="size">${esc(x.from)} · ${human(Math.max(0, x.size - 60))} · ${left(x.expires)}</div></div>
        <button class="btn small">Tải</button>`;
      row.querySelector("button").onclick = () => receive(`#s=${x.id}.${x.key}`);
      el.appendChild(row);
    }
  }
  setInterval(() => { if (!document.hidden && !$("page-recv").classList.contains("hidden")) loadOutbox(); }, 20000);
  document.querySelector('nav button[data-page="recv"]').addEventListener("click", loadOutbox);

  let received = null;
  async function receive(input) {
    $("recvBtn").disabled = true;
    $("recvResult").classList.add("hidden");
    $("recvBar").classList.remove("hidden");
    const fill = p => ($("recvFill").style.width = (p * 100).toFixed(1) + "%");
    try {
      let target = parseLink(input);
      if (!target) {
        const code = input.replace(/\D/g, "");
        if (code.length !== 6) throw new Error("Nhập mã 6 số hoặc dán link");
        msg("recvMsg", "Đang tìm…");
        const r = await fetch(`${API}/api/code/${code}`);
        const j = await r.json().catch(() => ({}));
        if (!r.ok) throw new Error(j.error || "Mã không đúng");
        target = { id: j.id, key: j.key };
      }
      msg("recvMsg", "Đang tải…");
      const buf = await download(target.id, p => fill(p * 0.8));
      msg("recvMsg", "Đang giải mã…");
      const file = await decryptBlob(buf, target.key, p => fill(0.8 + p * 0.2));
      showReceived(file);
      msg("recvMsg", "");
    } catch (e) {
      msg("recvMsg", e.message || String(e), "bad");
    } finally {
      $("recvBar").classList.add("hidden"); fill(0);
      $("recvBtn").disabled = $("codeInput").value.length !== 6;
    }
  }

  async function showReceived(file) {
    received = file;
    const url = URL.createObjectURL(file);
    const pv = $("preview");
    $("copyTextRow").classList.add("hidden");
    if (file.type.startsWith("image/")) pv.innerHTML = `<img alt="" src="${url}">`;
    else if (file.type.startsWith("video/")) pv.innerHTML = `<video src="${url}" controls playsinline></video>`;
    else if (file.type.startsWith("audio/")) pv.innerHTML = `<div class="filecard"><audio src="${url}" controls style="width:100%"></audio></div>`;
    else if (file.type.startsWith("text/") && file.size < 1048576) {
      const text = await file.text();
      pv.innerHTML = `<pre>${esc(text)}</pre>`;
      $("copyTextRow").classList.remove("hidden");
      $("copyText").onclick = () => copy(text);
    } else pv.innerHTML = `<div class="filecard"><div class="ext" style="width:48px;height:48px;border-radius:12px">${esc(extOf(file.name))}</div><div><div style="font-weight:600;word-break:break-all">${esc(file.name)}</div><div style="color:var(--dim);font-size:13px">${human(file.size)}</div></div></div>`;
    const a = $("saveBtn");
    a.href = url; a.download = file.name;
    $("shareFile").classList.toggle("hidden", !(navigator.canShare && navigator.canShare({ files: [file] })));
    $("recvResult").classList.remove("hidden");
    show("recv");
  }
  $("shareFile").onclick = async () => { if (received) { try { await navigator.share({ files: [received], title: received.name }); } catch (_) {} } };
  $("codeInput").addEventListener("input", e => { e.target.value = e.target.value.replace(/\D/g, "").slice(0, 6); $("recvBtn").disabled = e.target.value.length !== 6; if (e.target.value.length === 6) receive(e.target.value); });
  $("recvBtn").onclick = () => receive($("codeInput").value);
  $("pasteLink").onclick = async () => {
    let t = "";
    try { t = await navigator.clipboard.readText(); } catch (_) {}
    if (!parseLink(t)) t = prompt("Dán link nhận file:") || "";
    if (t) receive(t);
  };

  // ------------------------------------------------------------ settings
  $("apiHost").textContent = API.replace(/^https?:\/\//, "");

  // Windows app download (latest.json is published next to this page by the release workflow)
  fetch("latest.json", { cache: "no-store" }).then(r => (r.ok ? r.json() : null)).then(j => {
    if (!j) return;
    $("winVer").textContent = `Phiên bản ${j.version} · ${human(j.size || 0)}`;
    if (j.url) $("winDl").href = j.url;
  }).catch(() => {});

  // install as app
  let deferred = null;
  window.addEventListener("beforeinstallprompt", e => { e.preventDefault(); deferred = e; $("installCard").classList.remove("hidden"); });
  $("installBtn").onclick = async () => { if (!deferred) return; deferred.prompt(); await deferred.userChoice; deferred = null; $("installCard").classList.add("hidden"); };
  if (/iphone|ipad|ipod/i.test(navigator.userAgent) && !navigator.standalone) $("iosHint").style.display = "";
  if ("serviceWorker" in navigator && window.isSecureContext) navigator.serviceWorker.register("sw.js").catch(() => {});

  // ------------------------------------------------------------ start
  renderDevices(); renderTargets();
  syncAccount().catch(() => {}).finally(() => refreshDevices());
  setInterval(() => { if (!document.hidden) refreshDevices(); }, 60000);
  function handleLink() {
    const link = parseLink(location.href);
    if (!link) return;
    show("recv");
    history.replaceState(null, "", location.pathname); // keep the key out of history/screenshots
    receive(`#s=${link.id}.${link.key}`);
  }
  handleLink();
  // a share link opened while the app is already open only changes the #fragment
  window.addEventListener("hashchange", handleLink);
  // test hook (used by automated checks only)
  window.__alex = { encryptFile, decryptBlob, zipFiles, upload, parseLink };
})();
