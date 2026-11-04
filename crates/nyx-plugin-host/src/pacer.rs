//! Message limits: a rate (excess updates are coalesced, so only the newest of each
//! survives), item counts, drawing sizes and frame rates. Oversized lines are dropped
//! before they're parsed (see [`read_line_limited`]).

use crate::limits::Limits;
use nyx_plugin_protocol::{NodeKind, PluginMsg};
use std::collections::{HashMap, HashSet};
use std::io::BufRead;
use std::time::{Duration, Instant};

type Key = (String, Option<String>);

pub struct Pacer {
    limits: Limits,
    tokens: f64,
    last: Instant,
    pending: Vec<PluginMsg>,
    items: HashSet<Key>,
    frames: HashMap<(String, String), Instant>,
    logs: (Instant, u32),
    pub warnings: Vec<String>,
}

fn key(msg: &PluginMsg) -> Option<(&'static str, Key)> {
    Some(match msg {
        PluginMsg::Item(i) => ("item", (i.id.clone(), i.monitor.clone())),
        PluginMsg::Patch(p) => ("patch", (p.item.clone(), p.monitor.clone())),
        PluginMsg::Menu(m) => ("menu", (m.item.clone(), m.monitor.clone())),
        PluginMsg::Popover(p) => ("popover", (p.item.clone(), p.monitor.clone())),
        PluginMsg::Remove { item, monitor } => ("remove", (item.clone(), monitor.clone())),
        _ => return None,
    })
}

impl Pacer {
    pub fn new(limits: Limits) -> Pacer {
        let tokens = limits.messages_per_sec as f64;
        Pacer {
            limits,
            tokens,
            last: Instant::now(),
            pending: vec![],
            items: HashSet::new(),
            frames: HashMap::new(),
            logs: (Instant::now(), 0),
            warnings: vec![],
        }
    }

    fn refill(&mut self, now: Instant) {
        let rate = self.limits.messages_per_sec.max(1) as f64;
        self.tokens = (self.tokens + now.duration_since(self.last).as_secs_f64() * rate).min(rate);
        self.last = now;
    }

    fn warn(&mut self, w: String) {
        if !self.warnings.contains(&w) {
            self.warnings.push(w);
        }
    }

    /// Checks that don't depend on timing. A message that fails is dropped.
    fn check(&mut self, msg: &PluginMsg) -> Result<(), String> {
        let views: Vec<&nyx_plugin_protocol::Node> = match msg {
            PluginMsg::Item(i) => {
                let k = (i.id.clone(), i.monitor.clone());
                if !self.items.contains(&k) && self.items.iter().filter(|(_, m)| *m == i.monitor).count() >= self.limits.items {
                    return Err(format!("more than {} items; {} wasn't shown", self.limits.items, i.id));
                }
                self.items.insert(k);
                i.view.iter().collect()
            }
            PluginMsg::Popover(p) => p.view.iter().collect(),
            PluginMsg::Remove { item, monitor } => {
                self.items.remove(&(item.clone(), monitor.clone()));
                vec![]
            }
            _ => vec![],
        };
        for v in views {
            let mut drawn = 0;
            v.walk(&mut |n| {
                if let NodeKind::Vector(vec) = &n.kind {
                    drawn += vec.size();
                }
            });
            if drawn > self.limits.vector_elements {
                return Err(format!("a drawing with {drawn} elements (the limit is {})", self.limits.vector_elements));
            }
            if v.count() > 2000 {
                return Err("a view with more than 2000 nodes".into());
            }
        }
        Ok(())
    }

    /// A message from the plugin; returns what can go to the host now.
    pub fn offer(&mut self, msg: PluginMsg, now: Instant) -> Vec<PluginMsg> {
        if let Err(w) = self.check(&msg) {
            self.warn(w);
            return vec![];
        }
        match &msg {
            PluginMsg::FrameReady { item, node, .. } => {
                let min = Duration::from_secs_f64(1.0 / self.limits.fps.max(1) as f64);
                let k = (item.clone(), node.clone());
                if self.frames.get(&k).is_some_and(|t| now.duration_since(*t) < min.mul_f64(0.9)) {
                    return vec![];
                }
                self.frames.insert(k, now);
                return vec![msg];
            }
            PluginMsg::Log { .. } => {
                if now.duration_since(self.logs.0) > Duration::from_secs(1) {
                    self.logs = (now, 0);
                }
                self.logs.1 += 1;
                return if self.logs.1 <= 20 { vec![msg] } else { vec![] };
            }
            PluginMsg::FrameRequest { .. } => return vec![msg],
            _ => {}
        }
        self.refill(now);
        if self.pending.is_empty() && self.tokens >= 1.0 {
            self.tokens -= 1.0;
            return vec![msg];
        }
        self.coalesce(msg);
        self.drain()
    }

