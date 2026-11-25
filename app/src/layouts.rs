//! Named, saved workspace layouts: the dock tree, each pane's kind and settings, link
//! groups and popped-out windows. Stored one TOML file per layout in
//! `<config dir>/layouts/` (`~/Library/Application Support/terminal/layouts` on macOS),
//! so they can be shared, hand-edited, imported and exported.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use tr_chart::drawings::Drawing;
use tr_chart::scene::ChartType;
use tr_core::Timeframe;

use crate::command::IndicatorSpec;
use crate::congress_view::FeedFilter;
use crate::dock::{Node, PaneId};
use crate::layout::Layout;
use crate::storage;

pub const FORMAT_VERSION: u32 = 1;

/// Colour link groups: panes in the same group follow one security.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Link {
    Red,
    Orange,
    Green,
    Blue,
    Purple,
}

impl Link {
    pub const ALL: [Link; 5] = [Link::Red, Link::Orange, Link::Green, Link::Blue, Link::Purple];

    pub fn label(self) -> &'static str {
        match self {
            Link::Red => "Red",
            Link::Orange => "Orange",
            Link::Green => "Green",
            Link::Blue => "Blue",
            Link::Purple => "Purple",
        }
    }

    pub fn color(self) -> egui::Color32 {
        match self {
            Link::Red => egui::Color32::from_rgb(255, 69, 58),
            Link::Orange => egui::Color32::from_rgb(255, 159, 10),
            Link::Green => egui::Color32::from_rgb(48, 209, 88),
            Link::Blue => egui::Color32::from_rgb(10, 132, 255),
            Link::Purple => egui::Color32::from_rgb(191, 90, 242),
        }
    }
}

/// A pane's kind plus its persisted, panel-specific settings.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum PaneSettings {
    Chart {
        symbol: String,
        tf: Timeframe,
        chart_type: ChartType,
        log: bool,
        indicators: Vec<IndicatorSpec>,
        drawings: Vec<Drawing>,
        compare: Option<String>,
        #[serde(default = "yes")]
        show_volume: bool,
        #[serde(default = "yes")]
        show_congress: bool,
        #[serde(default)]
        magnet: bool,
    },
    /// Congress feed with its filter (`None` = unfiltered).
    Feed {
        filter: Option<FeedSettings>,
    },
    Ticker(String),
    Profile(String),
    Politicians,
    Alerts,
    Watchlist,
    Help,
    Wei,
    Curve,
    Search(String),
    /// Headlines for a security (`None` = general market news).
    News {
        symbol: Option<String>,
    },
    OrderBook {
        symbol: String,
        depth: usize,
        /// Price bucket size; 0 = raw levels.
        #[serde(default)]
        group: f64,
    },
    Settings,
    /// Option chain monitor.
    Options {
        symbol: String,
        /// Strikes shown each side of the money (0 = all).
        #[serde(default)]
        strikes: usize,
        #[serde(default = "yes")]
        greeks: bool,
    },
    /// Brokerage portfolio view (credentials are never stored in layouts).
    Portfolio {
        #[serde(default)]
        view: crate::command::PortView,
        #[serde(default)]
        account: Option<String>,
        #[serde(default)]
        symbol: Option<String>,
        #[serde(default)]
        sort: crate::app::PosSort,
        #[serde(default)]
        open_only: bool,
    },
}

fn yes() -> bool {
    true
}

/// Serializable mirror of the Congress feed filter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct FeedSettings {
    pub party: Option<tr_congress::Party>,
    pub chamber: Option<tr_congress::Chamber>,
    pub buys: Option<bool>,
    pub tickers_only: bool,
}

impl From<FeedFilter> for FeedSettings {
    fn from(f: FeedFilter) -> Self {
        Self { party: f.party, chamber: f.chamber, buys: f.buys, tickers_only: f.tickers_only }
    }
}

