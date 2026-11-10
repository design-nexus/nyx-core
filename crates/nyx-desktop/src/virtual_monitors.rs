//! Virtual monitors: one physical output split into two or three monitors by Nyx's
//! Hyprland (`hl.virtual_monitor`, behind `experimental:virtual_monitors`).
//!
//! Hyprland owns the live state (regions, their workspaces, merged or not); this module
//! is the one place Nyx keeps what should happen at login, and the typed way every Nyx
//! client changes it:
//!
//! - [`Settings`]: `~/.config/nyx/virtual-monitors.toml`, under NyxOS's defaults and an
//!   administrator's overrides (`/etc/nyxos/{defaults,overrides}/virtual-monitors.toml`).
//!   [`Settings::lua`] renders it into `~/.config/hypr/nyx-virtual-monitors.lua`, which
//!   the Hyprland config loads; on a Hyprland without virtual monitors the file does
//!   nothing.
//! - [`status`]: Hyprland's view (`j/virtualmonitors`).
//! - [`apply`], [`confirm`], [`merge`], [`restore`], [`disable_all`]: runtime changes.
//!   A layout applied with a timeout reverts inside Hyprland unless confirmed, and is
//!   saved only once confirmed, so neither a crash nor a closed Settings window leaves an
//!   unwanted layout behind.
//! - [`partition`]: the same exact split Hyprland makes, for previews and validation.

use crate::hypr::Instance;
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// The settings domain's file name, in `~/.config/nyx` and the NyxOS layers.
pub const CONFIG_FILE: &str = "virtual-monitors.toml";
/// The generated Hyprland config, in `~/.config/hypr`.
pub const LUA_FILE: &str = "nyx-virtual-monitors.lua";
/// The Lua line a Hyprland config uses to load it.
pub const REQUIRE_LINE: &str = r#"pcall(dofile, os.getenv("HOME") .. "/.config/hypr/nyx-virtual-monitors.lua")"#;
/// Regions per output.
pub const MAX_REGIONS: usize = 3;
/// Hyprland's default for `virtual_monitors:min_region_width`.
pub const MIN_REGION_WIDTH: f64 = 640.0;
/// Seconds a risky change waits for confirmation by default.
pub const DEFAULT_CONFIRM_SECONDS: u32 = 15;

/// Which region workspace shortcuts act on.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Target {
    #[default]
    Focus,
    Pointer,
}

/// What fullscreen does on a virtual monitor.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Fullscreen {
    /// Fill the region; the others stay usable.
    #[default]
    Region,
    /// Merge the regions for the window and fill the whole screen.
    Physical,
}

/// A named split.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Preset {
    /// One region: the screen as it is.
    Full,
    #[default]
    Equal,
    /// Three regions, the middle one twice as wide.
    CenterFocused,
    /// The proportions in [`Layout::ratios`].
    Custom,
}

impl Preset {
    pub fn as_str(self) -> &'static str {
        match self {
            Preset::Full => "full",
            Preset::Equal => "equal",
            Preset::CenterFocused => "center-focused",
            Preset::Custom => "custom",
        }
    }
}

/// How one screen is split.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Layout {
    pub preset: Preset,
    /// Regions, 1 to 3 (presets other than custom).
    pub count: u8,
    /// Proportions, left to right (custom).
    pub ratios: Vec<f64>,
}

impl Default for Layout {
    fn default() -> Self {
        Layout { preset: Preset::Equal, count: 2, ratios: Vec::new() }
    }
}

impl Layout {
    pub fn full() -> Self {
        Layout { preset: Preset::Full, count: 1, ratios: Vec::new() }
    }

    pub fn equal(count: u8) -> Self {
        Layout { preset: Preset::Equal, count, ratios: Vec::new() }
    }

    pub fn center_focused() -> Self {
        Layout { preset: Preset::CenterFocused, count: 3, ratios: Vec::new() }
    }

    pub fn custom(ratios: Vec<f64>) -> Self {
        Layout { preset: Preset::Custom, count: ratios.len() as u8, ratios }
    }

