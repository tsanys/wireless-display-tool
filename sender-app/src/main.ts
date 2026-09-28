import { invoke } from "@tauri-apps/api/core";
import { initLocale, locale, setLocale, t, tf, type Locale } from "./i18n";
import { listen } from "@tauri-apps/api/event";
import QRCode from "qrcode";

interface PairingInfo {
  ip: string;
  port: number;
  token: string;
  pairingString: string;
  mdnsInstance: string;
  serverRunning: boolean;
  serverError?: string;
  deviceId: string;
  displayName: string;
}

interface ReceiverView { deviceId?: string; deviceName?: string }
interface MirrorStatus {
  state: "idle" | "offering" | "connecting" | "connected" | "error";
  message?: string;
}
interface CapabilityState { available: boolean; reason?: string }
interface DisplayInfo {
  id: string;
  name: string;
  isPrimary: boolean;
  width: number;
  height: number;
}
interface CapabilityReport {
  displays: DisplayInfo[];
  virtualDisplay: CapabilityState;
  systemAudioCapture: CapabilityState;
  receiverAudio: boolean;
}
interface PipelineInfo {
  width: number;
  height: number;
  fps: number;
  ssrc: number;
  payloadType: number;
}
interface PipelineStats { frames: number; skipped: number; errors: number; fps: number }

type AudioRoute = "laptop" | "tv" | "both" | "muted";
type DisplayMode = "mirror" | "extended";
type VideoResolution = "p1080" | "p720";
type VideoFps = "fps30" | "fps60";
type VideoQuality = "balanced" | "sharp";
interface VideoSettingsUi {
  resolution: VideoResolution;
  frameRate: VideoFps;
  quality: VideoQuality;
}

function isVideoResolution(v: unknown): v is VideoResolution { return v === "p1080" || v === "p720"; }
function isVideoFps(v: unknown): v is VideoFps { return v === "fps30" || v === "fps60"; }
function isVideoQuality(v: unknown): v is VideoQuality { return v === "balanced" || v === "sharp"; }

const VIDEO_RESOLUTION_LABELS: Record<VideoResolution, string> = { p1080: "1080p", p720: "720p" };
const VIDEO_FPS_LABELS: Record<VideoFps, string> = { fps30: "30 fps", fps60: "60 fps" };
const VIDEO_QUALITY_ID: Record<VideoQuality, string> = { balanced: "Seimbang", sharp: "Tajam (teks)" };

interface AudioStats {
  packetsSent: number;
  sentMs: number;
  dropped: number;
  errors: number;
  reconnects: number;
  level: number;
}

const AUDIO_ROUTE_ID: Record<AudioRoute, string> = {
  laptop: "Laptop",
  tv: "TV",
  both: "Keduanya",
  muted: "Tanpa suara",
};

function isAudioRoute(value: unknown): value is AudioRoute {
  return value === "laptop" || value === "tv" || value === "both" || value === "muted";
}

const $ = <T extends HTMLElement = HTMLElement>(id: string) => document.getElementById(id) as T;
const busyStates = new Set<MirrorStatus["state"]>(["offering", "connecting"]);
const isTauri = "__TAURI_INTERNALS__" in window;

let pairingInfo: PairingInfo | null = null;
let capabilities: CapabilityReport | null = null;
let receivers: ReceiverView[] = [];
let selectedReceiver = "";
let displayMode: DisplayMode = "mirror";
let selectedDisplayId = "";
let displayNotice = "";
let audioRoute: AudioRoute = "laptop";
let videoSettings: VideoSettingsUi = { resolution: "p1080", frameRate: "fps30", quality: "balanced" };
let audioStats: AudioStats | null = null;
let audioRouteChanging = false;
let audioNotice: string | null = null;
let mirrorStatus: MirrorStatus = { state: "idle" };
let pipelineInfo: PipelineInfo | null = null;
let pipelineStats: PipelineStats | null = null;

function humanizeDeviceId(deviceId?: string): string {
  const raw = deviceId?.trim();
  if (!raw) return "Android TV";
  return raw
    .replace(/^wdt[-_ ]?/i, "")
    .replace(/[-_]+/g, " ")
    .replace(/\b\w/g, (letter) => letter.toUpperCase());
}

