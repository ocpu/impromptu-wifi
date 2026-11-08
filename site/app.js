// The page's client: the connect → choose a network → open the device flow
// around the wasm Device API. Nothing is stored; a passphrase lives in its
// form field, and in memory only while a "try again" action can use it.

import init, { Device } from "./pkg/impromptu_wifi_web.js";

const $ = (id) => document.getElementById(id);
const SVG = "http://www.w3.org/2000/svg";
const hasSerial = "serial" in navigator;
const hasBluetooth = "bluetooth" in navigator;

let device = null; // the connected Device
let transport = null; // "serial" | "bluetooth"
let caps = null; // Bluetooth capability names, or null on serial
let networks = []; // last scan
let selected = null; // selected SSID, or "" for "Other network"

// ---------- small DOM helpers ----------

function el(tag, attrs = {}, ...children) {
  const e = document.createElement(tag);
  for (const [k, v] of Object.entries(attrs)) {
    if (k === "class") e.className = v;
    else if (k === "text") e.textContent = v;
    else if (k.startsWith("on")) e.addEventListener(k.slice(2), v);
    else if (v !== false && v != null) e.setAttribute(k, v === true ? "" : v);
  }
  for (const c of children) if (c != null) e.append(c);
  return e;
}

function icon(name, cls = "icon") {
  const s = document.createElementNS(SVG, "svg");
  s.setAttribute("class", cls);
  s.setAttribute("aria-hidden", "true");
  const u = document.createElementNS(SVG, "use");
  u.setAttribute("href", `#i-${name}`);
  s.append(u);
  return s;
}

function button(label, onclick, cls = "button") {
  return el("button", { type: "button", class: cls, onclick }, label);
}

function quoted(s) {
  return `“${s}”`;
}

// ---------- page state ----------

function setStep(n) {
  document.querySelectorAll(".steps li").forEach((li) => {
    const s = Number(li.dataset.step);
    li.className = s < n ? "done" : s === n ? "current" : "";
    if (s === n) li.setAttribute("aria-current", "step");
    else li.removeAttribute("aria-current");
  });
}

function showView(name) {
  for (const v of ["connect", "device", "working", "done"]) $(`view-${v}`).hidden = v !== name;
  setStep({ connect: 1, device: 2, working: 2, done: 3 }[name]);
}

// One notice at a time, above the current view. `tone`: info | warn | bad.
function notice(tone, title, text, actions = []) {
  const n = $("notice");
  const iconName = { info: "info", warn: "warn", bad: "x-circle" }[tone];
  n.className = `notice ${tone}`;
  n.setAttribute("role", tone === "info" ? "status" : "alert");
  n.replaceChildren(
    icon(iconName),
    el(
      "div",
      { class: "notice-body" },
      el("p", { class: "notice-title", text: title }),
      text ? el("p", { class: "notice-text", text }) : null,
      actions.length ? el("div", { class: "row" }, ...actions) : null,
    ),
  );
  n.hidden = false;
}

function clearNotice() {
  $("notice").hidden = true;
  $("notice").replaceChildren();
}

function message(e) {
  return e instanceof Error ? e.message : String(e);
}

// Disable the client's controls while a command runs: one at a time.
async function busy(fn) {
  const controls = document.querySelectorAll("#try button, #try input");
  controls.forEach((c) => (c.disabled = true));
  try {
    return await fn();
  } finally {
    controls.forEach((c) => (c.disabled = false));
  }
}

// ---------- step 1: connect ----------

async function describeSupport() {
  const text = $("support").querySelector("span");
  if (!hasSerial && !hasBluetooth) {
    $("support").hidden = true;
    notice(
      "warn",
      "This browser can't talk to devices",
      "It has neither Web Serial nor Web Bluetooth. Open this page in Chrome or Edge on a computer, Chrome on Android, or Firefox for USB. Browsers on iPhone and iPad have neither.",
      [button("Copy page link", copyLink)],
    );
    return;
  }
  let line = "";
  if (hasSerial && !hasBluetooth) line = "Bluetooth isn't available in this browser. For Bluetooth, use Chrome or Edge, or Chrome on Android.";
  else if (!hasSerial) line = "USB isn't available in this browser. On a computer, Chrome, Edge or Firefox can connect over USB.";
  else line = "A device offers Bluetooth only until it is on a network.";
  if (hasBluetooth && navigator.bluetooth.getAvailability) {
    try {
      if (!(await navigator.bluetooth.getAvailability())) line += " Bluetooth looks off or unavailable on this device.";
    } catch {
      // Availability is a hint only.
    }
  }
  text.textContent = line;
}