    /// The proportions this layout stands for.
    pub fn proportions(&self) -> Result<Vec<f64>> {
        let ratios = match self.preset {
            Preset::Full => vec![1.0],
            Preset::Equal => vec![1.0; self.count.clamp(1, MAX_REGIONS as u8) as usize],
            Preset::CenterFocused => vec![1.0, 2.0, 1.0],
            Preset::Custom => self.ratios.clone(),
        };
        if ratios.is_empty() || ratios.len() > MAX_REGIONS {
            bail!("a layout needs 1 to {MAX_REGIONS} regions");
        }
        if ratios.iter().any(|r| !r.is_finite() || *r <= 0.0) {
            bail!("region proportions must be positive");
        }
        Ok(ratios)
    }

    pub fn regions(&self) -> usize {
        self.proportions().map_or(1, |r| r.len())
    }
}

/// One physical screen's virtual monitors.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Display {
    /// The screen's [`crate::Monitor::stable_id`] (`make|model|serial`).
    pub id: String,
    /// Hyprland's description of it (`make model serial`), matched with `desc:`; the
    /// connector is used when it has none.
    pub description: String,
    /// The connector it was last seen on, for screens that report no identity.
    pub connector: String,
    pub enabled: bool,
    pub layout: Layout,
    /// The [`Settings::profiles`] entry the layout came from, if any.
    pub profile: String,
    /// Workspaces on each region after the first, 1 to 10.
    pub workspaces: u8,
}

impl Default for Display {
    fn default() -> Self {
        Display {
            id: String::new(),
            description: String::new(),
            connector: String::new(),
            enabled: true,
            layout: Layout::default(),
            profile: String::new(),
            workspaces: 10,
        }
    }
}

impl Display {
    /// The Hyprland monitor selector for this screen.
    pub fn selector(&self) -> String {
        if self.description.trim().is_empty() { self.connector.clone() } else { format!("desc:{}", self.description.trim()) }
    }

    /// Whether this entry is for `monitor` (a physical screen, or a split one's host).
    pub fn matches(&self, stable_id: &str, connector: &str) -> bool {
        if !self.id.is_empty() && self.id == stable_id && !stable_id.ends_with('|') {
            return true;
        }
        // screens without a serial, or two identical ones: fall back to the port
        (self.id.is_empty() || self.id == stable_id) && self.connector == connector
    }
}

/// `virtual-monitors.toml`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// Master switch. Off: no screen is split, whatever [`Settings::displays`] says.
    pub enabled: bool,
    pub workspace_target: Target,
    pub fullscreen: Fullscreen,
    /// Seconds a layout change waits for confirmation before Hyprland reverts it.
    pub confirm_seconds: u32,
    #[serde(rename = "display")]
    pub displays: Vec<Display>,
    /// Saved layouts by name.
    pub profiles: BTreeMap<String, Layout>,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            enabled: false,
            workspace_target: Target::Focus,
            fullscreen: Fullscreen::Region,
            confirm_seconds: DEFAULT_CONFIRM_SECONDS,
            displays: Vec::new(),
            profiles: BTreeMap::from([
                ("Full width".into(), Layout::full()),
                ("Two equal".into(), Layout::equal(2)),
                ("Three equal".into(), Layout::equal(3)),
                ("Center focused".into(), Layout::center_focused()),
            ]),
        }
    }
}

/// Where the settings live.
#[derive(Debug, Clone)]
pub struct Paths {
    /// Lowest first: NyxOS's defaults, then an administrator's overrides.
    pub layers: Vec<PathBuf>,
    /// The user's file, the only one written.
    pub user: PathBuf,
    /// The generated Hyprland config.
    pub lua: PathBuf,
}

impl Paths {
    pub fn default_paths() -> Result<Self> {
        let config = std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
            .context("neither XDG_CONFIG_HOME nor HOME is set")?;
        Ok(Paths {
            layers: vec![
                PathBuf::from("/etc/nyxos/defaults").join(CONFIG_FILE),
                PathBuf::from("/etc/nyxos/overrides").join(CONFIG_FILE),
            ],
            user: config.join("nyx").join(CONFIG_FILE),
            lua: config.join("hypr").join(LUA_FILE),
        })
    }
}

impl Settings {
    /// Read the layers and the user's file. Missing files are fine; one that doesn't
    /// parse is an error naming it, so it is never silently replaced.
    pub fn load(paths: &Paths) -> Result<Settings> {
        let mut merged = toml::Table::new();
        for path in paths.layers.iter().chain([&paths.user]) {
            let text = match std::fs::read_to_string(path) {
                Ok(t) => t,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                Err(e) => return Err(e).with_context(|| format!("read {}", path.display())),
            };
            let table: toml::Table = toml::from_str(&text).with_context(|| format!("{} isn't valid TOML", path.display()))?;
            merge_tables(&mut merged, table);
        }
        let settings: Settings = toml::Value::Table(merged).try_into().context("unexpected virtual monitor settings")?;
        Ok(settings)
    }

