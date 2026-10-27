//! Pure merge. The same operations always produce the same records, in clock
//! order rather than arrival order.

use crate::hlc::Hlc;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OpKind {
    Upsert,
    Tombstone,
    SetAdd,
    SetRemove,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Op {
    pub hlc: Hlc,
    pub collection: String,
    pub record_id: String,
    pub kind: OpKind,
    /// Field values for [`OpKind::Upsert`]. Ignored for other kinds.
    #[serde(default)]
    pub fields: BTreeMap<String, String>,
    /// Set member for add and remove.
    #[serde(default)]
    pub member: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct Record {
    pub fields: BTreeMap<String, String>,
    field_clocks: BTreeMap<String, Hlc>,
    pub deleted: bool,
    deleted_at: Option<Hlc>,
    /// Members present after add/remove operations.
    pub members: BTreeMap<String, Hlc>,
}

pub type State = BTreeMap<(String, String), Record>;

fn key(op: &Op) -> (String, String) {
    (op.collection.clone(), op.record_id.clone())
}

/// Fold `ops` into `base`. Later clocks replace a field. A tombstone hides the
/// record until a newer upsert. Set members are added and removed one at a time.
pub fn merge(base: &State, ops: &[Op]) -> State {
    let mut state = base.clone();
    let mut ordered: Vec<&Op> = ops.iter().collect();
    ordered.sort_by(|a, b| a.hlc.cmp(&b.hlc).then(a.record_id.cmp(&b.record_id)).then(a.collection.cmp(&b.collection)));
    for op in ordered {
        let record = state.entry(key(op)).or_default();
        match op.kind {
            OpKind::Tombstone => {
                let newer = record.deleted_at.as_ref().is_none_or(|at| op.hlc >= *at);
                if newer {
                    record.deleted = true;
                    record.deleted_at = Some(op.hlc.clone());
                }
            }
            OpKind::Upsert => {
                let hidden = record.deleted && record.deleted_at.as_ref().is_some_and(|at| op.hlc < *at);
                if hidden {
                    continue;
                }
                if record.deleted && record.deleted_at.as_ref().is_some_and(|at| op.hlc >= *at) {
                    record.deleted = false;
                }
                for (name, value) in &op.fields {
                    let replace = record.field_clocks.get(name).is_none_or(|at| op.hlc >= *at);
                    if replace {
                        record.fields.insert(name.clone(), value.clone());
                        record.field_clocks.insert(name.clone(), op.hlc.clone());
                    }
                }
            }
            OpKind::SetAdd => {
                if let Some(member) = &op.member {
                    let replace = record.members.get(member).is_none_or(|at| op.hlc >= *at);
                    if replace {
                        record.members.insert(member.clone(), op.hlc.clone());
                    }
                }
            }
            OpKind::SetRemove => {
                if let Some(member) = &op.member {
                    let replace = record.members.get(member).is_none_or(|at| op.hlc >= *at);
                    if replace {
                        record.members.remove(member);
                    }
                }
            }
        }
    }
    state
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hlc(wall: u64, device: &str) -> Hlc {
        Hlc { wall_ms: wall, counter: 0, device: device.into() }
    }

    fn upsert(wall: u64, device: &str, id: &str, fields: &[(&str, &str)]) -> Op {
        Op {
            hlc: hlc(wall, device),
            collection: "feeds".into(),
            record_id: id.into(),
            kind: OpKind::Upsert,
            fields: fields.iter().map(|(k, v)| ((*k).into(), (*v).into())).collect(),
            member: None,
        }
    }

    #[test]
    fn field_updates_keep_the_later_clock_per_field() {
        let ops = vec![
            upsert(1, "a", "1", &[("title", "Old"), ("url", "https://a.example/feed")]),
            upsert(2, "b", "1", &[("title", "New")]),
        ];
        let state = merge(&State::new(), &ops);
        let record = &state[&("feeds".into(), "1".into())];
        assert_eq!(record.fields["title"], "New");
        assert_eq!(record.fields["url"], "https://a.example/feed");
        assert!(!record.deleted);
    }

    #[test]
    fn arrival_order_does_not_beat_the_clock() {
        let ops = vec![upsert(5, "b", "1", &[("title", "Later")]), upsert(1, "a", "1", &[("title", "Earlier")])];
        let state = merge(&State::new(), &ops);
        assert_eq!(state[&("feeds".into(), "1".into())].fields["title"], "Later");
    }

    #[test]
    fn a_later_upsert_restores_a_tombstone() {
        let ops = vec![
            upsert(1, "a", "1", &[("title", "One")]),
            Op {
                hlc: hlc(2, "a"),
                collection: "feeds".into(),
                record_id: "1".into(),
                kind: OpKind::Tombstone,
                fields: BTreeMap::new(),
                member: None,
            },
            upsert(3, "b", "1", &[("title", "Back")]),
        ];
        let state = merge(&State::new(), &ops);
        let record = &state[&("feeds".into(), "1".into())];
        assert!(!record.deleted);
        assert_eq!(record.fields["title"], "Back");
    }

    #[test]
    fn an_older_upsert_does_not_restore_a_tombstone() {
        let ops = vec![
            Op {
                hlc: hlc(5, "a"),
                collection: "feeds".into(),
                record_id: "1".into(),
                kind: OpKind::Tombstone,
                fields: BTreeMap::new(),
                member: None,
            },
            upsert(4, "b", "1", &[("title", "Stale")]),
        ];
        let state = merge(&State::new(), &ops);
        assert!(state[&("feeds".into(), "1".into())].deleted);
    }

    #[test]
    fn set_members_add_and_remove_independently() {
        let tag = |wall, kind, member: &str| Op {
            hlc: hlc(wall, "a"),
            collection: "article_tags".into(),
            record_id: "article".into(),
            kind,
            fields: BTreeMap::new(),
            member: Some(member.into()),
        };
        let ops = vec![
            tag(1, OpKind::SetAdd, "rust"),
            tag(3, OpKind::SetAdd, "linux"),
            tag(4, OpKind::SetRemove, "rust"),
            tag(2, OpKind::SetRemove, "linux"),
        ];
        let state = merge(&State::new(), &ops);
        let members = &state[&("article_tags".into(), "article".into())].members;
        assert!(!members.contains_key("rust"));
        assert!(members.contains_key("linux"));
    }

    #[test]
    fn the_same_inputs_produce_the_same_state() {
        let ops = vec![upsert(2, "b", "1", &[("title", "B")]), upsert(1, "a", "1", &[("title", "A"), ("url", "https://e")])];
        let left = merge(&State::new(), &ops);
        let mut reversed = ops.clone();
        reversed.reverse();
        assert_eq!(left, merge(&State::new(), &reversed));
    }
}