function receiverName(deviceId?: string): string {
  const receiver = receivers.find((candidate) => candidate.deviceId === deviceId);
  return receiver?.deviceName?.trim() || humanizeDeviceId(deviceId);
}

function selectedDisplay(): DisplayInfo | undefined {
  return capabilities?.displays.find((display) => display.id === selectedDisplayId);
}

function fallbackDisplay(): DisplayInfo | undefined {
  return capabilities?.displays.find((display) => display.isPrimary) ?? capabilities?.displays[0];
}

function ensureSelectedDisplay(): void {
  if (selectedDisplay()) return;
  const fallback = fallbackDisplay();
  if (selectedDisplayId && fallback) {
    displayNotice = `Layar pilihan sebelumnya tidak terhubung. Diganti ke ${fallback.name}.`;
  }
  selectedDisplayId = fallback?.id ?? "";
}

function sessionCopy(): { kicker: string; title: string; description: string } {
  if (mirrorStatus.state === "connected") {
    return {
      kicker: t("Koneksi aktif"),
      title: t("Layar sedang dibagikan"),
      description: tf("{tv} menerima tampilan dari laptop ini.", { tv: receiverName(selectedReceiver) }),
    };
  }
  if (busyStates.has(mirrorStatus.state)) {
    return {
      kicker: mirrorStatus.state === "offering" ? t("Menyiapkan sesi") : t("Hampir selesai"),
      title: tf("Menghubungkan ke {tv}…", { tv: receiverName(selectedReceiver) }),
      description: t("Biarkan WDT Receiver tetap terbuka di TV."),
    };
  }
  if (mirrorStatus.state === "error") {
    return {
      kicker: t("Koneksi membutuhkan perhatian"),
      title: t("Belum dapat terhubung"),
      description: t("Periksa TV dan jaringan, lalu coba lagi."),
    };
  }
  if (selectedReceiver) {
    return {
      kicker: t("TV siap menerima layar"),
      title: t("Siap berbagi"),
      description: t("Periksa ringkasan pengaturan, lalu mulai berbagi."),
    };
  }
  return {
    kicker: t("Belum ada TV terhubung"),
    title: t("Hubungkan TV untuk memulai"),
    description: t("Buka WDT Receiver di TV pada jaringan yang sama."),
  };
}

function friendlyError(message?: string): string {
  const raw = message?.trim();
  if (!raw) return t("TV tidak merespons. Pastikan kedua perangkat memakai jaringan yang sama.");
  const normalized = raw.toLowerCase();
  if (normalized.includes("permission") || normalized.includes("screen recording")) {
    return t("WDT belum memiliki izin merekam layar. Buka pengaturan privasi sistem, izinkan WDT, lalu coba lagi.");
  }
  if (normalized.includes("badtoken") || normalized.includes("token")) {
    return t("Kode pairing tidak cocok. Pastikan kode 6 digit di TV sama dengan yang tampil di sini.");
  }
  if (normalized.includes("senderbusy") || normalized.includes("busy") || normalized.includes("sedang dipakai")) {
    return t("TV sedang dipakai sesi lain. Putuskan sesi itu di TV, lalu coba lagi.");
  }
  if (
    normalized.includes("failed to fetch") ||
    normalized.includes("network") ||
    normalized.includes("offline") ||
    normalized.includes("refused") ||
    normalized.includes("ws ") ||
    normalized.includes("websocket")
  ) {
    return t("Tidak dapat menjangkau jaringan. Periksa koneksi Wi-Fi/lan, lalu coba lagi.");
  }
  if (normalized.includes("receiver") || normalized.includes("tv") || normalized.includes("peer")) {
    return t("Koneksi dengan TV terputus. Biarkan WDT Receiver terbuka, lalu coba lagi.");
  }
  if (normalized.includes("capture") || normalized.includes("capturer")) {
    return t("Layar belum dapat dibaca. Periksa izin perekaman layar, lalu coba lagi.");
  }
  if (normalized.includes("encoder")) {
    return t("Encoder video perangkat tidak dapat dimulai. Tutup aplikasi lain yang memakai perekaman layar, lalu coba lagi.");
  }
  return raw;
}