    /// Write the user's file and the generated Hyprland config.
    pub fn save(&self, paths: &Paths) -> Result<()> {
        write_atomic(&paths.user, &toml::to_string_pretty(self)?)?;
        write_atomic(&paths.lua, &self.lua())
    }

    pub fn display(&self, stable_id: &str, connector: &str) -> Option<&Display> {
        self.displays.iter().find(|d| d.matches(stable_id, connector))
    }

    /// The entry for a screen, made if it has none.
    pub fn display_mut(&mut self, monitor: &crate::Monitor) -> &mut Display {
        let id = monitor.stable_id();
        let i = match self.displays.iter().position(|d| d.matches(&id, &monitor.name)) {
            Some(i) => i,
            None => {
                self.displays.push(Display { id: id.clone(), ..Display::default() });
                self.displays.len() - 1
            }
        };
        let d = &mut self.displays[i];
        d.id = id;
        d.connector = monitor.name.clone();
        d.description = monitor.description.clone();
        d
    }

    /// The Hyprland Lua for these settings. It checks for `hl.virtual_monitor` first, so
    /// a Hyprland without virtual monitors loads it without errors.
    pub fn lua(&self) -> String {
        let mut out = String::from(
            "-- Generated by Nyx from virtual-monitors.toml. Change it in Nyx Settings (Displays),\n\
             -- or with `nyx display virtual`; edits here are overwritten.\n\
             -- To turn virtual monitors off without either: hyprctl dispatch 'hl.dsp.virtual_monitor.disable_all()'\n\
             if not hl.virtual_monitor then return end\n",
        );
        out += &format!(
            "hl.config({{ experimental = {{ virtual_monitors = {} }}, virtual_monitors = {{ workspace_target = {}, fullscreen = {} }} }})\n",
            self.enabled,
            lua_string(match self.workspace_target {
                Target::Focus => "focus",
                Target::Pointer => "pointer",
            }),
            lua_string(match self.fullscreen {
                Fullscreen::Region => "region",
                Fullscreen::Physical => "physical",
            }),
        );
        for d in &self.displays {
            out += &display_call(d, None);
            out.push('\n');
        }
        out
    }
}

fn display_call(d: &Display, confirm: Option<u32>) -> String {
    let ratios = d.layout.proportions().unwrap_or_else(|_| vec![1.0]);
    let enabled = d.enabled && ratios.len() > 1;
    let list = ratios.iter().map(|r| format_number(*r)).collect::<Vec<_>>().join(", ");
    let mut call = format!(
        "hl.virtual_monitor({{ output = {}, enabled = {}, regions = {{ {} }}, layout = {}, workspaces = {}, profile = {}",
        lua_string(&d.selector()),
        enabled,
        list,
        lua_string(d.layout.preset.as_str()),
        d.workspaces.clamp(1, 10),
        lua_string(&d.profile)
    );
    if let Some(seconds) = confirm {
        call += &format!(", confirm = {seconds}");
    }
    call + " })"
}

fn format_number(n: f64) -> String {
    let s = format!("{n:.4}");
    s.trim_end_matches('0').trim_end_matches('.').to_string()
}

fn lua_string(s: &str) -> String {
    let mut out = String::from("\"");
    for c in s.chars() {
        match c {
            '"' => out += "\\\"",
            '\\' => out += "\\\\",
            '\n' => out += "\\n",
            c if (c as u32) < 0x20 => out += &format!("\\{}", c as u32),
            c => out.push(c),
        }
    }
    out + "\""
}

fn merge_tables(into: &mut toml::Table, from: toml::Table) {
    for (k, v) in from {
        match (into.get_mut(&k), v) {
            (Some(toml::Value::Table(a)), toml::Value::Table(b)) => merge_tables(a, b),
            (_, v) => {
                into.insert(k, v);
            }
        }
    }
}

fn write_atomic(path: &Path, contents: &str) -> Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("create {}", dir.display()))?;
    }
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, contents).with_context(|| format!("write {}", tmp.display()))?;
    std::fs::rename(&tmp, path).with_context(|| format!("replace {}", path.display()))
}