async function copyLink() {
  try {
    await navigator.clipboard.writeText(location.href);
    notice("info", "Link copied", "Open it in a supported browser.");
  } catch {
    notice("info", "Copy this address", location.href);
  }
}

async function connect(kind) {
  clearNotice();
  try {
    const d = await busy(() => (kind === "serial" ? Device.connectSerial() : Device.connectBluetooth()));
    await onConnected(d, kind);
  } catch (e) {
    connectFailed(e, kind);
  }
}

function connectFailed(e, kind) {
  const m = message(e);
  if (e && e.name === "NotFoundError") {
    if (kind === "bluetooth" && /adapter|not available|disabled|globally/i.test(m)) {
      notice("warn", "Bluetooth is off or unavailable", "The browser found no Bluetooth adapter it can use. Turn Bluetooth on, or allow the browser to use it, and try again.", [
        button("Try again", () => connect("bluetooth"), "button primary"),
      ]);
    } else if (kind === "bluetooth") {
      notice("info", "Don't see your device in the list?", "A device stops offering Bluetooth once it is on a network. Reset it to set it up again, or connect over USB.");
    }
    return;
  }
  if (e && e.name === "InvalidStateError") {
    notice("warn", "The port is already open", "Another tab or program is using it. Close it there, then connect again.", [
      button("Connect again", () => connect(kind), "button primary"),
    ]);
    return;
  }
  if (e && e.name === "SecurityError") {
    notice("warn", "The browser blocked access", "Allow this page to use USB or Bluetooth in the browser's site settings, then try again.");
    return;
  }
  notice("bad", "Couldn't connect", m, [button("Try again", () => connect(kind), "button primary")]);
}

// ---------- step 2: connected ----------

async function onConnected(d, kind) {
  device = d;
  transport = kind;
  caps = null;
  networks = [];
  selected = null;
  $("device-icon").setAttribute("href", kind === "serial" ? "#i-usb" : "#i-bt");
  $("device-name").textContent = d.name || (kind === "serial" ? "USB device" : "Bluetooth device");
  $("device-sub").textContent = kind === "serial" ? "USB" : "Bluetooth";
  $("details").replaceChildren();
  $("networks").replaceChildren();
  $("names").open = false;
  $("identify").hidden = true;
  showView("device");

  try {
    await call(refreshState);
  } catch (e) {
    if (e && e.kind === "disconnected") return;
    if (e && e.kind !== "timeout") {
      notice("bad", "The device didn't answer as expected", message(e), [button("Try again", () => onConnected(device, transport), "button primary")]);
    } else {
      notice("warn", "No answer from the device within 10 s", "This port may belong to another device, or the firmware doesn't run Improv on it.", [
        button("Choose another port", () => disconnect().then(() => connect("serial")), "button primary"),
        button("Try again", () => onConnected(device, transport)),
      ]);
    }
    return;
  }
  await call(loadInfo).catch(() => {});
  if (transport === "serial") await call(loadNetworkState).catch(() => {});
  await scan();
}

// Run a device call; a disconnect returns to step 1 with a notice.
async function call(fn) {
  if (!device) throw new Error("not connected");
  try {
    return await busy(fn);
  } catch (e) {
    if (e && e.kind === "disconnected") lost();
    throw e;
  }
}

function lost() {
  const name = $("device-name").textContent;
  const kind = transport;
  device = null;
  showView("connect");
  notice("bad", `${name} disconnected`, "The cable was unplugged, the device restarted, or it went out of range.", [
    button("Connect again", () => connect(kind), "button primary"),
  ]);
}

