//! `plugin.toml`. The field names follow Glimpse's plugin manifest (`api_version`,
//! `exec` as an argument array, `needs`, `[permissions]`), so the two can merge later.

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;

pub const FILE: &str = "plugin.toml";

/// The hosts that load plugins.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Kind {
    Bar,
    ControlCenter,
    Desktop,
}

impl Kind {
    pub const ALL: [Kind; 3] = [Kind::Bar, Kind::ControlCenter, Kind::Desktop];

    pub fn name(self) -> &'static str {
        match self {
            Kind::Bar => "bar",
            Kind::ControlCenter => "control-center",
            Kind::Desktop => "desktop",
        }
    }

    /// What people call it.
    pub fn title(self) -> &'static str {
        match self {
            Kind::Bar => "Bar",
            Kind::ControlCenter => "Control Center",
            Kind::Desktop => "Desktop widgets",
        }
    }

    pub fn parse(s: &str) -> Option<Kind> {
        Kind::ALL.into_iter().find(|k| k.name() == s)
    }

    /// The program that hosts this kind, to tell whether it's installed.
    pub fn host_program(self) -> &'static str {
        match self {
            Kind::Bar => "nyx-bar",
            Kind::ControlCenter => "nyx-control-center",
            Kind::Desktop => "nyx-desktop-widgets",
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Manifest {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub version: String,
    #[serde(default)]
    pub author: String,
    #[serde(default)]
    pub license: String,
    #[serde(default)]
    pub description: String,
    pub api_version: u32,
    pub kinds: Vec<String>,
    /// The program and its arguments, for every kind that doesn't set its own. A first
    /// element that names a file in the plugin's folder runs that file.
    #[serde(default)]
    pub exec: Vec<String>,
    /// Commands that must be on PATH; the plugin is skipped without them.
    #[serde(default)]
    pub needs: Vec<String>,
    #[serde(default)]
    pub permissions: Permissions,
    /// Prebuilt programs by platform (`x86_64-linux`).
    #[serde(default)]
    pub artifacts: BTreeMap<String, Artifact>,
    #[serde(default)]
    pub limits: LimitRequest,
    #[serde(default)]
    pub bar: Option<KindSection>,
    #[serde(default, rename = "control-center")]
    pub control_center: Option<KindSection>,
    #[serde(default)]
    pub desktop: Option<KindSection>,
    /// Defaults for the plugin's settings; users override them per host.
    #[serde(default)]
    pub settings: toml::Table,
}

/// What a third-party plugin may do. Trusted plugins (packaged, first-party) aren't
/// sandboxed, so for them this is documentation.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Permissions {
    #[serde(default)]
    pub network: bool,
    /// Extra paths the plugin can read (absolute, or starting with `~/`).
    #[serde(default)]
    pub read: Vec<String>,
    /// D-Bus names on the session bus it may call and hear from.
    #[serde(default)]
    pub session_talk: Vec<String>,
    /// D-Bus names on the system bus.
    #[serde(default)]
    pub system_talk: Vec<String>,
    /// Exact command lines the host may run for it (clicks).
    #[serde(default)]
    pub host_commands: Vec<Vec<String>>,
    /// Hyprland's workspaces and windows, forwarded by the host (read only).
    #[serde(default)]
    pub desktop: bool,
    /// Its own windows (the Wayland socket).
    #[serde(default)]
    pub windows: bool,
    /// Shared-memory frames.
    #[serde(default)]
    pub frames: bool,
}

impl Permissions {
    pub fn is_empty(&self) -> bool {
        *self == Permissions::default()
    }

