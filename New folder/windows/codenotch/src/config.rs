use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// How small the notch may be drawn, as a multiple of its designed size. Below roughly 0.4 the
/// rings stop being readable at 100 % display scaling.
pub const SCALE_MIN: f64 = 0.40;
pub const SCALE_MAX: f64 = 1.00;

/// One half of the tray icon: which provider, and which of its windows.
/// `window` is a window id as the provider reports it ("session", "weekly_all", "primary"…), or
/// the empty string / "top" meaning "whichever of its windows is fullest" — the same rule the
/// notch ring uses, and the only choice that keeps working when a provider changes its windows.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TraySlot {
    pub provider: String,
    #[serde(default)]
    pub window: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    #[serde(default = "default_port")]
    pub port: u16,
    /// "auto" | "zh" | "en" | "ja" | "ko" | "ru"
    #[serde(default = "default_lang")]
    pub lang: String,
    #[serde(default)]
    pub bar_x: Option<i32>,
    #[serde(default)]
    pub bar_y: Option<i32>,
    /// Logical width of the bar (wheel-adjustable, 220-520); None = default 360
    #[serde(default)]
    pub bar_w: Option<u32>,
    /// Allow dragging + wheel resizing (tray toggle, off by default to prevent accidental drags)
    #[serde(default)]
    pub drag_enabled: bool,
    /// Vertical position of the notch: the window centre as a fraction of the primary monitor's height (0 = top, 1 = bottom), default 0.5; saved after a drag
    #[serde(default = "default_notch_y")]
    pub notch_y: f64,
    /// Notch size as a multiple of the designed size (slider at the foot of the hover card).
    /// Only the pill is scaled — the hover card keeps its size, so the slider does not move
    /// while it is being dragged.
    #[serde(default = "default_scale")]
    pub scale: f64,
    /// What the tray icon draws: "off" (the plain mark, the previous behaviour and the default),
    /// "numbers" (up to two readings as digits) or "bars" (a column per reading).
    #[serde(default = "default_tray_mode")]
    pub tray_mode: String,
    /// Which providers the tray icon covers, in the order they are drawn. Ids match the page:
    /// "claude", "codex", "cursor", "gemini". Superseded by `tray_slots`; kept so an existing
    /// config still upgrades cleanly, and migrated in `load()`.
    #[serde(default = "default_tray_providers")]
    pub tray_providers: Vec<String>,
    /// What each part of the tray icon shows, in drawing order: the first entry is the top half of
    /// the digit layout, the second the bottom half, and the bar layout uses them all in order.
    #[serde(default)]
    pub tray_slots: Vec<TraySlot>,
    /// Which providers the notch itself shows, in order. Empty means every provider that has
    /// something to report — the original behaviour, and the default. Superseded by `notch_slots`,
    /// kept so an existing config migrates cleanly.
    #[serde(default)]
    pub notch_providers: Vec<String>,
    /// What each ring on the notch shows: the provider, and which of its windows. An empty list
    /// means every provider, each showing whichever of its windows is fullest — the original
    /// behaviour. Same shape as the tray slots so the two settings read alike.
    #[serde(default)]
    pub notch_slots: Vec<TraySlot>,
    /// false = the pill is kept off the screen edge entirely; the tray icon is then the only way in
    #[serde(default = "yes")]
    pub notch_visible: bool,
    /// false = the tray icon is hidden. Refused while the notch is also hidden, because that would
    /// leave the app running with no way to reach it.
    #[serde(default = "yes")]
    pub tray_visible: bool,
    /// Global hotkey to toggle notch visibility (e.g., "Ctrl+Alt+N")
    #[serde(default = "default_hotkey")]
    pub hotkey_toggle: String,
    /// Visual material style: "windows" (Windows 11 Acrylic & taskbar tint), "apple" (frosted milky glass),
    /// "oled" (pure deep black), "solid" (matte dark), "custom" (custom accent)
    #[serde(default = "default_appearance_style")]
    pub appearance_style: String,
    /// Custom accent color hex (e.g., "#0078d4")
    #[serde(default = "default_custom_accent")]
    pub custom_accent: String,
    /// Auto-hide notch at screen edge when cursor is away
    #[serde(default)]
    pub autohide: bool,
    /// Delay in seconds before auto-hiding after cursor leaves (default 5s)
    #[serde(default = "default_autohide_delay")]
    pub autohide_delay: u32,
    /// Screen edge attachment: "right" (default), "left", or "top"
    #[serde(default = "default_screen_edge")]
    pub screen_edge: String,
    /// Peek width in px when docked/auto-hidden (0 = fully hidden, default 14px)
    #[serde(default = "default_autohide_peek")]
    pub autohide_peek: u32,
    /// Enable system hardware and network metrics monitoring
    #[serde(default = "yes")]
    pub hardware_enabled: bool,
    /// Transparent in-game hardware overlay window active
    #[serde(default)]
    pub overlay_enabled: bool,
    /// Overlay opacity (0.1 to 1.0, default 0.85)
    #[serde(default = "default_overlay_opacity")]
    pub overlay_opacity: f64,
    /// Overlay click-through mode (transparent to mouse clicks)
    #[serde(default)]
    pub overlay_clickthrough: bool,
    /// Saved overlay X position
    #[serde(default)]
    pub overlay_x: Option<i32>,
    /// Saved overlay Y position
    #[serde(default)]
    pub overlay_y: Option<i32>,
    /// System tray display mode: "ai" (AI tokens), "hardware" (CPU/RAM), "network" (Speed/Daily)
    #[serde(default = "default_tray_display_mode")]
    pub tray_display_mode: String,
    /// Global hotkey to toggle overlay HUD (e.g. "Ctrl+Alt+H")
    #[serde(default = "default_hotkey_overlay")]
    pub hotkey_overlay: String,
    /// Global hotkey to toggle overlay click-through lock (e.g. "Ctrl+Alt+L")
    #[serde(default = "default_hotkey_lock")]
    pub hotkey_lock: String,
    /// Overlay font size in pixels (min 9, max 36, default 14)
    #[serde(default = "default_overlay_font_size")]
    pub overlay_font_size: u32,
    /// Overlay color theme: "msi", "cyber", "mono"
    #[serde(default = "default_overlay_theme")]
    pub overlay_theme: String,
    /// Overlay background tint shade (0.0 to 0.6)
    #[serde(default = "default_overlay_bg_opacity")]
    pub overlay_bg_opacity: f64,
    /// Metrics visibility toggles
    #[serde(default = "default_true")]
    pub overlay_show_fps: bool,
    #[serde(default = "default_true")]
    pub overlay_show_low: bool,
    #[serde(default = "default_true")]
    pub overlay_show_gpu: bool,
    #[serde(default = "default_true")]
    pub overlay_show_vram: bool,
    #[serde(default = "default_true")]
    pub overlay_show_cpu: bool,
    #[serde(default = "default_true")]
    pub overlay_show_ram: bool,
    #[serde(default = "default_true")]
    pub overlay_show_page: bool,
    #[serde(default = "default_true")]
    pub overlay_show_net: bool,
    #[serde(default = "default_true")]
    pub overlay_show_day: bool,
    #[serde(default = "default_false")]
    pub overlay_show_app: bool,
    /// Whether user completed the initial first-run onboarding tour
    #[serde(default = "default_false")]
    pub first_run_completed: bool,
    /// HUD Dock corner: "top-left", "top-right", "bottom-left", "bottom-right", "custom"
    #[serde(default = "default_overlay_dock")]
    pub overlay_dock: String,
    /// HUD margin from screen horizontal edge in px (0 = flush against bezel)
    #[serde(default = "default_overlay_margin")]
    pub overlay_margin_x: i32,
    /// HUD margin from screen vertical edge in px (0 = flush against bezel)
    #[serde(default = "default_overlay_margin")]
    pub overlay_margin_y: i32,
}