const STATE_BADGE = {
  stopped: ["", "Stopped"],
  "authorization-required": ["warn", "Waiting for authorization"],
  authorized: ["ok", "Ready for Wi-Fi"],
  provisioning: ["warn", "Connecting to a network"],
  provisioned: ["ok", "Provisioned"],
};

async function refreshState() {
  const s = await device.state();
  const [tone, label] = STATE_BADGE[s.code] || ["", s.label];
  const badge = $("device-state");
  badge.className = `badge ${tone}`;
  badge.textContent = label;
  badge.title = s.label;
  if (s.capabilities) caps = s.capabilities;
  $("identify").hidden = !(transport === "bluetooth" && (!caps || caps.includes("identify")));
  $("hostname-field").hidden = transport === "bluetooth" && caps && !caps.includes("hostname");
  return s;
}

function detail(term, value) {
  $("details").append(el("dt", { text: term }), el("dd", { text: value }));
}

async function loadInfo() {
  const i = await device.info();
  detail("Firmware", `${i.firmware} ${i.version}`);
  detail("Hardware", i.hardware);
  if (i.os) detail("System", i.os);
  if (!device.name && i.name) $("device-name").textContent = i.name;
  $("device-sub").textContent = `${transport === "serial" ? "USB" : "Bluetooth"} · ${i.firmware} ${i.version} · ${i.hardware}`;
}

async function loadNetworkState() {
  const n = await device.network();
  detail("Online", n.online ? "yes" : "no");
  if (n.interfaces.length) detail("Interfaces", n.interfaces.join(", "));
  for (const u of n.urls) detail("Address", u);
}

async function scan() {
  const status = $("scan-status");
  if (transport === "bluetooth" && caps && !caps.includes("scan")) {
    networks = [];
    status.textContent = "This device can't list networks. Choose “Other network” and type the name.";
    status.hidden = false;
    renderNetworks();
    return;
  }
  status.textContent = "Scanning…";
  status.hidden = false;
  try {
    networks = await call(() => device.scan());
    status.hidden = networks.length > 0;
    status.textContent = networks.length ? "" : "The device found no networks. Scan again, or choose “Other network”.";
  } catch (e) {
    if (e && e.kind === "disconnected") return;
    networks = [];
    status.textContent = e && e.kind === "unsupported" ? "This device can't list networks. Choose “Other network” and type the name." : message(e);
  }
  renderNetworks();
}

function signal(rssi) {
  const bars = rssi >= -60 ? 3 : rssi >= -75 ? 2 : 1;
  const s = document.createElementNS(SVG, "svg");
  s.setAttribute("class", "icon signal");
  s.setAttribute("viewBox", "0 0 24 24");
  s.setAttribute("role", "img");
  s.setAttribute("aria-label", `${["Weak", "Medium", "Strong"][bars - 1]} signal, ${rssi} dBm`);
  const parts = ["M2 9a15 15 0 0 1 20 0", "M5.5 12.5a10 10 0 0 1 13 0", "M9 16a5 5 0 0 1 6 0"];
  parts.forEach((d, i) => {
    const p = document.createElementNS(SVG, "path");
    p.setAttribute("d", d);
    if (i >= 3 - bars) p.setAttribute("class", "on");
    s.append(p);
  });
  const c = document.createElementNS(SVG, "circle");
  c.setAttribute("cx", "12");
  c.setAttribute("cy", "19.5");
  c.setAttribute("r", "1");
  c.setAttribute("class", "on");
  s.append(c);
  return s;
}