function renderServiceStatus(): void {
  const pill = $("service-pill");
  const label = $("service-label");
  pill.classList.remove("pending", "ok", "error");
  if (!pairingInfo) {
    pill.classList.add("pending");
    label.textContent = t("Menyiapkan koneksi…");
  } else if (pairingInfo.serverRunning) {
    pill.classList.add("ok");
    label.textContent = t("Siap digunakan");
  } else {
    pill.classList.add("error");
    label.textContent = t("Koneksi lokal bermasalah");
  }
}

function renderReceivers(): void {
  const container = $("receiver-list");
  container.replaceChildren();
  if (receivers.length === 0) {
    const empty = document.createElement("div");
    empty.className = "receiver-empty";
    empty.innerHTML = `<span class="receiver-radar" aria-hidden="true"><i></i></span><span><strong>${t("Mencari TV…")}</strong><small>${t("TV akan muncul otomatis")}</small></span>`;
    container.appendChild(empty);
    selectedReceiver = "";
    return;
  }

  if (!receivers.some((receiver) => (receiver.deviceId ?? "") === selectedReceiver)) {
    selectedReceiver = receivers[0].deviceId ?? "";
  }
  for (const receiver of receivers) {
    const id = receiver.deviceId ?? "";
    const button = document.createElement("button");
    button.type = "button";
    button.className = `receiver-choice${selectedReceiver === id ? " selected" : ""}`;
    button.setAttribute("aria-pressed", String(selectedReceiver === id));
    button.innerHTML = `
      <span class="receiver-icon" aria-hidden="true"><svg viewBox="0 0 24 24"><rect x="3" y="4" width="18" height="14" rx="2"/><path d="M9 21h6M12 18v3"/></svg></span>
      <span><strong>${receiver.deviceName?.trim() || humanizeDeviceId(id)}</strong><small>${t("Terhubung dan siap")}</small></span>
      <span class="selected-check" aria-hidden="true">✓</span>`;
    button.onclick = () => {
      selectedReceiver = id;
      restoreSettings();
      persistSettings();
      renderAll();
    };
    container.appendChild(button);
  }
}

function renderCapabilities(): void {
  if (!capabilities) return;
  const extendedInput = document.querySelector<HTMLInputElement>('input[name="display-mode"][value="extended"]')!;
  extendedInput.disabled = !capabilities.virtualDisplay.available;
  $("extended-option").classList.toggle("is-disabled", extendedInput.disabled);
  $("extended-badge").hidden = capabilities.virtualDisplay.available;
  $("extended-reason").textContent = capabilities.virtualDisplay.available
    ? "Eksperimental · macOS memakai API tidak resmi; bisa berubah setelah pembaruan sistem"
    : (capabilities.virtualDisplay.reason ?? t("Belum tersedia"));

  ensureSelectedDisplay();
  const displaySelect = $("display-source") as HTMLSelectElement;
  displaySelect.replaceChildren();
  for (const display of capabilities.displays) {
    const option = document.createElement("option");
    option.value = display.id;
    option.textContent = `${display.name} · ${display.width}×${display.height}${display.isPrimary ? " · Utama" : ""}`;
    option.selected = display.id === selectedDisplayId;
    displaySelect.appendChild(option);
  }
  if (capabilities.displays.length === 0) {
    const option = document.createElement("option");
    option.textContent = t("Tidak ada layar tersedia");
    displaySelect.appendChild(option);
  }
  $("display-source-control").hidden = displayMode !== "mirror";
  const displayNote = $("display-source-note");
  displayNote.classList.toggle("notice", Boolean(displayNotice));
  displayNote.textContent = displayNotice || (capabilities.displays.length > 1
    ? `${capabilities.displays.length} layar terdeteksi. Pilih layar yang ingin ditampilkan di TV.`
    : t("Layar ini akan dicerminkan ke TV."));

  const tvAudioAvailable = capabilities.systemAudioCapture.available && capabilities.receiverAudio;
  for (const value of ["tv", "both"] as const) {
    const input = document.querySelector<HTMLInputElement>(`input[name="audio-route"][value="${value}"]`)!;
    input.disabled = !tvAudioAvailable;
    input.closest("label")?.classList.toggle("is-disabled", input.disabled);
  }
  $("audio-reason").textContent = tvAudioAvailable
    ? t("Audio sistem dapat dikirim ke TV.")
    : (capabilities.systemAudioCapture.reason
        ?? (capabilities.receiverAudio ? t("Audio TV belum siap.") : t("TV ini belum mendukung audio.")));

  // Peringatan jujur per route.
  const warning = $("audio-warning");
  if (!tvAudioAvailable) {
    warning.hidden = true;
    warning.textContent = "";
  } else if (audioRoute === "tv") {
    warning.hidden = false;
    warning.textContent = t("Laptop tidak dapat dipastikan senyap secara aman. Matikan volume laptop bila suara masih terdengar.");
  } else if (audioRoute === "both") {
    warning.hidden = false;
    warning.textContent = t("Suara laptop dan TV bisa terasa tidak sinkron karena perbedaan jarak dan latensi jaringan.");
  } else if (audioNotice) {
    warning.hidden = false;
    warning.textContent = audioNotice;
  } else {
    warning.hidden = true;
    warning.textContent = "";
  }
}