    /// The names the host passes in `hello`.
    pub fn names(&self) -> Vec<String> {
        let mut out = vec![];
        let flags = [("network", self.network), ("desktop", self.desktop), ("windows", self.windows), ("frames", self.frames)];
        for (name, on) in flags {
            if on {
                out.push(name.to_string());
            }
        }
        if !self.read.is_empty() {
            out.push("read".into());
        }
        if !self.session_talk.is_empty() {
            out.push("session_talk".into());
        }
        if !self.system_talk.is_empty() {
            out.push("system_talk".into());
        }
        if !self.host_commands.is_empty() {
            out.push("host_commands".into());
        }
        out
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Artifact {
    pub url: String,
    pub sha256: String,
}

/// Limits a plugin asks for. Third-party requests are capped at the defaults unless the
/// user raises them.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LimitRequest {
    /// `64M`, `256M`, …
    #[serde(default)]
    pub memory: Option<String>,
    /// Percent of one core.
    #[serde(default)]
    pub cpu: Option<u32>,
    #[serde(default)]
    pub tasks: Option<u32>,
    /// Messages a second.
    #[serde(default)]
    pub messages: Option<u32>,
    #[serde(default)]
    pub fps: Option<u32>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    #[default]
    Stream,
    Interval,
}

/// One kind's section: `[bar]`, `[control-center]`, `[desktop]`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct KindSection {
    #[serde(default)]
    pub exec: Option<Vec<String>>,
    #[serde(default)]
    pub mode: Mode,
    /// Seconds between runs, for `interval`.
    #[serde(default)]
    pub interval: Option<u64>,
    /// Where its parts go when the layout doesn't place them (`left`, `center`, `right`).
    #[serde(default)]
    pub section: Option<String>,
    /// One copy of each item per monitor.
    #[serde(default)]
    pub per_monitor: bool,
    /// Keep running while the host is hidden (trusted plugins only).
    #[serde(default)]
    pub keep_alive: bool,
    #[serde(default)]
    pub items: Vec<Part>,
    /// Keys a host reads itself (tile sizes, swatches, …).
    #[serde(flatten)]
    pub extra: toml::Table,
}

/// A part the user can turn on or off.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Part {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub default: bool,
    #[serde(flatten)]
    pub extra: toml::Table,
}

/// The part a plugin without `items` has: everything it shows.
pub const WHOLE: &str = "main";

/// Desktop widget sizes, in logical pixels.
pub const DESKTOP_SIZES: [(&str, i32, i32); 3] = [("compact", 160, 160), ("medium", 336, 160), ("large", 336, 336)];

pub fn desktop_size(name: &str) -> Option<(i32, i32)> {
    DESKTOP_SIZES.iter().find(|(n, _, _)| *n == name).map(|&(_, w, h)| (w, h))
}

impl Part {
    /// A desktop part's sizes, the first being its default.
    pub fn sizes(&self) -> Vec<String> {
        let sizes: Vec<String> = self
            .extra
            .get("sizes")
            .and_then(|s| s.as_array())
            .map(|a| a.iter().filter_map(|s| s.as_str().map(String::from)).collect())
            .unwrap_or_default();
        if sizes.is_empty() { vec!["medium".into()] } else { sizes }
    }

    /// A desktop part that takes clicks; others let them through to the desktop.
    pub fn interactive(&self) -> bool {
        self.extra.get("interactive").and_then(|i| i.as_bool()).unwrap_or(false)
    }

    /// A desktop part that can also be any size between `min` and `max` (`size = "free"`).
    pub fn resizable(&self) -> bool {
        self.extra.get("resizable").and_then(|i| i.as_bool()).unwrap_or(false)
    }

    fn pair(&self, key: &str) -> Option<(i32, i32)> {
        let a = self.extra.get(key)?.as_array()?;
        Some((a.first()?.as_integer()? as i32, a.get(1)?.as_integer()? as i32))
    }

    /// The smallest and largest free size, in logical pixels.
    pub fn free_range(&self) -> ((i32, i32), (i32, i32)) {
        let min = self.pair("min").unwrap_or((160, 120));
        let max = self.pair("max").unwrap_or((672, 504));
        let min = (min.0.clamp(64, 1200), min.1.clamp(64, 1200));
        (min, (max.0.clamp(min.0, 1200), max.1.clamp(min.1, 1200)))
    }
}

pub fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && !id.contains("..")
        && id.chars().next().is_some_and(|c| c.is_ascii_alphanumeric())
        && id.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

impl Manifest {
    pub fn load(dir: &Path) -> Result<Manifest> {
        let path = dir.join(FILE);
        let text = std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
        let m: Manifest = toml::from_str(&text).with_context(|| format!("in {}", path.display()))?;
        m.validate()?;
        Ok(m)
    }