function renderNetworks() {
  const list = $("networks");
  list.replaceChildren();
  for (const n of networks) {
    const isSel = selected === n.ssid;
    const lock = n.secured ? icon("lock", "icon small") : null;
    if (lock) {
      lock.setAttribute("role", "img");
      lock.setAttribute("aria-label", "Needs a passphrase");
      lock.removeAttribute("aria-hidden");
    }
    const row = el(
      "li",
      { class: isSel ? "selected" : "" },
      el(
        "button",
        { type: "button", class: "net", "aria-expanded": String(isSel), onclick: () => select(n.ssid) },
        signal(n.rssi),
        el("span", { class: "net-name", text: n.ssid }),
        el("span", { class: "net-meta", text: n.security === "secured" ? "" : n.security === "open" ? "Open" : n.security }),
        lock,
      ),
    );
    if (isSel) row.append(joinForm(n.ssid, n.secured));
    list.append(row);
  }
  const other = el(
    "li",
    { class: selected === "" ? "selected" : "" },
    el(
      "button",
      { type: "button", class: "net other", "aria-expanded": String(selected === ""), onclick: () => select("") },
      icon("plus"),
      el("span", { class: "net-name", text: "Other network (hidden, or not listed)" }),
    ),
  );
  if (selected === "") other.append(joinForm("", true));
  list.append(other);
}

function select(ssid) {
  selected = selected === ssid ? null : ssid;
  renderNetworks();
  const field = $("networks").querySelector(".selected input");
  if (field) field.focus();
}

let formId = 0;

function passphraseField(id, label) {
  const input = el("input", { id, type: "password", autocomplete: "off", spellcheck: "false" });
  const reveal = el(
    "button",
    {
      type: "button",
      class: "icon-button",
      "aria-label": "Show passphrase",
      "aria-pressed": "false",
      onclick: () => {
        const show = input.type === "password";
        input.type = show ? "text" : "password";
        reveal.setAttribute("aria-pressed", String(show));
        reveal.setAttribute("aria-label", show ? "Hide passphrase" : "Show passphrase");
      },
    },
    icon("eye"),
  );
  return el("label", { class: "field", for: id }, label, el("span", { class: "reveal" }, input, reveal));
}

function joinForm(ssid, secured) {
  const n = ++formId;
  const form = el("form", { class: "join-form" });
  let ssidInput = null;
  let open = null;
  if (ssid === "") {
    ssidInput = el("input", { id: `ssid-${n}`, autocomplete: "off", spellcheck: "false", required: true });
    form.append(el("label", { class: "field", for: `ssid-${n}` }, "Network name", ssidInput));
  }
  const pass = secured ? passphraseField(`pass-${n}`, ssid ? `Passphrase for ${ssid}` : "Passphrase") : null;
  if (pass) form.append(pass);
  if (ssid === "") {
    open = el("input", { type: "checkbox", onchange: () => (pass.hidden = open.checked) });
    form.append(el("label", { class: "check" }, open, "Open network (no passphrase)"));
  }
  const msg = el("span", { class: "field-msg bad", hidden: true });
  form.append(el("button", { type: "submit", class: "button primary" }, "Connect device"), msg);
  form.addEventListener("submit", (ev) => {
    ev.preventDefault();
    const name = ssidInput ? ssidInput.value : ssid;
    const input = pass && pass.querySelector("input");
    const needs = input && !(open && open.checked);
    const passphrase = needs ? input.value : "";
    if (needs && passphrase === "") {
      input.setAttribute("aria-invalid", "true");
      msg.textContent = "This network needs a passphrase.";
      msg.hidden = false;
      input.focus();
      return;
    }
    if (input) input.value = "";
    provision(name, passphrase);
  });
  return form;
}

// ---------- provisioning ----------