function renderSession(): void {
  const copy = sessionCopy();
  $("session-kicker").textContent = copy.kicker;
  $("session-title").textContent = copy.title;
  $("session-description").textContent = copy.description;
  document.body.dataset.sessionState = mirrorStatus.state;
  document.body.dataset.hasReceiver = String(Boolean(selectedReceiver));

  const tvName = selectedReceiver ? receiverName(selectedReceiver) : t("Belum dipilih");
  $("tv-device-label").textContent = selectedReceiver ? tvName : "TV tujuan";
  $("tv-screen-label").textContent = mirrorStatus.state === "connected"
    ? t("Terhubung")
    : selectedReceiver ? tvName : t("Menunggu TV");
  $("summary-tv").textContent = tvName;
  $("summary-display").textContent = displayMode === "mirror"
    ? (selectedDisplay()?.name ?? "Cerminkan layar")
    : t("Layar tambahan");
  $("summary-audio").textContent = t(AUDIO_ROUTE_ID[audioRoute]);
  $("summary-quality").textContent = `${VIDEO_RESOLUTION_LABELS[videoSettings.resolution]} · ${VIDEO_FPS_LABELS[videoSettings.frameRate]} · ${t(VIDEO_QUALITY_ID[videoSettings.quality])}`;

  const callout = $("status-callout");
  if (mirrorStatus.state === "error") {
    callout.hidden = false;
    $("status-callout-text").textContent = friendlyError(mirrorStatus.message);
  } else if (pairingInfo && !pairingInfo.serverRunning) {
    callout.hidden = false;
    $("status-callout-text").textContent = pairingInfo.serverError ?? t("Layanan koneksi lokal tidak dapat dimulai.");
  } else if (mirrorStatus.state === "idle" && mirrorStatus.message && !selectedReceiver) {
    callout.hidden = false;
    $("status-callout-text").textContent = t("Koneksi TV terputus. Buka kembali WDT Receiver; TV akan muncul otomatis.");
  } else {
    callout.hidden = true;
  }

  const isBusy = busyStates.has(mirrorStatus.state);
  const isConnected = mirrorStatus.state === "connected";
  const settingsLocked = isBusy || isConnected;
  // Mode tampilan terkunci saat sesi aktif; route audio BOLEH diubah saat
  // connected (live switching tanpa putus video), hanya terkunci saat
  // transisi (offering/connecting).
  document.querySelectorAll<HTMLInputElement>('input[name="display-mode"]').forEach((input) => {
    const capabilityDisabled = input.value === "extended" && !capabilities?.virtualDisplay.available;
    input.disabled = settingsLocked || capabilityDisabled;
  });
  document.querySelectorAll<HTMLInputElement>('input[name="audio-route"]').forEach((input) => {
    const capabilityDisabled =
      ["tv", "both"].includes(input.value) &&
      !(capabilities?.systemAudioCapture.available && capabilities?.receiverAudio);
    input.disabled = isBusy || capabilityDisabled || audioRouteChanging;
  });
  const displaySelect = $("display-source") as HTMLSelectElement;
  displaySelect.disabled = settingsLocked || displayMode !== "mirror" || !selectedDisplayId;
  ($("video-resolution") as HTMLSelectElement).disabled = settingsLocked;
  ($("video-fps") as HTMLSelectElement).disabled = settingsLocked;
  ($("video-quality") as HTMLSelectElement).disabled = settingsLocked;
  $("video-settings-note").textContent = `${VIDEO_RESOLUTION_LABELS[videoSettings.resolution]} · ${VIDEO_FPS_LABELS[videoSettings.frameRate]} · ${t(VIDEO_QUALITY_ID[videoSettings.quality])}`;

  const start = $("btn-start") as HTMLButtonElement;
  start.disabled = !selectedReceiver || !selectedDisplayId || !pairingInfo?.serverRunning || isBusy || isConnected;
  start.hidden = isConnected;
  $("start-label").textContent = isBusy
    ? t("Menghubungkan…")
    : mirrorStatus.state === "error"
      ? "Coba lagi"
      : t("Mulai berbagi");
  $("btn-stop").hidden = !(isBusy || isConnected);

  const liveStats = $("live-stats");
  liveStats.hidden = !isConnected;
  if (isConnected) {
    $("mirror-stats").textContent = pipelineStats
      ? `${pipelineStats.fps.toFixed(1)} fps · ${pipelineStats.skipped} frame dilewati`
      : pipelineInfo ? `${pipelineInfo.width}×${pipelineInfo.height} · ${pipelineInfo.fps} fps` : t("Menyiapkan statistik…");
  }
}