    pub fn validate(&self) -> Result<()> {
        if !valid_id(&self.id) {
            bail!("plugin id {:?}: use letters, digits, dots, dashes and underscores", self.id);
        }
        if self.name.trim().is_empty() {
            bail!("{}: name is empty", self.id);
        }
        if self.api_version != 1 {
            bail!("{}: api_version {} isn't supported (this host speaks 1)", self.id, self.api_version);
        }
        if self.kinds.is_empty() {
            bail!("{}: kinds is empty", self.id);
        }
        for kind in self.known_kinds() {
            let section = self.section(kind);
            if self.exec_for(kind).is_empty() {
                bail!("{}: no exec for {}", self.id, kind.name());
            }
            if section.mode == Mode::Interval && section.interval.unwrap_or(0) == 0 {
                bail!("{}: {} is mode = \"interval\" but has no interval", self.id, kind.name());
            }
            let mut seen = std::collections::BTreeSet::new();
            for part in &section.items {
                if !valid_id(&part.id) || part.id.contains('/') {
                    bail!("{}: part id {:?} in {}", self.id, part.id, kind.name());
                }
                if !seen.insert(part.id.clone()) {
                    bail!("{}: part {} appears twice in {}", self.id, part.id, kind.name());
                }
            }
            if kind == Kind::Desktop {
                for part in &section.items {
                    if let Some(bad) = part.sizes().iter().find(|s| desktop_size(s).is_none() && !(*s == "free" && part.resizable())) {
                        bail!("{}: part {} has size {bad:?} (use compact, medium or large; free needs resizable)", self.id, part.id);
                    }
                }
            }
            if let Some(s) = &section.section
                && !matches!(s.as_str(), "left" | "center" | "right")
            {
                bail!("{}: section {s:?} (use left, center or right)", self.id);
            }
        }
        if self.permissions.host_commands.iter().any(|c| c.is_empty()) {
            bail!("{}: an empty host command", self.id);
        }
        for p in &self.permissions.read {
            if !(p.starts_with('/') || p.starts_with("~/")) || p.contains("..") {
                bail!("{}: read path {p:?} must be absolute or start with ~/", self.id);
            }
        }
        for (platform, a) in &self.artifacts {
            if a.sha256.len() != 64 || !a.sha256.chars().all(|c| c.is_ascii_hexdigit()) {
                bail!("{}: artifact {platform} has no valid sha256", self.id);
            }
            if !a.url.starts_with("https://") {
                bail!("{}: artifact {platform} must be an https:// URL", self.id);
            }
        }
        Ok(())
    }

    /// The kinds this build of the hosts knows, in the manifest's order.
    pub fn known_kinds(&self) -> Vec<Kind> {
        self.kinds.iter().filter_map(|k| Kind::parse(k)).collect()
    }

    pub fn has_kind(&self, kind: Kind) -> bool {
        self.kinds.iter().any(|k| k == kind.name())
    }

    /// The kind's section, or an empty one.
    pub fn section(&self, kind: Kind) -> KindSection {
        match kind {
            Kind::Bar => self.bar.clone(),
            Kind::ControlCenter => self.control_center.clone(),
            Kind::Desktop => self.desktop.clone(),
        }
        .unwrap_or_default()
    }

    pub fn exec_for(&self, kind: Kind) -> Vec<String> {
        self.section(kind).exec.filter(|e| !e.is_empty()).unwrap_or_else(|| self.exec.clone())
    }

    /// The parts of a kind; a plugin without `items` has one, [`WHOLE`].
    pub fn parts(&self, kind: Kind) -> Vec<Part> {
        let s = self.section(kind);
        if s.items.is_empty() {
            vec![Part { id: WHOLE.into(), name: self.name.clone(), description: self.description.clone(), default: true, extra: Default::default() }]
        } else {
            s.items
        }
    }

    pub fn default_parts(&self, kind: Kind) -> Vec<String> {
        self.parts(kind).into_iter().filter(|p| p.default).map(|p| p.id).collect()
    }

    /// The part an item the plugin sends belongs to: the part with its id, or whose id
    /// it starts with followed by `-` (`tray-3` → `tray`), or the whole plugin.
    pub fn part_of(&self, kind: Kind, item: &str) -> Option<String> {
        let parts = self.section(kind).items;
        if parts.is_empty() {
            return Some(WHOLE.into());
        }
        parts
            .iter()
            .find(|p| p.id == item || item.strip_prefix(p.id.as_str()).is_some_and(|rest| rest.starts_with('-')))
            .map(|p| p.id.clone())
    }

    /// This machine's prebuilt program, if the plugin ships them.
    pub fn artifact(&self) -> Option<&Artifact> {
        self.artifacts.get(&platform())
    }
}

pub fn platform() -> String {
    format!("{}-linux", std::env::consts::ARCH)
}

#[cfg(test)]
mod tests {
    use super::*;