async function provision(ssid, passphrase) {
  clearNotice();
  $("working-title").textContent = `Connecting to ${ssid}`;
  const stages = [...document.querySelectorAll("#view-working [data-stage]")];
  stages.forEach((li) => {
    li.className = li.dataset.stage === "connected" ? "done" : "";
    if (li.dataset.stage === "authorize" || li.dataset.stage === "diagnose") li.hidden = true;
  });
  showView("working");
  let last = null;
  const countdown = $("countdown");
  let timer = null;
  const stage = (name) => {
    for (const li of stages) {
      if (li.dataset.stage === last || (name === "diagnose" && li.dataset.stage === "join")) li.className = "done";
    }
    const li = stages.find((s) => s.dataset.stage === name);
    li.hidden = false;
    li.className = name === "authorize" ? "current waiting" : "current";
    if (name === "send") {
      // The credentials go out at once; the wait is for the join.
      li.className = "done";
      stages.find((s) => s.dataset.stage === "join").className = "current";
      name = "join";
    }
    last = name;
    clearInterval(timer);
    countdown.hidden = name !== "authorize";
    if (name === "authorize") {
      const start = Date.now();
      const bar = countdown.firstElementChild;
      timer = setInterval(() => {
        const left = Math.max(0, 60 - (Date.now() - start) / 1000);
        bar.style.transform = `scaleX(${left / 60})`;
        countdown.setAttribute("aria-valuenow", String(Math.round(left)));
      }, 250);
    }
  };
  try {
    const url = await call(() => device.provision(ssid, passphrase, stage));
    clearInterval(timer);
    done(ssid, url);
  } catch (e) {
    clearInterval(timer);
    countdown.hidden = true;
    if (e && e.kind === "disconnected") return;
    showView("device");
    await call(refreshState).catch(() => {});
    joinFailed(e, ssid, passphrase, last);
  }
}

function joinFailed(e, ssid, passphrase, last) {
  const again = () => provision(ssid, passphrase);
  const reenter = button("Re-enter passphrase", () => {
    clearNotice();
    selected = networks.some((n) => n.ssid === ssid) ? ssid : "";
    renderNetworks();
    const inputs = $("networks").querySelectorAll(".selected input:not([type=checkbox])");
    if (selected === "" && inputs.length > 1) inputs[0].value = ssid;
    inputs[inputs.length - 1]?.focus();
  }, "button primary");
  const other = button("Choose another network", () => {
    clearNotice();
    selected = null;
    renderNetworks();
    $("networks").querySelector("button")?.focus();
  });
  const seen = e && typeof e.rssi === "number" ? ` The device sees it at ${e.rssi} dBm, so it is in range.` : "";
  switch (e && e.kind) {
    case "not-seen": {
      const acts = [];
      if (e.similar) {
        acts.push(button(`Use ${quoted(e.similar)}`, () => {
          clearNotice();
          selected = e.similar;
          renderNetworks();
          $("networks").querySelector(".selected input")?.focus();
        }, "button primary"));
      }
      acts.push(other);
      const hint = e.similar ? ` It does see ${quoted(e.similar)}: is that the one?` : "";
      notice("bad", e.title, e.advice + hint, acts);
      break;
    }
    case "open-network":
      notice("warn", e.title, e.advice, [button("Connect without a passphrase", () => provision(ssid, ""), "button primary"), other]);
      break;
    case "passphrase-missing":
    case "wpa3-only":
    case "rejected":
    case "unknown":
      notice("bad", e.title, e.advice + seen, [reenter, other]);
      break;
    case "timeout":
      if (last === "authorize") {
        notice("warn", "The button wasn't pressed in time", "The device waits 60 s for a press before it accepts Wi-Fi settings. Press its button, then send again.", [
          button("Send again", again, "button primary"),
        ]);
      } else {
        notice("warn", "No answer while joining", "The device didn't report a result within 60 s. It may still be joining; check its state before sending again.", [
          button("Check state", () => call(refreshState).catch(() => {}), "button primary"),
        ]);
      }
      break;
    case "not-authorized":
      notice("warn", "The device needs authorization", "Press the button on the device, then send again.", [button("Send again", again, "button primary")]);
      break;
    default:
      notice("bad", "Couldn't send the settings", message(e), [button("Try again", again, "button primary")]);
  }
}

function done(ssid, url) {
  const name = $("device-name").textContent;
  $("done-title").textContent = `${name} is on ${ssid}`;
  const link = $("done-open");
  const safe = url && /^https?:\/\//i.test(url);
  $("done-text").textContent = url
    ? "The device reports this address. Open it to finish setting the device up."
    : "The device joined the network. It didn't report an address.";
  $("done-url").textContent = url || "";
  $("done-url").hidden = !url;
  link.hidden = !safe;
  if (safe) link.href = url;
  $("device-state").className = "badge ok";
  $("device-state").textContent = "Provisioned";
  showView("done");
}

// ---------- names, identify, disconnect ----------

