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

  // ------------------------------------------------------------ server status
  let maxBytes = 0;
  async function ping() {
    const tok = store.get("token", "");
    try {
      const r = await fetch(API + "/api/ping", { headers: tok ? { "X-Alex-Token": tok } : {} });
      const j = await r.json();
      maxBytes = j.max_bytes || 0;
      $("dot").className = "dot " + (j.auth ? "ok" : "");
      $("statusText").textContent = j.auth ? "Sẵn sàng" : tok ? "Sai mã truy cập" : "Chỉ nhận";
      return j;
    } catch (_) {
      $("dot").className = "dot bad";
      $("statusText").textContent = "Mất kết nối";
      return null;
    }
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

  function upload(blob, keyB64, withCode, onProgress) {
    return new Promise((resolve, reject) => {
      const x = new XMLHttpRequest();
      x.open("POST", API + "/api/upload");
      x.setRequestHeader("X-Alex-Token", store.get("token", ""));
      x.setRequestHeader("Content-Type", "application/octet-stream");
      if (withCode) x.setRequestHeader("X-Alex-Key", keyB64);
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
    if (!store.get("token", "")) { toast("Nhập mã truy cập trong Cài đặt trước"); show("set"); return; }
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
      const r = await upload(blob, key, withCode, p => fill(0.3 + p * 0.7));
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
  $("tokenInput").value = store.get("token", "");
  $("showToken").onclick = () => { const i = $("tokenInput"); i.type = i.type === "password" ? "text" : "password"; $("showToken").textContent = i.type === "password" ? "Hiện" : "Ẩn"; };
  $("saveToken").onclick = async () => {
    store.set("token", $("tokenInput").value.trim());
    msg("tokenMsg", "Đang kiểm tra…");
    const j = await ping();
    if (!j) msg("tokenMsg", "Không kết nối được máy chủ " + API, "bad");
    else if (j.auth) msg("tokenMsg", `Đúng mã. Tối đa ${human(j.max_bytes)} mỗi lần, lưu ${Math.round(j.ttl / 3600)} giờ.`, "ok");
    else msg("tokenMsg", "Máy chủ chạy, nhưng mã truy cập không đúng", "bad");
  };
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
  ping();
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