    const WEATHER: &str = r#"
id = "nyx.weather"
name = "Weather"
version = "1.0.0"
api_version = 1
kinds = ["bar", "control-center", "desktop", "glimpse"]
exec = ["sh", "weather.sh"]
needs = ["nyx-weather"]

[permissions]
host_commands = [["nyx-weather", "--toggle"]]

[bar]
mode = "interval"
interval = 600
section = "right"
[[bar.items]]
id = "now"
name = "Current conditions"
default = true
[[bar.items]]
id = "forecast"
name = "Tomorrow"

[control-center]
[[control-center.items]]
id = "tile"
name = "Weather tile"
default = true
sizes = ["1x1", "2x1"]

[settings]
units = "metric"
"#;

    #[test]
    fn reads_a_manifest_with_three_kinds() {
        let m: Manifest = toml::from_str(WEATHER).unwrap();
        m.validate().unwrap();
        assert_eq!(m.known_kinds(), vec![Kind::Bar, Kind::ControlCenter, Kind::Desktop]);
        assert_eq!(m.section(Kind::Bar).mode, Mode::Interval);
        assert_eq!(m.default_parts(Kind::Bar), vec!["now"]);
        assert_eq!(m.default_parts(Kind::Desktop), vec![WHOLE]);
        assert_eq!(m.exec_for(Kind::ControlCenter), vec!["sh", "weather.sh"]);
        let tile = &m.parts(Kind::ControlCenter)[0];
        assert!(tile.extra.contains_key("sizes"));
    }

    #[test]
    fn items_belong_to_parts() {
        let m: Manifest = toml::from_str(WEATHER).unwrap();
        assert_eq!(m.part_of(Kind::Bar, "now").as_deref(), Some("now"));
        assert_eq!(m.part_of(Kind::Bar, "forecast-2").as_deref(), Some("forecast"));
        assert_eq!(m.part_of(Kind::Bar, "nowhere"), None);
        assert_eq!(m.part_of(Kind::Desktop, "anything").as_deref(), Some(WHOLE));
    }

    #[test]
    fn refuses_bad_manifests() {
        let bad = |edit: &str, with: &str| {
            let text = WEATHER.replace(edit, with);
            toml::from_str::<Manifest>(&text).map_err(anyhow::Error::from).and_then(|m| m.validate()).is_err()
        };
        assert!(bad("id = \"nyx.weather\"", "id = \"../x\""));
        assert!(bad("api_version = 1", "api_version = 2"));
        assert!(bad("interval = 600", ""));
        assert!(bad("section = \"right\"", "section = \"middle\""));
        assert!(bad("[permissions]", "[permissions]\nread = [\"relative\"]"));
        assert!(bad("[permissions]", "[permissions]\nroot = true"));
        assert!(bad("[settings]", "[[desktop.items]]\nid = \"card\"\nname = \"Card\"\nsizes = [\"huge\"]\n[settings]"));
    }

    #[test]
    fn desktop_parts() {
        let text = WEATHER.replace(
            "[settings]",
            "[[desktop.items]]\nid = \"card\"\nname = \"Card\"\nsizes = [\"large\", \"medium\"]\ninteractive = true\n[[desktop.items]]\nid = \"small\"\nname = \"Small\"\n[settings]",
        );
        let m: Manifest = toml::from_str(&text).unwrap();
        m.validate().unwrap();
        let parts = m.parts(Kind::Desktop);
        assert_eq!(parts[0].sizes(), vec!["large", "medium"]);
        assert!(parts[0].interactive());
        assert_eq!(parts[1].sizes(), vec!["medium"]);
        assert!(!parts[1].interactive());
        assert_eq!(desktop_size("large"), Some((336, 336)));
        assert_eq!(parts[0].free_range(), ((160, 120), (672, 504)));
        let free = text.replace("interactive = true", "interactive = true\nresizable = true\nmin = [200, 100]\nmax = [900, 50]");
        let m: Manifest = toml::from_str(&free).unwrap();
        let part = &m.parts(Kind::Desktop)[0];
        assert!(part.resizable());
        assert_eq!(part.free_range(), ((200, 100), (900, 100)), "max is at least min");
        let bad = text.replace("sizes = [\"large\", \"medium\"]", "sizes = [\"free\"]");
        assert!(toml::from_str::<Manifest>(&bad).unwrap().validate().is_err(), "free needs resizable");
    }

    #[test]
    fn ids() {
        assert!(valid_id("acme.ping-2"));
        assert!(!valid_id(".hidden"));
        assert!(!valid_id("a/b"));
        assert!(!valid_id("a..b"));
    }
}