/// One region of a span, in physical pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Span {
    pub x: i64,
    pub width: i64,
}

/// The smallest pixel count that is a whole number of logical pixels at `scale`.
pub fn logical_pixel_step(scale: f64) -> i64 {
    if !scale.is_finite() || scale <= 0.0 {
        return 1;
    }
    (1..=120).find(|step| ((*step as f64 / scale) - (*step as f64 / scale).round()).abs() < 1e-6).unwrap_or(1)
}

/// Hyprland's split of `pixel_width` into `ratios`: adjacent spans that cover it exactly,
/// every boundary on a whole logical pixel, none narrower than `min_logical_width`.
pub fn partition(pixel_width: i64, scale: f64, ratios: &[f64], min_logical_width: f64) -> Result<Vec<Span>> {
    if ratios.is_empty() || ratios.len() > MAX_REGIONS {
        bail!("need 1 to {MAX_REGIONS} regions, got {}", ratios.len());
    }
    if pixel_width <= 0 {
        bail!("the output has no width");
    }
    if !scale.is_finite() || scale <= 0.0 {
        bail!("invalid scale");
    }
    if ratios.iter().any(|r| !r.is_finite() || *r <= 0.0) {
        bail!("region proportions must be positive");
    }
    let total: f64 = ratios.iter().sum();
    let step = logical_pixel_step(scale);
    let mut spans = Vec::with_capacity(ratios.len());
    let (mut cumulative, mut start) = (0.0, 0i64);
    for (i, r) in ratios.iter().enumerate() {
        cumulative += r;
        let end = if i + 1 < ratios.len() {
            (cumulative / total * pixel_width as f64 / step as f64).round() as i64 * step
        } else {
            pixel_width
        };
        if end <= start || end > pixel_width {
            bail!("region {} would be empty", i + 1);
        }
        spans.push(Span { x: start, width: end - start });
        start = end;
    }
    for (i, s) in spans.iter().enumerate() {
        let logical = s.width as f64 / scale;
        if logical + 1e-6 < min_logical_width {
            bail!("region {} would be {logical:.0} logical pixels wide, less than the minimum of {min_logical_width:.0}", i + 1);
        }
    }
    Ok(spans)
}

/// Check a layout against a screen before applying it.
pub fn validate(layout: &Layout, pixel_width: i32, scale: f64) -> Result<Vec<Span>> {
    partition(pixel_width as i64, scale, &layout.proportions()?, MIN_REGION_WIDTH)
}

/// Hyprland's state, from `j/virtualmonitors`.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Status {
    /// `experimental:virtual_monitors` is on and nothing turned them off.
    pub available: bool,
    /// Turned off with `disable_all` until the next config reload.
    pub killed: bool,
    /// Seconds left before an unconfirmed change reverts.
    pub pending_confirm: Option<u32>,
    pub groups: Vec<Group>,
}

/// One split screen as Hyprland has it.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Group {
    /// The selector it was configured with.
    pub output: String,
    pub host: String,
    pub make: String,
    pub model: String,
    pub serial: String,
    /// `split` or `merged`.
    pub state: String,
    pub layout: String,
    pub profile: String,
    pub workspaces: u32,
    pub regions: Vec<Region>,
}

impl Group {
    pub fn merged(&self) -> bool {
        self.state == "merged"
    }