async function switchAudioRoute(next: AudioRoute, previous: AudioRoute): Promise<void> {
  audioRouteChanging = true;
  audioNotice = "Memindahkan tujuan suara…";
  renderSession();
  renderCapabilities();
  try {
    await invoke("set_audio_route", { route: next });
    // Konfirmasi final datang lewat event audio-route-changed.
    audioNotice = null;
  } catch (error) {
    // Video tetap berjalan; hanya audio yang gagal berpindah.
    audioRoute = previous;
    audioNotice = `Tujuan suara tidak dapat diubah: ${String(error)}`;
    persistSettings();
  } finally {
    audioRouteChanging = false;
    const input = document.querySelector<HTMLInputElement>(`input[name="audio-route"][value="${audioRoute}"]`);
    if (input) input.checked = true;
    renderAll();
  }
}

function renderDiagnostics(): void {
  $("diagnostic-server").textContent = pairingInfo?.serverRunning ? t("Aktif") : t("Tidak aktif");
  $("diagnostic-address").textContent = pairingInfo?.serverRunning ? `${pairingInfo.ip}:${pairingInfo.port}` : "—";
  $("diagnostic-tv").textContent = selectedReceiver ? receiverName(selectedReceiver) : t("Belum terhubung");
  $("diagnostic-session").textContent = mirrorStatus.state;
  $("diagnostic-pipeline").textContent = pipelineInfo
    ? `${pipelineInfo.width}×${pipelineInfo.height}@${pipelineInfo.fps} · PT ${pipelineInfo.payloadType}`
    : t("Belum aktif");
  $("diagnostic-audio").textContent = audioStats
    ? `${t(AUDIO_ROUTE_ID[audioRoute])} · ${audioStats.packetsSent} paket · ${(audioStats.sentMs / 1000).toFixed(1)} dtk`
      + (audioStats.errors > 0 ? ` · ${audioStats.errors} error` : "")
      + (audioStats.reconnects > 0 ? ` · ${audioStats.reconnects} reconnect` : "")
    : t(AUDIO_ROUTE_ID[audioRoute]);
}

function renderAll(): void {
  renderServiceStatus();
  renderReceivers();
  renderCapabilities();
  renderSession();
  renderDiagnostics();
}

function persistSettings(): void {
  if (!selectedReceiver) return;
  localStorage.setItem(
    `wdt:settings:${selectedReceiver}`,
    JSON.stringify({ displayMode, displayId: selectedDisplayId, audioRoute, videoSettings }),
  );
}