async function readNames() {
  try {
    $("name-input").value = await call(() => device.deviceName(undefined));
  } catch (e) {
    unsupportedField(e, "name-field", "This device doesn't support changing its name.");
  }
  if ($("hostname-field").hidden) return;
  try {
    $("hostname-input").value = await call(() => device.hostname(undefined));
  } catch (e) {
    unsupportedField(e, "hostname-field", "This device doesn't support changing its hostname.");
  }
}

function unsupportedField(e, id, text) {
  if (!e || e.kind !== "unsupported") return;
  $(id).replaceChildren(el("span", { text: id === "name-field" ? "Device name" : "Hostname" }), el("span", { class: "field-msg", text }));
}

async function saveName(kind) {
  const input = $(kind === "name" ? "name-input" : "hostname-input");
  const msg = input.closest(".field").querySelector(".field-msg");
  input.removeAttribute("aria-invalid");
  try {
    input.value = await call(() => (kind === "name" ? device.deviceName(input.value) : device.hostname(input.value)));
    msg.className = "field-msg ok";
    msg.textContent = "Saved.";
    msg.hidden = false;
  } catch (e) {
    if (e && e.kind === "disconnected") return;
    input.setAttribute("aria-invalid", "true");
    msg.className = "field-msg bad";
    msg.textContent = e && e.kind === "bad-hostname" ? "The device rejected it: letters, digits and hyphens only." : message(e);
    msg.hidden = false;
  }
}

async function disconnect() {
  const d = device;
  device = null;
  clearNotice();
  if (d) await busy(() => d.disconnect());
  showView("connect");
}

// ---------- navigation ----------

function setupNav() {
  const nav = document.querySelector(".nav");
  const toggle = document.querySelector(".nav-toggle");
  const scrim = document.querySelector(".nav-scrim");
  const set = (open) => {
    nav.classList.toggle("open", open);
    toggle.setAttribute("aria-expanded", String(open));
    toggle.setAttribute("aria-label", open ? "Close menu" : "Menu");
    toggle.querySelector("use").setAttribute("href", open ? "#i-close" : "#i-menu");
    scrim.hidden = !open;
  };
  toggle.addEventListener("click", () => set(!nav.classList.contains("open")));
  scrim.addEventListener("click", () => set(false));
  nav.querySelectorAll(".nav-links a").forEach((a) => a.addEventListener("click", () => set(false)));
  document.addEventListener("keydown", (e) => {
    if (e.key === "Escape" && nav.classList.contains("open")) {
      set(false);
      toggle.focus();
    }
  });
}

// ---------- start ----------

async function main() {
  setupNav();
  showView("connect");
  $("connect-serial").hidden = !hasSerial;
  $("connect-ble").hidden = !hasBluetooth;
  await describeSupport();
  if (!hasSerial && !hasBluetooth) return;
  try {
    await init();
  } catch (e) {
    notice("bad", "The client didn't load", "The WebAssembly module could not start. Reload the page; if it keeps failing, the browser may block WebAssembly.", [
      button("Reload", () => location.reload(), "button primary"),
    ]);
    $("connect-serial").disabled = true;
    $("connect-ble").disabled = true;
    return;
  }
  $("connect-serial").addEventListener("click", () => connect("serial"));
  $("connect-ble").addEventListener("click", () => connect("bluetooth"));
  $("rescan").addEventListener("click", () => scan());
  $("identify").addEventListener("click", async () => {
    try {
      await call(() => device.identify());
      notice("info", "The device was asked to identify itself", "Look for a blinking light or a sound.");
    } catch (e) {
      if (e && e.kind !== "disconnected") notice("bad", "Identify failed", message(e));
    }
  });
  $("disconnect").addEventListener("click", disconnect);
  $("names").addEventListener("toggle", () => {
    if ($("names").open && device) readNames();
  });
  $("name-save").addEventListener("click", () => saveName("name"));
  $("hostname-save").addEventListener("click", () => saveName("hostname"));
  $("done-names").addEventListener("click", () => {
    showView("device");
    $("names").open = true;
  });
  $("done-another").addEventListener("click", disconnect);
}

main();