fn default_overlay_dock() -> String {
    "top-left".into()
}

fn default_overlay_margin() -> i32 {
    8
}

fn default_overlay_font_size() -> u32 {
    14
}

fn default_overlay_theme() -> String {
    "msi".into()
}

fn default_overlay_bg_opacity() -> f64 {
    0.0
}

fn default_true() -> bool {
    true
}

fn default_false() -> bool {
    false
}

fn default_overlay_opacity() -> f64 {
    0.85
}

fn default_tray_display_mode() -> String {
    "ai".into()
}

fn default_hotkey_overlay() -> String {
    "Ctrl+Alt+H".into()
}

fn default_hotkey_lock() -> String {
    "Ctrl+Alt+L".into()
}

fn default_screen_edge() -> String {
    "right".into()
}

fn default_autohide_peek() -> u32 {
    14
}

fn default_hotkey() -> String {
    "Ctrl+Alt+N".into()
}

fn default_appearance_style() -> String {
    "windows".into()
}

fn default_custom_accent() -> String {
    "#0078d4".into()
}

fn default_autohide_delay() -> u32 {
    5
}

fn default_notch_y() -> f64 {
    0.5
}
fn default_scale() -> f64 {
    1.0
}
fn yes() -> bool {
    true
}
/// A fresh install shows the two readings straight away — a tray icon nobody knows to look for is
/// a feature nobody finds. An install that predates this setting is handled in `load()` instead:
/// it keeps the plain mark it already has, so upgrading never changes anyone's icon unasked.
fn default_tray_mode() -> String {
    "numbers".into()
}
fn default_tray_providers() -> Vec<String> {
    vec!["claude".into(), "codex".into()]
}