function restoreSettings(): void {
  if (!selectedReceiver) return;
  let savedDisplayId = "";
  try {
    const saved = JSON.parse(localStorage.getItem(`wdt:settings:${selectedReceiver}`) ?? "null");
    if (saved?.displayMode === "mirror" || saved?.displayMode === "extended") displayMode = saved.displayMode;
    if (typeof saved?.displayId === "string") savedDisplayId = saved.displayId;
    if (isAudioRoute(saved?.audioRoute)) audioRoute = saved.audioRoute;
    if (isVideoResolution(saved?.videoSettings?.resolution)) videoSettings.resolution = saved.videoSettings.resolution;
    if (isVideoFps(saved?.videoSettings?.frameRate)) videoSettings.frameRate = saved.videoSettings.frameRate;
    if (isVideoQuality(saved?.videoSettings?.quality)) videoSettings.quality = saved.videoSettings.quality;
  } catch {
    // Setting lama yang korup diabaikan; default aman tetap digunakan.
  }
  if (!capabilities?.virtualDisplay.available) displayMode = "mirror";
  // Route TV/Both butuh capture sistem + receiver audio; kalau tidak, turun ke Laptop.
  if (["tv", "both"].includes(audioRoute) && !(capabilities?.systemAudioCapture.available && capabilities?.receiverAudio)) {
    audioRoute = "laptop";
  }
  displayNotice = "";
  if (savedDisplayId && capabilities?.displays.some((display) => display.id === savedDisplayId)) {
    selectedDisplayId = savedDisplayId;
  } else {
    selectedDisplayId = fallbackDisplay()?.id ?? "";
    if (savedDisplayId && selectedDisplayId) {
      displayNotice = `Layar pilihan sebelumnya tidak terhubung. Diganti ke ${selectedDisplay()?.name ?? "layar utama"}.`;
    }
  }
  document.querySelector<HTMLInputElement>(`input[name="display-mode"][value="${displayMode}"]`)!.checked = true;
  document.querySelector<HTMLInputElement>(`input[name="audio-route"][value="${audioRoute}"]`)!.checked = true;
  ($("video-resolution") as HTMLSelectElement).value = videoSettings.resolution;
  ($("video-fps") as HTMLSelectElement).value = videoSettings.frameRate;
  ($("video-quality") as HTMLSelectElement).value = videoSettings.quality;
}

async function renderPairing(): Promise<void> {
  if (!pairingInfo?.serverRunning) return;
  $("pairing-token").textContent = pairingInfo.token;
  $("pairing-text").textContent = pairingInfo.pairingString;
  try {
    await QRCode.toCanvas($("qr-canvas") as HTMLCanvasElement, pairingInfo.pairingString, {
      width: 216,
      margin: 2,
      color: { dark: "#0B1320", light: "#FFFFFF" },
    });
  } catch (error) {
    console.error("QR gagal:", error);
  }
}

async function refreshAll(): Promise<void> {
  if (!isTauri) {
    const mock = new URLSearchParams(window.location.search).get("mock") ?? "empty";
    pairingInfo = {
      ip: "192.168.1.24",
      port: 8420,
      token: "482731",
      pairingString: "192.168.1.24:8420:482731",
      mdnsInstance: "WDT Laptop Pandu",
      serverRunning: mock !== "server-error",
      serverError: mock === "server-error" ? "Port koneksi sedang dipakai aplikasi lain" : undefined,
      deviceId: "sender-8472",
      displayName: "Laptop Pandu",
    };
    const mockDisplays: DisplayInfo[] = [
      { id: "mock:built-in", name: "Layar MacBook", isPrimary: true, width: 2560, height: 1600 },
      { id: "mock:external", name: "Monitor Studio", isPrimary: false, width: 1920, height: 1080 },
    ];
    capabilities = {
      displays: mock === "single" ? mockDisplays.slice(0, 1) : mockDisplays,
      virtualDisplay: { available: false, reason: "Komponen layar tambahan masih dalam pengembangan" },
      systemAudioCapture: mock === "no-audio"
        ? { available: false, reason: "Izin Screen Recording belum diberikan untuk WDT." }
        : { available: true, reason: undefined },
      receiverAudio: mock !== "no-audio",
    };
    receivers = mock === "empty" || mock === "server-error" ? [] : [
      { deviceId: "tv-ruang-keluarga", deviceName: "TV Ruang Keluarga" },
      { deviceId: "tv-studio", deviceName: "TV Studio" },
    ];
    mirrorStatus = mock === "connected"
      ? { state: "connected" }
      : mock === "connecting"
        ? { state: "connecting" }
        : mock === "error"
          ? { state: "error", message: "TV berhenti merespons saat negosiasi koneksi." }
          : { state: "idle" };
    if (mock === "connected") {
      pipelineInfo = { width: 1920, height: 1080, fps: 30, ssrc: 1938472, payloadType: 96 };
      pipelineStats = { frames: 1834, skipped: 2, errors: 0, fps: 29.7 };
    }
    if (!selectedReceiver && receivers.length > 0) selectedReceiver = receivers[0].deviceId ?? "";
    restoreSettings();
    ($("device-name-input") as HTMLInputElement).value = pairingInfo.displayName;
    renderAll();
    await renderPairing();
    return;
  }

  const [pairingResult, receiverResult, statusResult, capabilityResult] = await Promise.allSettled([
    invoke<PairingInfo>("get_pairing_info"),
    invoke<ReceiverView[]>("get_receivers"),
    invoke<MirrorStatus>("get_mirror_status"),
    invoke<CapabilityReport>("get_capabilities"),
  ]);
  pairingInfo = pairingResult.status === "fulfilled" ? pairingResult.value : {
    ip: "", port: 0, token: "", pairingString: "", mdnsInstance: "", serverRunning: false,
    serverError: String(pairingResult.reason),
    deviceId: "", displayName: "",
  };
  if (receiverResult.status === "fulfilled") receivers = receiverResult.value;
  if (statusResult.status === "fulfilled") mirrorStatus = statusResult.value;
  if (capabilityResult.status === "fulfilled") capabilities = capabilityResult.value;
  ensureSelectedDisplay();

  if (!selectedReceiver && receivers.length > 0) {
    selectedReceiver = receivers[0].deviceId ?? "";
    restoreSettings();
  }
  if (pairingInfo.displayName) {
    ($("device-name-input") as HTMLInputElement).value = pairingInfo.displayName;
  }
  renderAll();
  await renderPairing();
}