    fn coalesce(&mut self, msg: PluginMsg) {
        let Some((kind, k)) = key(&msg) else {
            self.pending.push(msg);
            return;
        };
        let same = |m: &PluginMsg| key(m).is_some_and(|(_, mk)| mk == k);
        match kind {
            "item" | "remove" => {
                self.pending.retain(|m| !(same(m) && matches!(m, PluginMsg::Item(_) | PluginMsg::Patch(_) | PluginMsg::Remove { .. })));
                self.pending.push(msg);
            }
            "patch" => {
                if let (Some(PluginMsg::Patch(last)), PluginMsg::Patch(new)) =
                    (self.pending.iter_mut().rev().find(|m| same(m)), &msg)
                {
                    for n in &new.nodes {
                        match last.nodes.iter_mut().find(|o| o.id == n.id) {
                            Some(o) => o.set.extend(n.set.clone()),
                            None => last.nodes.push(n.clone()),
                        }
                    }
                    return;
                }
                self.pending.push(msg);
            }
            other => {
                self.pending.retain(|m| !(same(m) && key(m).is_some_and(|(mk, _)| mk == other)));
                self.pending.push(msg);
            }
        }
    }

    fn drain(&mut self) -> Vec<PluginMsg> {
        let mut out = vec![];
        while self.tokens >= 1.0 && !self.pending.is_empty() {
            self.tokens -= 1.0;
            out.push(self.pending.remove(0));
        }
        out
    }

    /// Called on a timer: what's become sendable.
    pub fn tick(&mut self, now: Instant) -> Vec<PluginMsg> {
        self.refill(now);
        self.drain()
    }

    /// When to call [`tick`](Self::tick) next.
    pub fn next_wake(&self) -> Option<Duration> {
        (!self.pending.is_empty()).then(|| Duration::from_secs_f64(1.0 / self.limits.messages_per_sec.max(1) as f64))
    }
}

pub enum Line {
    Text(String),
    TooLong(usize),
    Eof,
}

/// Read one line of at most `max` bytes; longer lines are skipped (and reported).
pub fn read_line_limited(r: &mut impl BufRead, max: usize) -> std::io::Result<Line> {
    let mut buf = Vec::new();
    let n = std::io::Read::take(&mut *r, max as u64 + 1).read_until(b'\n', &mut buf)?;
    if n == 0 {
        return Ok(Line::Eof);
    }
    if buf.last() != Some(&b'\n') && buf.len() > max {
        // Skip the rest of it.
        let mut skipped = buf.len();
        loop {
            let chunk = r.fill_buf()?;
            if chunk.is_empty() {
                break;
            }
            match chunk.iter().position(|b| *b == b'\n') {
                Some(i) => {
                    r.consume(i + 1);
                    skipped += i;
                    break;
                }
                None => {
                    let l = chunk.len();
                    r.consume(l);
                    skipped += l;
                }
            }
        }
        return Ok(Line::TooLong(skipped));
    }
    Ok(Line::Text(String::from_utf8_lossy(&buf).into_owned()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use nyx_plugin_protocol::{Item, NodePatch, Patch};

    fn patch(item: &str, id: &str, text: &str) -> PluginMsg {
        let mut set = serde_json::Map::new();
        set.insert("text".into(), text.into());
        PluginMsg::Patch(Patch { item: item.into(), monitor: None, nodes: vec![NodePatch { id: id.into(), set }] })
    }

    #[test]
    fn floods_are_coalesced_to_the_newest() {
        let mut l = Limits::third_party();
        l.messages_per_sec = 2;
        let mut p = Pacer::new(l);
        let t = Instant::now();
        let mut out = vec![];
        for i in 0..100 {
            out.extend(p.offer(patch("cpu", "pct", &format!("{i}%")), t));
        }
        assert_eq!(out.len(), 2);
        let later = p.tick(t + Duration::from_secs(1));
        assert_eq!(later.len(), 1);
        let PluginMsg::Patch(last) = &later[0] else { panic!() };
        assert_eq!(last.nodes[0].set["text"], "99%");
        assert!(p.next_wake().is_none());
    }

    #[test]
    fn items_and_drawings_are_limited() {
        let mut l = Limits::third_party();
        l.items = 2;
        let mut p = Pacer::new(l);
        let t = Instant::now();
        assert_eq!(p.offer(PluginMsg::Item(Item::new("a")), t).len(), 1);
        assert_eq!(p.offer(PluginMsg::Item(Item::new("b")), t).len(), 1);
        assert!(p.offer(PluginMsg::Item(Item::new("c")), t).is_empty());
        assert!(p.warnings[0].contains("more than 2 items"));
        let svg = format!("<svg>{}</svg>", "<rect/>".repeat(6000));
        let mut item = Item::new("a");
        item.view = Some(nyx_plugin_protocol::Node::new(NodeKind::Vector(nyx_plugin_protocol::Vector {
            svg: Some(svg),
            ..Default::default()
        })));
        assert!(p.offer(PluginMsg::Item(item), t).is_empty());
    }

    #[test]
    fn frames_are_capped() {
        let mut p = Pacer::new(Limits::third_party());
        let t = Instant::now();
        let f = || PluginMsg::FrameReady { item: "v".into(), node: "f".into(), buffer: 0 };
        let shown: usize = (0..60).map(|i| p.offer(f(), t + Duration::from_millis(i * 10)).len()).sum();
        assert!(shown <= 20, "{shown}");
    }

    #[test]
    fn long_lines_are_skipped() {
        let data = format!("{}\nshort\n", "x".repeat(100));
        let mut r = std::io::BufReader::with_capacity(16, data.as_bytes());
        assert!(matches!(read_line_limited(&mut r, 50).unwrap(), Line::TooLong(_)));
        assert!(matches!(read_line_limited(&mut r, 50).unwrap(), Line::Text(s) if s == "short\n"));
        assert!(matches!(read_line_limited(&mut r, 50).unwrap(), Line::Eof));
    }
}