fn default_port() -> u16 {
    48666
}
fn default_lang() -> String {
    "auto".into()
}

impl Default for Config {
    fn default() -> Self {
        Self {
            port: default_port(),
            lang: default_lang(),
            bar_x: None,
            bar_y: None,
            bar_w: None,
            drag_enabled: false,
            notch_y: default_notch_y(),
            scale: default_scale(),
            tray_mode: default_tray_mode(),
            tray_providers: default_tray_providers(),
            tray_slots: Vec::new(), // filled in by load(), from tray_providers
            notch_providers: Vec::new(), // empty = show them all
            notch_slots: Vec::new(),     // filled in by load(), from notch_providers
            notch_visible: true,
            tray_visible: true,
            hotkey_toggle: default_hotkey(),
            appearance_style: default_appearance_style(),
            custom_accent: default_custom_accent(),
            autohide: false,
            autohide_delay: default_autohide_delay(),
            screen_edge: default_screen_edge(),
            autohide_peek: default_autohide_peek(),
            hardware_enabled: true,
            overlay_enabled: false,
            overlay_opacity: default_overlay_opacity(),
            overlay_clickthrough: false,
            overlay_x: None,
            overlay_y: None,
            tray_display_mode: default_tray_display_mode(),
            hotkey_overlay: default_hotkey_overlay(),
            hotkey_lock: default_hotkey_lock(),
            overlay_font_size: default_overlay_font_size(),
            overlay_theme: default_overlay_theme(),
            overlay_bg_opacity: default_overlay_bg_opacity(),
            overlay_show_fps: true,
            overlay_show_low: true,
            overlay_show_gpu: true,
            overlay_show_vram: true,
            overlay_show_cpu: true,
            overlay_show_ram: true,
            overlay_show_page: true,
            overlay_show_net: true,
            overlay_show_day: true,
            overlay_show_app: false,
            first_run_completed: false,
            overlay_dock: default_overlay_dock(),
            overlay_margin_x: default_overlay_margin(),
            overlay_margin_y: default_overlay_margin(),
        }
    }
}

pub fn config_path() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("codenotch")
        .join("config.json")
}

pub fn load() -> Config {
    let path = config_path();
    let raw = std::fs::read_to_string(&path).ok();
    let mut cfg: Config = raw
        .as_deref()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default();

    // If upgrading an existing config file that predated first_run_completed, assume it was completed
    let had_first_run_key = raw
        .as_deref()
        .and_then(|t| serde_json::from_str::<serde_json::Value>(t).ok())
        .map(|v| v.get("first_run_completed").is_some())
        .unwrap_or(false);
    if raw.is_some() && !had_first_run_key {
        cfg.first_run_completed = true;
    }

    // Discoverability without surprising anyone. `default_tray_mode` gives a NEW install the
    // numbers icon, but serde applies that same default to an EXISTING config that simply predates
    // the setting — which would silently change the tray icon of everyone who upgrades. So an
    // existing file with no `tray_mode` key is pinned to the plain mark it already has; only a
    // machine with no config at all gets the new default.
    let upgrading = raw
        .as_deref()
        .and_then(|t| serde_json::from_str::<serde_json::Value>(t).ok())
        .map(|v| v.get("tray_mode").is_none())
        .unwrap_or(false);
    if upgrading {
        cfg.tray_mode = "off".into();
    }

    // Migration: before slots existed the icon was a plain provider list, each showing whichever of
    // its windows was fullest. That is exactly a slot with an empty `window`, so nobody's choice is
    // lost and nobody has to reconfigure anything.
    if cfg.tray_slots.is_empty() {
        cfg.tray_slots = cfg
            .tray_providers
            .iter()
            .map(|p| TraySlot { provider: p.clone(), window: String::new() })
            .collect();
    }

    // Same migration for the notch: a plain provider list becomes slots that each show whichever
    // window is fullest, which is exactly what the list used to mean.
    if cfg.notch_slots.is_empty() {
        cfg.notch_slots = cfg
            .notch_providers
            .iter()
            .map(|p| TraySlot { provider: p.clone(), window: String::new() })
            .collect();
    }

    // Both hidden would leave the app unreachable: no pill, no tray icon, no way to open settings.
    if !cfg.notch_visible && !cfg.tray_visible {
        cfg.tray_visible = true;
    }

    // A hand-edited file must not be able to produce an invisible window
    cfg.scale = cfg.scale.clamp(SCALE_MIN, SCALE_MAX);
    cfg
}

pub fn save(cfg: &Config) {
    let path = config_path();
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Ok(txt) = serde_json::to_string_pretty(cfg) {
        let _ = std::fs::write(path, txt);
    }
}