impl From<FeedSettings> for FeedFilter {
    fn from(f: FeedSettings) -> Self {
        Self { party: f.party, chamber: f.chamber, buys: f.buys, tickers_only: f.tickers_only }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SavedPane {
    pub id: PaneId,
    pub settings: PaneSettings,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub link: Option<Link>,
    /// User-chosen tab title.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Popped out into its own window (with its last outer position and size).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window: Option<WindowGeometry>,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct WindowGeometry {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LayoutDoc {
    #[serde(default = "version")]
    pub version: u32,
    pub name: String,
    pub dock: Node,
    pub panes: Vec<SavedPane>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub focus: Option<PaneId>,
    /// Which side the sidebar was on (restored with the layout).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sidebar_side: Option<crate::prefs::SidebarSide>,
    /// Theme to switch to with this layout (only when the layout carries its theme).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub theme: Option<String>,
}

fn version() -> u32 {
    FORMAT_VERSION
}

impl LayoutDoc {
    /// Make a loaded document self-consistent: unique pane ids, a dock that references
    /// only known docked panes, every docked pane placed, and at least one pane.
    pub fn repair(&mut self) {
        let mut seen = HashSet::new();
        self.panes.retain(|p| seen.insert(p.id));
        let docked: HashSet<PaneId> = self.panes.iter().filter(|p| p.window.is_none()).map(|p| p.id).collect();
        self.dock.retain_known(&docked);
        let placed: HashSet<PaneId> = self.dock.panes().into_iter().collect();
        for id in self.panes.iter().filter(|p| p.window.is_none() && !placed.contains(&p.id)).map(|p| p.id) {
            if self.dock.is_empty() {
                self.dock = Node::leaf(id);
            } else if let Some(first) = self.dock.panes().first().copied() {
                self.dock.insert(first, crate::dock::Zone::Center, id);
            }
        }
        if self.panes.is_empty() {
            self.panes.push(SavedPane {
                id: 1,
                settings: PaneSettings::Watchlist,
                link: None,
                title: None,
                window: None,
            });
            self.dock = Node::leaf(1);
        }
        if self.focus.is_some_and(|f| !seen.contains(&f)) {
            self.focus = None;
        }
        self.version = FORMAT_VERSION;
    }

    pub fn from_toml(bytes: &[u8]) -> Result<LayoutDoc> {
        let mut doc: LayoutDoc = toml::from_str(std::str::from_utf8(bytes)?).context("parsing layout TOML")?;
        if doc.version > FORMAT_VERSION {
            bail!("layout format v{} is newer than this build supports (v{FORMAT_VERSION})", doc.version);
        }
        doc.repair();
        Ok(doc)
    }

    pub fn to_toml(&self) -> Result<String> {
        Ok(toml::to_string_pretty(self)?)
    }

    /// The default workspace: chart, Congress feed, world markets and watchlist.
    pub fn default_workspace(name: &str, defaults: &crate::prefs::PanelDefaults) -> LayoutDoc {
        let panes = vec![
            chart_settings("AAPL US Equity", defaults),
            PaneSettings::Feed { filter: None },
            PaneSettings::Wei,
            PaneSettings::Watchlist,
        ];
        Self::preset(name, Layout::Grid2x2, panes)
    }

    pub fn preset(name: &str, layout: Layout, panes: Vec<PaneSettings>) -> LayoutDoc {
        let panes: Vec<SavedPane> = panes
            .into_iter()
            .enumerate()
            .map(|(i, settings)| SavedPane { id: i as PaneId + 1, settings, link: None, title: None, window: None })
            .collect();
        let ids: Vec<PaneId> = panes.iter().map(|p| p.id).collect();
        LayoutDoc {
            version: FORMAT_VERSION,
            name: name.into(),
            dock: Node::preset(layout, &ids),
            panes,
            focus: None,
            sidebar_side: None,
            theme: None,
        }
    }
}

pub fn chart_settings(symbol: &str, d: &crate::prefs::PanelDefaults) -> PaneSettings {
    PaneSettings::Chart {
        symbol: symbol.into(),
        tf: d.timeframe,
        chart_type: d.chart_type,
        log: false,
        indicators: crate::command::default_indicators(),
        drawings: Vec::new(),
        compare: None,
        show_volume: d.show_volume,
        show_congress: d.congress_markers,
        magnet: false,
    }
}

/// The layouts folder and its files.
pub struct Store {
    pub dir: PathBuf,
}

impl Store {
    pub fn new(dir: PathBuf) -> Self {
        Self { dir }
    }

    /// A safe file stem for a layout name (keeps letters, digits, space, `-`, `_`, `.`).
    pub fn file_stem(name: &str) -> String {
        let s: String = name
            .trim()
            .chars()
            .map(|c| if c.is_alphanumeric() || matches!(c, ' ' | '-' | '_' | '.') { c } else { '_' })
            .collect();
        let s = s.trim_matches(|c| c == '.' || c == ' ').to_string();
        if s.is_empty() { "Untitled".into() } else { s }
    }

    pub fn path(&self, name: &str) -> PathBuf {
        self.dir.join(format!("{}.toml", Self::file_stem(name)))
    }

    /// Saved layout names, sorted case-insensitively. Names come from the documents, so a
    /// file copied in under any file name keeps its display name.
    pub fn list(&self) -> Vec<String> {
        let mut names: Vec<String> = storage::list(&self.dir)
            .into_iter()
            .filter(|p| p.extension().is_some_and(|x| x == "toml"))
            .filter_map(|p| {
                let bytes = storage::read(&p).ok()?;
                let doc = LayoutDoc::from_toml(&bytes).ok()?;
                Some(if doc.name.is_empty() {
                    p.file_stem().unwrap_or_default().to_string_lossy().into_owned()
                } else {
                    doc.name
                })
            })
            .collect();
        names.sort_by_key(|n| n.to_lowercase());
        names.dedup();
        names
    }

    pub fn exists(&self, name: &str) -> bool {
        self.find_file(name).is_some()
    }

    /// The file holding layout `name`: the canonical path, else any file whose document has
    /// that name (e.g. dropped into the folder by hand).
    fn find_file(&self, name: &str) -> Option<PathBuf> {
        let p = self.path(name);
        if storage::exists(&p) {
            return Some(p);
        }
        storage::list(&self.dir).into_iter().find(|p| {
            p.extension().is_some_and(|x| x == "toml")
                && storage::read(p)
                    .ok()
                    .and_then(|b| LayoutDoc::from_toml(&b).ok())
                    .is_some_and(|v| v.name.eq_ignore_ascii_case(name))
        })
    }

    pub fn load(&self, name: &str) -> Result<LayoutDoc> {
        let path = self.find_file(name).with_context(|| format!("no saved layout named {name:?}"))?;
        let mut doc = LayoutDoc::from_toml(&storage::read(&path)?).with_context(|| format!("{}", path.display()))?;
        if doc.name.trim().is_empty() {
            doc.name = name.to_string();
        }
        Ok(doc)
    }

    pub fn save(&self, doc: &LayoutDoc) -> Result<PathBuf> {
        let path = self.path(&doc.name);
        storage::write_atomic(&path, doc.to_toml()?.as_bytes())
            .with_context(|| format!("writing {}", path.display()))?;
        Ok(path)
    }

    pub fn delete(&self, name: &str) -> Result<()> {
        let path = self.find_file(name).with_context(|| format!("no saved layout named {name:?}"))?;
        storage::remove(&path)?;
        Ok(())
    }

    pub fn rename(&self, old: &str, new: &str) -> Result<LayoutDoc> {
        if new.trim().is_empty() {
            bail!("layout name cannot be empty");
        }
        if !old.eq_ignore_ascii_case(new) && self.exists(new) {
            bail!("a layout named {new:?} already exists");
        }
        let mut doc = self.load(old)?;
        self.delete(old)?;
        doc.name = new.trim().to_string();
        self.save(&doc)?;
        Ok(doc)
    }

    pub fn duplicate(&self, name: &str, new: Option<&str>) -> Result<LayoutDoc> {
        let mut doc = self.load(name)?;
        doc.name = match new {
            Some(n) if !n.trim().is_empty() => n.trim().to_string(),
            _ => self.unique_name(&format!("{name} copy")),
        };
        if self.exists(&doc.name) {
            bail!("a layout named {:?} already exists", doc.name);
        }
        self.save(&doc)?;
        Ok(doc)
    }

    /// `base`, or `base 2`, `base 3`… if taken.
    pub fn unique_name(&self, base: &str) -> String {
        if !self.exists(base) {
            return base.to_string();
        }
        (2..).map(|i| format!("{base} {i}")).find(|n| !self.exists(n)).expect("unbounded")
    }

    /// Write a layout to an arbitrary path (a download in the browser). Returns where it went.
    pub fn export(&self, name: &str, to: &Path) -> Result<String> {
        let doc = self.load(name)?;
        let to = to.with_extension("toml");
        storage::export(&to, doc.to_toml()?.as_bytes()).with_context(|| format!("writing {}", to.display()))
    }

    /// Copy a layout file into the store (renaming on conflict). Returns its name.
    pub fn import(&self, from: &Path) -> Result<String> {
        let mut doc =
            LayoutDoc::from_toml(&storage::import(from).with_context(|| format!("reading {}", from.display()))?)?;
        if doc.name.trim().is_empty() {
            doc.name = from.file_stem().unwrap_or_default().to_string_lossy().into_owned();
        }
        doc.name = self.unique_name(doc.name.trim());
        self.save(&doc)?;
        Ok(doc.name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dock::Zone;
    use crate::prefs::PanelDefaults;

    fn temp_store(tag: &str) -> Store {
        let dir = std::env::temp_dir().join(format!("tr-layouts-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        Store::new(dir)
    }

    fn rich_doc() -> LayoutDoc {
        let mut doc = LayoutDoc::default_workspace("Trading", &PanelDefaults::default());
        doc.panes.push(SavedPane {
            id: 5,
            settings: PaneSettings::OrderBook { symbol: "BTCUSD Curncy".into(), depth: 25, group: 10.0 },
            link: Some(Link::Blue),
            title: Some("BTC depth".into()),
            window: None,
        });
        doc.panes.push(SavedPane {
            id: 6,
            settings: PaneSettings::News { symbol: Some("BTCUSD Curncy".into()) },
            link: Some(Link::Blue),
            title: None,
            window: Some(WindowGeometry { x: 40.0, y: 60.0, w: 640.0, h: 480.0 }),
        });
        doc.panes.push(SavedPane {
            id: 7,
            settings: PaneSettings::Options { symbol: "AAPL US Equity".into(), strikes: 10, greeks: false },
            link: Some(Link::Blue),
            title: None,
            window: None,
        });
        doc.dock.insert(1, Zone::Right, 5);
        doc.dock.insert(5, Zone::Center, 7);
        doc.panes[0].link = Some(Link::Blue);
        doc.focus = Some(5);
        doc.sidebar_side = Some(crate::prefs::SidebarSide::Right);
        doc.theme = Some("Nord".into());
        doc
    }

    #[test]
    fn toml_round_trip_preserves_everything() {
        let doc = rich_doc();
        let text = doc.to_toml().unwrap();
        let back = LayoutDoc::from_toml(text.as_bytes()).unwrap();
        assert_eq!(back, doc);
        assert!(!back.dock.contains(6));
        assert!(back.dock.contains(5));
    }

    #[test]
    fn hand_edited_toml_is_repaired_and_newer_versions_are_rejected() {
        let text = br#"
name = "Broken"
focus = 42
[dock]
type = "tabs"
panes = [1, 9]
[[panes]]
id = 1
settings = "Wei"
[[panes]]
id = 1
settings = "Curve"
[[panes]]
id = 2
settings.News = {}
[[panes]]
id = 3
settings = "Settings"
window = { x = 0.0, y = 0.0, w = 300.0, h = 200.0 }
"#;
        let doc = LayoutDoc::from_toml(text).unwrap();
        assert_eq!(doc.panes.len(), 3);
        assert_eq!(doc.dock.panes(), vec![1, 2]);
        assert_eq!(doc.focus, None);
        let empty = "name = \"Empty\"\npanes = []\n[dock]\ntype = \"tabs\"\npanes = []\n";
        assert_eq!(LayoutDoc::from_toml(empty.as_bytes()).unwrap().dock.panes(), vec![1]);
        assert!(LayoutDoc::from_toml(format!("version = 99\n{empty}").as_bytes()).is_err());
        assert!(LayoutDoc::from_toml(br#"{"name":"JSON"}"#).is_err());
    }

    #[test]
    fn store_save_list_rename_duplicate_delete_import_export() {
        let store = temp_store("ops");
        assert!(store.list().is_empty());
        let doc = rich_doc();
        store.save(&doc).unwrap();
        store.save(&LayoutDoc::default_workspace("alpha", &PanelDefaults::default())).unwrap();
        assert_eq!(store.list(), vec!["alpha", "Trading"]);
        assert_eq!(store.load("trading").unwrap(), doc, "lookup is case-insensitive");

        let dup = store.duplicate("Trading", None).unwrap();
        assert_eq!(dup.name, "Trading copy");
        assert_eq!(store.duplicate("Trading", None).unwrap().name, "Trading copy 2");
        assert!(store.duplicate("Trading", Some("alpha")).is_err());

        let renamed = store.rename("Trading copy", "Crypto/Desk").unwrap();
        assert_eq!(renamed.name, "Crypto/Desk");
        assert!(store.path("Crypto/Desk").ends_with("Crypto_Desk.toml"));
        assert!(store.rename("alpha", "Crypto/Desk").is_err());
        assert!(store.rename("alpha", "  ").is_err());

        let out = store.dir.join("export/trading.toml");
        store.export("Trading", &out).unwrap();
        let imported = store.import(&out).unwrap();
        assert_eq!(imported, "Trading 2");
        assert_eq!(store.load("Trading 2").unwrap().panes, doc.panes);

        // A file dropped in by hand under any name is listed by its document name.
        std::fs::write(
            store.dir.join("whatever.toml"),
            LayoutDoc::preset("Hand Made", Layout::One, vec![PaneSettings::Help]).to_toml().unwrap(),
        )
        .unwrap();
        assert!(store.list().contains(&"Hand Made".to_string()));
        assert_eq!(store.load("Hand Made").unwrap().panes.len(), 1);

        store.delete("Trading").unwrap();
        assert!(store.load("Trading").is_err());
        assert!(store.delete("Trading").is_err());
        std::fs::remove_dir_all(&store.dir).unwrap();
    }

    #[test]
    fn file_stems_are_safe() {
        assert_eq!(Store::file_stem("../../etc/passwd"), "_.._etc_passwd");
        assert_eq!(Store::file_stem("  "), "Untitled");
        assert_eq!(Store::file_stem("My Layout 2"), "My Layout 2");
        assert_eq!(Store::file_stem(".hidden"), "hidden");
    }
}
