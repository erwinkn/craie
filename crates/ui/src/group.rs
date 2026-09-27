//! Focus groups (ARCHITECTURE-update topic 3).
//!
//! A group (`Mutation::Group`) is one Tab stop over its members, and
//! arrow keys move among them:
//!
//! ```text
//! radiogroup (vertical, selectOnFocus)
//! ├─ radio "Small"
//! ├─ radio "Medium"   checked
//! └─ radio "Large"    disabled
//! ```
//!
//! Tab reaches Medium alone and the next Tab leaves the group; ↓ from
//! Medium goes to Small (Large is skipped, and the group loops) and
//! activates it.
//!
//! Members are the group's focusable descendants that are enabled and
//! not hidden or inert, none inside another member; a group with a
//! composite role (`radiogroup`, `tablist`) takes only those with its
//! item role. A group nested in another is one member of the outer one,
//! entered at its own stop, and keeps its members to itself: in a
//! vertical group of horizontal toolbars, ↑↓ move between the toolbars
//! and ←→ within one.
//!
//! The stop is the member holding the focus, else the selected one
//! (checked or selected), else the last focused, else the first. The
//! last focused is kept natively, per group, as (id, generation).

use crate::events::{Key, KeyInput, Mods, activate_source};
use crate::host::{NodeFlags, NodeId};
use crate::mutation::{Role, group_flag, press};
use crate::states::state_bit;
use crate::ui::Ui;

/// A focus group's native state.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Group {
    pub flags: u8,
    /// The member focused last, as (id, generation): a leaf, possibly
    /// inside a nested group.
    last: Option<(NodeId, u16)>,
}

/// `walk_group`'s buffers: the members found, and its stack. The Tab
/// walk reuses one across the groups it meets.
#[derive(Default)]
pub(crate) struct GroupWalk {
    members: Vec<(NodeId, NodeId)>,
    stack: Vec<NodeId>,
}

/// The item role a composite group role implies (Marbre web's pairs
/// where Craie has the roles).
fn item_role(role: Role) -> Option<Role> {
    match role {
        Role::RadioGroup => Some(Role::RadioButton),
        Role::TabList => Some(Role::Tab),
        _ => None,
    }
}

impl Ui {
    /// Makes `id` a focus group with `group_flag` bits, keeping its last
    /// focused member; no bits unmake it.
    pub(crate) fn set_group(&mut self, id: NodeId, flags: u8) {
        let old = self.groups.get(&id.0).map_or(0, |g| g.flags);
        if flags == old {
            return;
        }
        if flags == 0 {
            self.groups.remove(&id.0);
        } else {
            self.groups.entry(id.0).or_default().flags = flags;
        }
        self.host.interaction[id.index()].group = flags != 0;
        // The orientation is in the accessibility tree.
        self.host.revs.semantic.bump();
        self.host.dirty.semantic.push(id.0);
    }

    pub(crate) fn is_group(&self, id: NodeId) -> bool {
        self.host.interaction(id).group
    }

    /// Whether `n` would be a leaf member of a group taking `item`
    /// roles: focusable, and of the item role if there is one.
    fn candidate(&self, n: NodeId, item: Option<Role>) -> bool {
        let i = self.host.interaction(n);
        (i.focusable || self.host.kind(n) == Some(crate::mutation::NodeKind::Input))
            && item.is_none_or(|r| i.role == r)
    }

    fn disabled(&self, n: NodeId) -> bool {
        self.host.interaction(n).press & press::DISABLED != 0
            || self.state_bits(n) & state_bit::DISABLED != 0
    }

    /// Group `g`'s members in tree order, each with its leaf: itself, or
    /// a nested group's stop. Hidden and inert subtrees hold none, and a
    /// nested group with no members is not one. A disabled candidate is
    /// no member (what it holds may be), and goes to `disabled`, unless
    /// it holds the focus: then it is one, the stop, until focus leaves.
    /// They land in `walk.members`.
    fn walk_group(&self, g: NodeId, walk: &mut GroupWalk, mut disabled: impl FnMut(NodeId)) {
        let item = item_role(self.host.interaction(g).role);
        let GroupWalk {
            members: out,
            stack,
        } = walk;
        out.clear();
        stack.clear();
        stack.extend(self.host.children(g).iter().rev());
        while let Some(n) = stack.pop() {
            let Some(node) = self.host.node(n) else {
                continue;
            };
            if node.flags.contains(NodeFlags::INERT) || self.host.display_none(n) {
                continue;
            }
            if self.is_group(n) {
                if let Some(leaf) = self.group_stop(n).map(|(_, leaf)| leaf) {
                    out.push((n, leaf));
                }
                continue;
            }
            if self.candidate(n, item) {
                if !self.disabled(n) {
                    out.push((n, n));
                    continue;
                }
                if self.focus == Some(n) {
                    out.push((n, n));
                } else {
                    disabled(n);
                }
            }
            stack.extend(self.host.children(n).iter().rev());
        }
    }

    fn group_members(&self, g: NodeId) -> Vec<(NodeId, NodeId)> {
        let mut walk = GroupWalk::default();
        self.walk_group(g, &mut walk, |_| {});
        walk.members
    }

    /// Whether `n` is a member of group `g`, which holds it: what
    /// `walk_group` finds, checked up `n`'s path instead of over `g`.
    fn is_member(&self, g: NodeId, n: NodeId) -> bool {
        let item = item_role(self.host.interaction(g).role);
        let shown = |a: NodeId| {
            self.host
                .node(a)
                .is_some_and(|h| !h.flags.contains(NodeFlags::INERT))
                && !self.host.display_none(a)
        };
        let member = if self.is_group(n) {
            self.group_stop(n).is_some()
        } else {
            self.candidate(n, item) && (!self.disabled(n) || self.focus == Some(n))
        };
        member
            && self.ancestors(n).take_while(|&a| a != g).all(|a| {
                shown(a)
                    && (a == n
                        || !self.is_group(a) && !(self.candidate(a, item) && !self.disabled(a)))
            })
    }