    pub fn stable_id(&self) -> String {
        if self.make.trim().is_empty() && self.model.trim().is_empty() {
            return self.host.clone();
        }
        format!("{}|{}|{}", self.make.trim(), self.model.trim(), self.serial.trim())
    }
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Region {
    pub index: u32,
    pub name: String,
    pub x: i64,
    pub width: i64,
    pub ratio: f64,
    pub workspace_base: i64,
    /// -1 while merged.
    pub active_workspace: i64,
}

/// Whether the running Hyprland has virtual monitors at all.
pub fn supported(instance: &Instance) -> bool {
    status(instance).is_ok()
}

pub fn status(instance: &Instance) -> Result<Status> {
    instance.json("virtualmonitors").context("this Hyprland has no virtual monitors (it needs Nyx's Hyprland 0.57.1 or newer)")
}

fn eval(instance: &Instance, lua: &str) -> Result<()> {
    let reply = instance.request(&format!("eval {lua}"))?;
    if reply.trim() != "ok" {
        bail!("Hyprland refused the change: {}", reply.trim());
    }
    Ok(())
}

fn dispatch(instance: &Instance, call: &str) -> Result<()> {
    let reply = instance.request(&format!("dispatch {call}"))?;
    if reply.trim() != "ok" {
        bail!("Hyprland refused `{call}`: {}", reply.trim());
    }
    Ok(())
}

/// Put the global options (master switch, targeting, fullscreen) into effect now.
pub fn apply_options(instance: &Instance, settings: &Settings) -> Result<()> {
    supported(instance).then_some(()).context("this Hyprland has no virtual monitors")?;
    let lua = settings.lua();
    let options = lua.lines().find(|l| l.starts_with("hl.config(")).unwrap_or_default();
    eval(instance, options)
}

/// Put one screen's layout into effect now. With `confirm`, Hyprland reverts it after
/// that many seconds unless [`confirm`] is called; save the settings only after that.
pub fn apply(instance: &Instance, settings: &Settings, display: &Display, confirm: Option<u32>) -> Result<()> {
    supported(instance).then_some(()).context("this Hyprland has no virtual monitors")?;
    if display.enabled && display.layout.regions() > 1 && !settings.enabled {
        bail!("virtual monitors are turned off; turn them on first");
    }
    apply_options(instance, settings)?;
    eval(instance, &display_call(display, confirm.filter(|s| *s > 0)))
}

/// Keep the change waiting for confirmation.
pub fn confirm(instance: &Instance) -> Result<()> {
    dispatch(instance, "hl.dsp.virtual_monitor.confirm()")
}

/// Merge a split screen into one for now (`monitor`: the screen or any of its regions).
pub fn merge(instance: &Instance, monitor: &str) -> Result<()> {
    dispatch(instance, &format!("hl.dsp.virtual_monitor.merge({{ monitor = {} }})", lua_string(monitor)))
}

/// Split a merged screen again, with its workspaces back where they were.
pub fn restore(instance: &Instance, monitor: &str) -> Result<()> {
    dispatch(instance, &format!("hl.dsp.virtual_monitor.restore({{ monitor = {} }})", lua_string(monitor)))
}

/// Recovery: give every screen back whole until the Hyprland config is next reloaded.
pub fn disable_all(instance: &Instance) -> Result<()> {
    dispatch(instance, "hl.dsp.virtual_monitor.disable_all()")
}

/// Which user workspace (1 to 10) each region of each split screen shows, by the
/// screen's stable id; [`reapply_active`] shows them again after a login.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SessionState {
    pub active: BTreeMap<String, Vec<u32>>,
}

impl SessionState {
    pub fn path() -> Result<PathBuf> {
        let state = std::env::var_os("XDG_STATE_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/state")))
            .context("neither XDG_STATE_HOME nor HOME is set")?;
        Ok(state.join("nyx").join("virtual-monitors.json"))
    }

    pub fn load(path: &Path) -> SessionState {
        std::fs::read_to_string(path).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default()
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        write_atomic(path, &serde_json::to_string_pretty(self)?)
    }

    /// Record what the regions show now. Returns whether anything changed.
    pub fn remember(&mut self, desktop: &crate::Desktop) -> bool {
        let mut changed = false;
        for host in &desktop.hosts {
            let numbers: Vec<u32> = host
                .regions
                .iter()
                .map(|r| {
                    desktop
                        .monitor(r)
                        .and_then(|m| desktop.workspaces.iter().find(|w| w.id == m.active_workspace))
                        .and_then(|w| w.number())
                        .unwrap_or(1)
                })
                .collect();
            if self.active.get(&host.stable_id) != Some(&numbers) {
                self.active.insert(host.stable_id.clone(), numbers);
                changed = true;
            }
        }
        changed
    }
}

impl SessionState {
    /// The workspaces to show again so every region of every split screen is back on its
    /// remembered one, as workspace ids; already-shown ones are left out.
    pub fn plan(&self, desktop: &crate::Desktop) -> Vec<i64> {
        let mut ids = Vec::new();
        for host in &desktop.hosts {
            let Some(numbers) = self.active.get(&host.stable_id) else { continue };
            for (region, number) in host.regions.iter().zip(numbers) {
                let Some(m) = desktop.monitor(region) else { continue };
                let id = desktop.workspace_id(region, *number);
                if m.active_workspace != id {
                    ids.push(id);
                }
            }
        }
        ids
    }
}