async function openHelp(): Promise<void> {
  ($("help-dialog") as HTMLDialogElement).showModal();
  try {
    $("firewall-text").textContent = await invoke<string>("get_firewall_help");
  } catch (error) {
    $("firewall-text").textContent = String(error);
  }
}

function bindControls(): void {
  document.querySelectorAll<HTMLInputElement>('input[name="display-mode"]').forEach((input) => {
    input.addEventListener("change", () => {
      displayMode = input.value as DisplayMode;
      persistSettings();
      renderCapabilities();
      renderSession();
    });
  });
  for (const id of ["video-resolution", "video-fps", "video-quality"] as const) {
    $(id).addEventListener("change", () => {
      videoSettings = {
        resolution: ($("video-resolution") as HTMLSelectElement).value as VideoResolution,
        frameRate: ($("video-fps") as HTMLSelectElement).value as VideoFps,
        quality: ($("video-quality") as HTMLSelectElement).value as VideoQuality,
      };
      persistSettings();
      renderSession();
    });
  }
  $("display-source").addEventListener("change", (event) => {
    selectedDisplayId = (event.currentTarget as HTMLSelectElement).value;
    displayNotice = "";
    persistSettings();
    renderCapabilities();
    renderSession();
  });
  document.querySelectorAll<HTMLInputElement>('input[name="audio-route"]').forEach((input) => {
    input.addEventListener("change", () => {
      const next = input.value as AudioRoute;
      if (next === audioRoute) return;
      const previous = audioRoute;
      audioRoute = next;
      audioNotice = null;
      persistSettings();
      renderCapabilities();
      renderSession();
      // Sesi aktif → live switch via backend (tanpa putus video).
      if (isTauri && mirrorStatus.state === "connected") {
        void switchAudioRoute(next, previous);
      }
    });
  });

  $("btn-start").onclick = async () => {
    mirrorStatus = { state: "offering" };
    renderSession();
    try {
      await invoke("start_sharing", {
        settings: {
          receiverId: selectedReceiver,
          displayMode: displayMode === "mirror"
            ? { kind: "mirror", displayId: selectedDisplayId }
            : { kind: "extended", width: 1920, height: 1080, refreshHz: 30 },
          audioRoute,
          video: {
            resolution: videoSettings.resolution,
            frameRate: videoSettings.frameRate,
            quality: videoSettings.quality,
          },
        },
      });
    } catch (error) {
      mirrorStatus = { state: "error", message: String(error) };
      renderAll();
    }
  };
  $("btn-stop").onclick = async () => {
    try {
      await invoke("stop_mirroring");
      mirrorStatus = { state: "idle" };
      pipelineInfo = null;
      pipelineStats = null;
      renderAll();
    } catch (error) {
      mirrorStatus = { state: "error", message: String(error) };
      renderAll();
    }
  };
  $("btn-pairing").onclick = () => ($("pairing-dialog") as HTMLDialogElement).showModal();
  $("btn-help").onclick = () => void openHelp();
  $("btn-diagnostics").onclick = () => ($("diagnostics-dialog") as HTMLDialogElement).showModal();
  const langSelect = $("language-select") as HTMLSelectElement;
  langSelect.value = locale();
  langSelect.addEventListener("change", () => {
    setLocale(langSelect.value as Locale);
    // Terjemahan statis sudah ditukar DOM-walk; segarkan teks dinamis.
    langSelect.value = locale();
    renderAll();
  });
  $("btn-export-diagnostics").onclick = async () => {
    const note = $("diagnostic-export-note");
    if (!isTauri) {
      note.textContent = t("Ekspor tersedia di aplikasi WDT (bukan mode pratinjau).");
      return;
    }
    note.textContent = t("Menyiapkan berkas…");
    try {
      const path = await invoke<string>("export_diagnostics");
      note.textContent = `Tersimpan lokal: ${path}`;
    } catch (error) {
      note.textContent = `Gagal mengekspor: ${String(error)}`;
    }
  };
  $("btn-copy-pairing").onclick = async () => {
    if (!pairingInfo?.pairingString) return;
    await navigator.clipboard.writeText(pairingInfo.pairingString);
    $("btn-copy-pairing").textContent = t("Detail tersalin");
    window.setTimeout(() => { $("btn-copy-pairing").textContent = t("Salin detail koneksi"); }, 1800);
  };
  $("btn-save-device-name").onclick = async () => {
    const input = $("device-name-input") as HTMLInputElement;
    const note = $("device-name-note");
    const name = input.value.trim();
    if (name.length < 2) {
      note.textContent = "Gunakan nama antara 2–32 karakter.";
      return;
    }
    if (!isTauri) {
      if (pairingInfo) pairingInfo.displayName = name;
      note.textContent = "Nama disimpan untuk pratinjau ini.";
      return;
    }
    try {
      await invoke("set_device_name", { name });
      note.textContent = "Nama tersimpan. TV di jaringan akan melihat nama baru ini.";
      await refreshAll();
    } catch (error) {
      note.textContent = `Nama belum tersimpan: ${String(error)}`;
    }
  };
  document.querySelectorAll<HTMLDialogElement>("dialog").forEach((dialog) => {
    dialog.addEventListener("click", (event) => { if (event.target === dialog) dialog.close(); });
  });
}