    /// The member of `g` that is `n` or holds it (a nested group), if
    /// `n` is in one: `members` are `g`'s.
    fn member_holding(&self, members: &[(NodeId, NodeId)], n: NodeId) -> Option<usize> {
        // Members don't nest, so at most one is on `n`'s path.
        self.ancestors(n)
            .find_map(|a| members.iter().position(|&(m, _)| m == a))
    }

    /// Group `g`'s Tab stop, as (member, leaf): the member holding the
    /// focus, else the first selected (checked or selected), else the
    /// last focused, else the first.
    pub(crate) fn group_stop(&self, g: NodeId) -> Option<(NodeId, NodeId)> {
        let members = self.group_members(g);
        self.stop_of(g, &members).map(|i| members[i])
    }

    fn stop_of(&self, g: NodeId, members: &[(NodeId, NodeId)]) -> Option<usize> {
        if members.is_empty() {
            return None;
        }
        let focused = self.focus.and_then(|f| self.member_holding(members, f));
        let selected = || {
            members.iter().position(|&(_, leaf)| {
                self.state_bits(leaf) & (state_bit::CHECKED | state_bit::SELECTED) != 0
            })
        };
        let last = || {
            let (n, generation) = self.groups.get(&g.0)?.last?;
            let live = self
                .host
                .node(n)
                .is_some_and(|h| h.generation == generation);
            live.then(|| self.member_holding(members, n)).flatten()
        };
        Some(focused.or_else(selected).or_else(last).unwrap_or(0))
    }

    /// The Tab stops group `g` takes away: every member's leaf but the
    /// stop's, and the disabled candidates left focusable. The Tab walk
    /// calls it with each group it reaches, reusing one `walk`, and
    /// filters once at the end.
    pub(crate) fn group_skips(&self, g: NodeId, skip: &mut Vec<u32>, walk: &mut GroupWalk) {
        self.walk_group(g, walk, |n| skip.push(n.0));
        let stop = self.stop_of(g, &walk.members);
        for (i, &(_, leaf)) in walk.members.iter().enumerate() {
            if Some(i) != stop {
                skip.push(leaf.0);
            }
        }
    }

    /// Focus moved to `f`: it is the last focused of each group it is a
    /// member of (through nested groups).
    pub(crate) fn note_group_focus(&mut self, f: NodeId) {
        if self.groups.is_empty() {
            return;
        }
        let Some(generation) = self.host.node(f).map(|n| n.generation) else {
            return;
        };
        let mut cur = f;
        let groups: Vec<NodeId> = self
            .ancestors(f)
            .skip(1)
            .filter(|&a| self.is_group(a))
            .collect();
        for g in groups {
            if !self.is_member(g, cur) {
                return;
            }
            if let Some(group) = self.groups.get_mut(&g.0) {
                group.last = Some((f, generation));
            }
            cur = g;
        }
    }

    /// Arrows, Home and End on a focused group member, after claims:
    /// the innermost group the focus is a member of (through nested
    /// groups) whose orientation takes the key moves focus, and with
    /// `SELECT_ON_FOCUS` activates the member reached. Not in a text
    /// input (its caret keeps the keys), nor with Ctrl, Alt or Meta,
    /// nor for a group around the Tab scope. Returns whether a group took
    /// the key.
    pub(crate) fn group_key(&mut self, k: &KeyInput) -> bool {
        if self.groups.is_empty() || k.mods.ctrl || k.mods.alt || k.mods.meta {
            return false;
        }
        let Some(f) = self.focus else {
            return false;
        };
        if self.host.kind(f) == Some(crate::mutation::NodeKind::Input) {
            return false;
        }
        let axis = match k.key {
            Key::Left | Key::Right => group_flag::HORIZONTAL,
            Key::Up | Key::Down => group_flag::VERTICAL,
            Key::Home | Key::End => group_flag::HORIZONTAL | group_flag::VERTICAL,
            _ => return false,
        };
        // Groups around the Tab scope (a trap in a group) don't apply.
        let scope = self.tab_scope();
        let mut groups = Vec::new();
        for a in self.ancestors(f).skip(1) {
            if self.is_group(a) {
                groups.push(a);
            }
            if a == scope {
                break;
            }
        }
        let mut cur = f;
        for g in groups {
            let members = self.group_members(g);
            let Some(i) = members.iter().position(|&(m, _)| m == cur) else {
                return false;
            };
            let flags = self.groups[&g.0].flags;
            if flags & axis == 0 {
                cur = g;
                continue;
            }
            let (n, looped) = (members.len(), flags & group_flag::LOOP != 0);
            let j = match k.key {
                Key::Home => 0,
                Key::End => n - 1,
                Key::Right | Key::Down if looped => (i + 1) % n,
                Key::Right | Key::Down => (i + 1).min(n - 1),
                _ if looped => (i + n - 1) % n,
                _ => i.saturating_sub(1),
            };
            let leaf = members[j].1;
            if leaf != f {
                self.set_focus(Some(leaf));
                // With no modifiers, as the web's `click()`: Shift+↓ is
                // no Shift+click.
                if flags & group_flag::SELECT_ON_FOCUS != 0 {
                    self.activate(leaf, activate_source::KEY, Mods::default());
                }
            }
            return true;
        }
        false
    }
}
