// The page's client: wires the buttons to the wasm Device API.
// Nothing is stored: the passphrase lives in the form field until it is sent.

import init, { Device } from "./pkg/impromptu_wifi_web.js";

const $ = (id) => document.getElementById(id);
const hasSerial = "serial" in navigator;
const hasBluetooth = "bluetooth" in navigator;

let device = null;

function show(text, kind = "") {
  const out = $("result");
  out.textContent = text;
  out.className = `result ${kind}`;
}

// Disable every control while a call runs, so one command runs at a time.
async function busy(fn) {
  const controls = document.querySelectorAll("#try button, #try input");
  controls.forEach((c) => (c.disabled = true));
  try {
    return await fn();
  } finally {
    controls.forEach((c) => (c.disabled = false));
    syncOpen();
  }
}

function message(e) {
  return e instanceof Error ? e.message : String(e);
}

function cancelled(e) {
  return e && e.name === "NotFoundError";
}

function syncOpen() {
  $("passphrase").disabled = $("open").checked;
}

function showDevice(d) {
  device = d;
  $("connect").hidden = true;
  $("device").hidden = false;
  const kind = d.transport === "bluetooth" ? "Bluetooth" : "USB serial";
  $("device-label").textContent = d.name ? `${d.name} (${kind})` : `Connected over ${kind}`;
  document.querySelectorAll("[data-only]").forEach((el) => {
    el.hidden = el.dataset.only !== d.transport;
  });
  $("networks").hidden = true;
  show("");
}

function showConnect(note) {
  device = null;
  $("device").hidden = true;
  $("connect").hidden = false;
  $("passphrase").value = "";
  if (note) $("support").textContent = note;
  else describeSupport();
}

function describeSupport() {
  const missing = [];
  if (!hasSerial) missing.push("Web Serial");
  if (!hasBluetooth) missing.push("Web Bluetooth");
  const text =
    missing.length === 0
      ? "Web Serial and Web Bluetooth need a desktop Chromium-based browser or Chrome on Android; Firefox provides Web Serial only."
      : missing.length === 2
        ? "This browser provides neither Web Serial nor Web Bluetooth. Chrome and Edge provide both; Firefox provides Web Serial."
        : `This browser does not provide ${missing[0]}, so that option is not shown.`;
  $("support").textContent = text;
}

async function connect(open) {
  try {
    await busy(async () => showDevice(await open()));
  } catch (e) {
    if (!cancelled(e)) $("support").textContent = message(e);
  }
}

// Run a device call; a disconnect returns to the connect buttons.
async function call(fn) {
  if (!device) return;
  try {
    await busy(fn);
  } catch (e) {
    const m = message(e);
    if (m === "the device disconnected") {
      showConnect("The device disconnected. Connect again to continue.");
    } else {
      show(m, "error");
    }
  }
}

function renderNetworks(networks) {
  const list = $("networks");
  list.replaceChildren();
  for (const n of networks) {
    const li = document.createElement("li");
    const b = document.createElement("button");
    b.type = "button";
    const name = document.createElement("span");
    name.textContent = n.ssid;
    const sec = document.createElement("span");
    sec.className = "meta";
    sec.textContent = n.security;
    const rssi = document.createElement("span");
    rssi.className = "meta";
    rssi.textContent = `${n.rssi} dBm`;
    b.append(name, sec, rssi);
    b.addEventListener("click", () => {
      $("ssid").value = n.ssid;
      $("open").checked = !n.secured;
      syncOpen();
      if (n.secured) $("passphrase").focus();
    });
    li.append(b);
    list.append(li);
  }
  list.hidden = networks.length === 0;
}

const actions = {
  async state() {
    const s = await device.state();
    const caps = s.capabilities ? `\nSupports: ${s.capabilities.join(", ") || "-"}` : "";
    show(`${s.label}${caps}`);
  },
  async info() {
    const i = await device.info();
    show(
      `Name:     ${i.name}\nFirmware: ${i.firmware} ${i.version}\nHardware: ${i.hardware}` +
        (i.os ? `\nOS:       ${i.os}` : ""),
    );
  },
  async scan() {
    show("Scanning…");
    const networks = await device.scan();
    renderNetworks(networks);
    show(networks.length ? `${networks.length} networks. Choose one to fill in its name.` : "No networks found.");
  },
  async identify() {
    await device.identify();
    show("The device was asked to identify itself.", "ok");
  },
  async network() {
    const n = await device.network();
    show(
      `Online:   ${n.online ? "yes" : "no"}\nSupports: ${n.interfaces.join(", ") || "-"}` +
        n.urls.map((u) => `\nURL:      ${u}`).join(""),
    );
  },
  async "hostname-read"() {
    $("hostname").value = await device.hostname(undefined);
    show("Hostname read.", "ok");
  },
  async "hostname-set"() {
    $("hostname").value = await device.hostname($("hostname").value);
    show("Hostname set.", "ok");
  },
  async "name-read"() {
    $("device-name").value = await device.deviceName(undefined);
    show("Device name read.", "ok");
  },
  async "name-set"() {
    $("device-name").value = await device.deviceName($("device-name").value);
    show("Device name set.", "ok");
  },
};

function renderUrl(url) {
  const out = $("result");
  out.className = "result ok";
  out.textContent = "Connected. The device is at ";
  const a = document.createElement("a");
  // Only link http(s) URLs; show anything else as text.
  if (/^https?:\/\//i.test(url)) {
    a.href = url;
    a.textContent = url;
    a.rel = "noopener";
    out.append(a);
  } else {
    out.append(url);
  }
}

async function provision(event) {
  event.preventDefault();
  const ssid = $("ssid").value;
  const passphrase = $("open").checked ? "" : $("passphrase").value;
  if (!$("open").checked && passphrase === "") {
    show("Enter the passphrase, or tick “Open network”.", "error");
    return;
  }
  await call(async () => {
    try {
      const url = await device.provision(ssid, passphrase, (line) => show(line));
      if (url) renderUrl(url);
      else show("Connected.", "ok");
    } finally {
      $("passphrase").value = "";
    }
  });
}

async function main() {
  $("connect-serial").hidden = !hasSerial;
  $("connect-ble").hidden = !hasBluetooth;
  describeSupport();
  try {
    await init();
  } catch (e) {
    $("support").textContent = `The client could not load: ${message(e)}`;
    return;
  }
  $("connect-serial").addEventListener("click", () => connect(() => Device.connectSerial()));
  $("connect-ble").addEventListener("click", () => connect(() => Device.connectBluetooth()));
  $("disconnect").addEventListener("click", async () => {
    const d = device;
    showConnect();
    if (d) await d.disconnect();
  });
  $("open").addEventListener("change", syncOpen);
  $("provision").addEventListener("submit", provision);
  document.querySelectorAll("[data-action]").forEach((b) => {
    b.addEventListener("click", () => call(actions[b.dataset.action]));
  });
}

main();