async function bindEvents(): Promise<void> {
  if (!isTauri) return;
  await listen("receiver-joined", () => void refreshAll());
  await listen("receiver-left", () => void refreshAll());
  await listen<MirrorStatus>("mirror-status", (event) => {
    mirrorStatus = event.payload;
    if (mirrorStatus.state === "idle") {
      pipelineInfo = null;
      pipelineStats = null;
    }
    renderAll();
  });
  await listen<PipelineInfo>("mirror-pipeline", (event) => { pipelineInfo = event.payload; renderAll(); });
  await listen<PipelineStats>("mirror-stats", (event) => { pipelineStats = event.payload; renderSession(); });
  await listen<AudioStats>("audio-stats", (event) => { audioStats = event.payload; renderDiagnostics(); });
  await listen<{ route: AudioRoute }>("audio-route-changed", (event) => {
    audioRoute = event.payload.route;
    audioRouteChanging = false;
    audioNotice = null;
    persistSettings();
    renderAll();
  });
  await listen<{ message: string }>("audio-degraded", (event) => {
    audioRouteChanging = false;
    audioNotice = event.payload.message;
    audioStats = null;
    renderAll();
  });
  await listen("server-status", () => void refreshAll());
  await listen("identity-changed", () => void refreshAll());
}

window.addEventListener("DOMContentLoaded", async () => {
  initLocale();
  bindControls();
  await bindEvents();
  await refreshAll();
});