/// Show each region's remembered workspace again, then give focus back.
pub fn reapply_active(instance: &Instance, desktop: &crate::Desktop, state: &SessionState) -> Result<()> {
    let plan = state.plan(desktop);
    if plan.is_empty() {
        return Ok(());
    }
    let focused = desktop.focused_monitor().map(|m| m.name.clone());
    for id in plan {
        dispatch(instance, &format!("hl.dsp.focus({{ workspace = {} }})", lua_string(&id.to_string())))?;
    }
    if let Some(name) = focused {
        dispatch(instance, &format!("hl.dsp.focus({{ monitor = {} }})", lua_string(&name)))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn partition_matches_hyprland() {
        let spans = partition(7680, 1.25, &[1.0, 2.0, 1.0], 640.0).unwrap();
        assert_eq!(spans, vec![Span { x: 0, width: 1920 }, Span { x: 1920, width: 3840 }, Span { x: 5760, width: 1920 }]);
        let spans = partition(3440, 1.0, &[1.0, 1.0, 1.0], 640.0).unwrap();
        assert_eq!(spans.iter().map(|s| s.width).collect::<Vec<_>>(), vec![1147, 1146, 1147]);
        assert_eq!(spans.last().map(|s| s.x + s.width), Some(3440));
        let spans = partition(7680, 1.5, &[20.0, 55.0, 25.0], 640.0).unwrap();
        assert!(spans.iter().all(|s| s.x % 3 == 0));
        assert!(partition(1000, 1.0, &[1.0, 1.0], 640.0).is_err(), "regions narrower than 640 are refused");
        assert!(partition(3840, 1.0, &[1.0, -1.0], 640.0).is_err());
        assert!(partition(3840, 1.0, &[1.0; 4], 640.0).is_err());
        assert_eq!(logical_pixel_step(1.6), 8);
    }

    #[test]
    fn layouts() {
        assert_eq!(Layout::center_focused().proportions().unwrap(), vec![1.0, 2.0, 1.0]);
        assert_eq!(Layout::equal(3).regions(), 3);
        assert_eq!(Layout::full().regions(), 1);
        assert!(Layout::custom(vec![]).proportions().is_err());
        assert!(validate(&Layout::equal(3), 7680, 1.0).is_ok());
    }

    fn ultrawide() -> crate::Monitor {
        crate::Monitor {
            id: 1,
            name: "DP-3".into(),
            description: "Samsung Odyssey G95NC H1AK".into(),
            make: "Samsung".into(),
            model: "Odyssey G95NC".into(),
            serial: "H1AK".into(),
            width: 7680,
            height: 2160,
            x: 0,
            y: 0,
            scale: 1.25,
            transform: 0,
            refresh_rate: 120.0,
            focused: true,
            disabled: false,
            active_workspace: 1,
            special_workspace: None,
            reserved: [0; 4],
        }
    }

    #[test]
    fn renders_guarded_lua() {
        let mut s = Settings { enabled: true, workspace_target: Target::Pointer, ..Settings::default() };
        let d = s.display_mut(&ultrawide());
        d.layout = Layout::custom(vec![20.0, 55.0, 25.0]);
        let lua = s.lua();
        assert!(lua.contains("if not hl.virtual_monitor then return end"), "loads on a Hyprland without virtual monitors");
        assert!(lua.contains("experimental = { virtual_monitors = true }"));
        assert!(lua.contains("workspace_target = \"pointer\""));
        assert!(lua.contains(
            r#"hl.virtual_monitor({ output = "desc:Samsung Odyssey G95NC H1AK", enabled = true, regions = { 20, 55, 25 }, layout = "custom", workspaces = 10, profile = "" })"#
        ));

        s.displays[0].layout = Layout::full();
        assert!(s.lua().contains("enabled = false, regions = { 1 }"), "a single region is no split");
        assert!(display_call(&s.displays[0], Some(15)).ends_with(", confirm = 15 })"));
    }

    #[test]
    fn layers_and_round_trip() {
        let dir = crate::hypr::tests::tempdir("virtual-monitors");
        let paths = Paths {
            layers: vec![dir.join("defaults.toml"), dir.join("overrides.toml")],
            user: dir.join("user/virtual-monitors.toml"),
            lua: dir.join("hypr/nyx-virtual-monitors.lua"),
        };
        std::fs::write(&paths.layers[0], "enabled = true\nconfirm_seconds = 20\n").unwrap();
        std::fs::write(&paths.layers[1], "confirm_seconds = 30\n").unwrap();
        let mut s = Settings::load(&paths).unwrap();
        assert!(s.enabled);
        assert_eq!(s.confirm_seconds, 30, "an administrator's override wins over the defaults");
        assert!(s.profiles.contains_key("Center focused"));

        s.display_mut(&ultrawide()).layout = Layout::center_focused();
        s.save(&paths).unwrap();
        let again = Settings::load(&paths).unwrap();
        assert_eq!(again.displays, s.displays);
        assert!(std::fs::read_to_string(&paths.lua).unwrap().contains("center-focused"));

        std::fs::write(&paths.user, "enabled = [").unwrap();
        assert!(Settings::load(&paths).is_err(), "a broken file is reported, not replaced");
    }

    #[test]
    fn display_matching_follows_the_screen() {
        let mut s = Settings::default();
        s.display_mut(&ultrawide());
        let mut moved = ultrawide();
        moved.name = "DP-1".into();
        assert!(s.display(&moved.stable_id(), &moved.name).is_some(), "the same screen on another port");
        let mut no_serial = ultrawide();
        no_serial.serial.clear();
        s.display_mut(&no_serial);
        assert_eq!(s.displays.len(), 2);
        assert!(s.display("Samsung|Odyssey G95NC|", "DP-9").is_none(), "without a serial the port must match too");
    }

    #[test]
    fn reads_status_and_remembers_regions() {
        let json = r#"{"available": true, "killed": false, "pendingConfirm": 12, "groups": [{"output": "desc:Samsung Odyssey G95NC H1AK",
            "host": "DP-3", "hostId": 1, "make": "Samsung", "model": "Odyssey G95NC", "serial": "H1AK", "state": "split", "layout": "equal",
            "profile": "", "workspaces": 10, "regions": [{"index": 1, "name": "DP-3.v1", "id": 3, "x": 0, "width": 3840, "ratio": 0.5,
            "workspaceBase": 0, "activeWorkspace": 2}, {"index": 2, "name": "DP-3.v2", "id": 4, "x": 3840, "width": 3840, "ratio": 0.5,
            "workspaceBase": 200, "activeWorkspace": 204}]}], "configs": []}"#;
        let status: Status = serde_json::from_str(json).unwrap();
        assert_eq!(status.pending_confirm, Some(12));
        assert_eq!(status.groups[0].stable_id(), "Samsung|Odyssey G95NC|H1AK");
        assert_eq!(status.groups[0].regions[1].workspace_base, 200);
        assert!(!status.groups[0].merged());

        let mut d = crate::Desktop::default();
        d.hosts.push(crate::VirtualHost {
            name: "DP-3".into(),
            stable_id: "S|O|H".into(),
            regions: vec!["DP-3.v1".into(), "DP-3.v2".into()],
            ..Default::default()
        });
        let mut left = ultrawide();
        left.name = "DP-3.v1".into();
        left.active_workspace = 2;
        let mut right = ultrawide();
        right.name = "DP-3.v2".into();
        right.active_workspace = 204;
        d.monitors = vec![left, right];
        d.regions.insert(
            "DP-3.v2".into(),
            crate::VirtualRegion { host: "DP-3".into(), index: 2, count: 2, workspace_base: 200, ..Default::default() },
        );
        d.workspaces = vec![
            crate::Workspace {
                id: 2,
                name: "2".into(),
                monitor: "DP-3.v1".into(),
                windows: 0,
                has_fullscreen: false,
                tiled_layout: String::new(),
            },
            crate::Workspace {
                id: 204,
                name: "4".into(),
                monitor: "DP-3.v2".into(),
                windows: 0,
                has_fullscreen: false,
                tiled_layout: String::new(),
            },
        ];
        let mut state = SessionState::default();
        assert!(state.remember(&d));
        assert_eq!(state.active["S|O|H"], vec![2, 4]);
        assert!(!state.remember(&d), "nothing new");
        assert!(state.plan(&d).is_empty(), "already showing what was remembered");
        state.active.insert("S|O|H".into(), vec![2, 7]);
        assert_eq!(state.plan(&d), vec![207]);
    }
}
