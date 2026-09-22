//! cAST-style chunking: recursive split-then-merge over the syntax tree, in row space.
//!
//! * A unit (child node plus its leading comments/attributes) that fits in `max_lines` is a
//!   candidate; adjacent candidates merge greedily while the merged span fits.
//! * A unit larger than `max_lines` is split into its children. The node's header (for example
//!   `impl Foo for Bar {`) is carried into the first piece instead of becoming its own chunk.
//! * A childless node larger than `max_lines` (a giant string or comment) is cut every
//!   `max_lines` rows.
//! * Post-passes trim blank edges, cover any stray non-blank rows, and merge undersized chunks
//!   into a neighbour when the result still fits.

use laya_core::{Chunk, Lang};
use tree_sitter::{Node, Tree};

use crate::ChunkConfig;
use crate::defs::{Def, KindTable, node_rows};
use crate::lines::Lines;

/// Beyond this nesting depth, big nodes are cut by rows instead of descended (stack safety).
const MAX_DEPTH: usize = 256;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Affinity {
    /// Merge into whichever neighbour yields the smaller chunk.
    Either,
    /// Trailing piece of a split node (closing brace, last small method): merge backwards.
    Back,
    /// Leading piece flushed before a split node: merge forwards.
    Fwd,
}

#[derive(Debug, Clone, Copy)]
struct Seg<'k> {
    start: usize,
    end: usize,
    kind: &'k str,
    weight: usize,
    affinity: Affinity,
}

impl<'k> Seg<'k> {
    fn rows(&self) -> usize {
        self.end - self.start + 1
    }

    fn absorb(&mut self, other: &Seg<'k>) {
        self.start = self.start.min(other.start);
        self.end = self.end.max(other.end);
        if other.weight > self.weight {
            self.weight = other.weight;
            self.kind = other.kind;
        }
    }
}

/// A child node plus the comment/attribute rows directly before it.
struct Unit<'t> {
    start: usize,
    end: usize,
    node: Node<'t>,
    node_start: usize,
    has_trivia: bool,
}

impl<'t> Unit<'t> {
    fn seg(&self) -> Seg<'t> {
        let weight = if self.node.is_named() {
            self.end - self.node_start + 1
        } else {
            0
        };
        Seg {
            start: self.start,
            end: self.end,
            kind: kind_of(self.node),
            weight,
            affinity: Affinity::Either,
        }
    }
}

/// Node kind, looking through wrappers (`export_statement`, `decorated_definition`,
/// `template_declaration`) to the declaration they wrap.
fn kind_of(node: Node<'_>) -> &str {
    match node.kind() {
        "export_statement" | "decorated_definition" | "template_declaration" => node
            .child_by_field_name("declaration")
            .or_else(|| node.child_by_field_name("definition"))
            .or_else(|| {
                node.named_child(
                    u32::try_from(node.named_child_count().saturating_sub(1)).unwrap_or(0),
                )
            })
            .map_or_else(|| node.kind(), |n| n.kind()),
        k => k,
    }
}

struct Segmenter<'a, 't> {
    max: usize,
    header_max: usize,
    rows: usize,
    table: &'a KindTable,
    out: Vec<Seg<'t>>,
}

impl<'a, 't> Segmenter<'a, 't> {
    fn rows_of(&self, node: Node<'_>, floor: usize) -> Option<(usize, usize)> {
        let (s, e) = node_rows(node);
        let e = e.min(self.rows - 1);
        let s = s.max(floor);
        (s <= e).then_some((s, e))
    }

    /// Children of `node` as units. Rows already owned by an earlier sibling (tokens sharing a
    /// line) are clamped away so units never overlap; fully-covered children are dropped.
    fn units(&self, node: Node<'t>, floor: usize) -> Vec<Unit<'t>> {
        let mut units = Vec::new();
        let mut next_free = floor;
        let mut trivia: Option<(usize, usize, Node<'t>)> = None;
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            let Some((s, e)) = self.rows_of(child, next_free) else {
                continue;
            };
            next_free = e + 1;
            if self.table.is_trivia(child) {
                trivia = Some(match trivia {
                    Some((ts, _, tn)) => (ts, e, tn),
                    None => (s, e, child),
                });
                continue;
            }
            match trivia.take() {
                Some((ts, _, _)) if child.is_named() => {
                    units.push(Unit {
                        start: ts,
                        end: e,
                        node: child,
                        node_start: s,
                        has_trivia: true,
                    });
                    continue;
                }
                Some((ts, te, tn)) => units.push(Unit {
                    start: ts,
                    end: te,
                    node: tn,
                    node_start: ts,
                    has_trivia: false,
                }),
                None => {}
            }
            units.push(Unit {
                start: s,
                end: e,
                node: child,
                node_start: s,
                has_trivia: false,
            });
        }
        if let Some((ts, te, tn)) = trivia {
            units.push(Unit {
                start: ts,
                end: te,
                node: tn,
                node_start: ts,
                has_trivia: false,
            });
        }
        units
    }

    fn push(&mut self, seg: Seg<'t>) {
        self.out.push(seg);
    }

    fn cut(&mut self, start: usize, end: usize, kind: &'t str) {
        let mut a = start;
        while a <= end {
            let b = (a + self.max - 1).min(end);
            self.push(Seg {
                start: a,
                end: b,
                kind,
                weight: b - a + 1,
                affinity: Affinity::Either,
            });
            a = b + 1;
        }
    }

    /// Leading comment rows of a split unit, carried into its first piece when small enough.
    fn trivia_carry(&mut self, prefix: Option<Seg<'t>>, u: &Unit<'t>) -> Option<Seg<'t>> {
        let trivia = u.has_trivia.then(|| Seg {
            start: u.start,
            end: u.node_start - 1,
            kind: "comment",
            weight: 0,
            affinity: Affinity::Fwd,
        });
        let merged = match (prefix, trivia) {
            (Some(mut p), Some(t)) => {
                p.absorb(&t);
                Some(p)
            }
            (p, t) => p.or(t),
        };
        match merged {
            Some(mut c) if c.rows() <= self.max / 2 => {
                // Headers and comments never decide the chunk's kind.
                c.weight = 0;
                Some(c)
            }
            Some(c) => {
                // Too long to carry: emit the prefix and the comment block on their own.
                if let Some(mut p) = prefix {
                    p.affinity = Affinity::Fwd;
                    self.push(p);
                }
                if u.has_trivia {
                    let from = prefix.map_or(c.start, |p| p.end + 1).max(u.start);
                    self.cut(from, u.node_start - 1, "comment");
                }
                None
            }
            None => None,
        }
    }

    /// Emit `node` (clamped to `floor`), prefixed by `carry`.
    fn node(&mut self, node: Node<'t>, floor: usize, carry: Option<Seg<'t>>, depth: usize) {
        let Some((s, e)) = self.rows_of(node, floor) else {
            if let Some(c) = carry {
                self.push(c);
            }
            return;
        };
        let start = carry.map_or(s, |c| c.start);
        let own = Seg {
            start: s,
            end: e,
            kind: node.kind(),
            weight: e - s + 1,
            affinity: Affinity::Either,
        };
        if e - start < self.max {
            let mut seg = own;
            if let Some(c) = carry {
                seg.absorb(&c);
            }
            self.push(seg);
            return;
        }
        if node.child_count() == 0 || depth >= MAX_DEPTH {
            self.cut(start, e, node.kind());
            return;
        }
        self.children(node, s, carry, depth, false);
    }

    fn children(
        &mut self,
        node: Node<'t>,
        floor: usize,
        carry: Option<Seg<'t>>,
        depth: usize,
        is_root: bool,
    ) {
        let units = self.units(node, floor);
        let mut acc = carry;
        let mut flushed = false;
        // First row this node itself contributed to `acc` (as opposed to the inherited carry).
        let mut own_from: Option<usize> = None;
        for u in units {
            let useg = u.seg();
            if let Some(a) = acc.as_mut()
                && u.end - a.start < self.max
            {
                a.absorb(&useg);
                own_from.get_or_insert(u.start);
                continue;
            }
            let fits = u.end - u.start < self.max;
            // A comment-only unit may span several comment nodes; it is never descended.
            let trivia_only = self.table.is_trivia(u.node);
            // `acc` is this node's header (plus inherited carry) when nothing was flushed yet, the
            // node's own leading rows are few, and the whole carry stays well under max_lines.
            let is_header = !is_root
                && !flushed
                && acc.is_some_and(|a| {
                    let own = own_from.map_or(0, |f| a.end + 1 - f);
                    own <= self.header_max && a.rows() <= self.max / 2
                });
            if is_header && !trivia_only {
                // Keep this node's header (`impl X {`, `class Y:`) with the first piece of `u`.
                let c = self.trivia_carry(acc.take(), &u);
                self.node(u.node, u.node_start, c, depth + 1);
                flushed = true;
                continue;
            }
            if let Some(mut a) = acc.take() {
                a.affinity = if fits {
                    Affinity::Either
                } else {
                    Affinity::Fwd
                };
                self.push(a);
                flushed = true;
            }
            if fits {
                acc = Some(useg);
                own_from = Some(u.start);
                continue;
            }
            if trivia_only {
                self.cut(u.start, u.end, useg.kind);
                flushed = true;
                continue;
            }
            let c = self.trivia_carry(None, &u);
            self.node(u.node, u.node_start, c, depth + 1);
            flushed = true;
        }
        if let Some(mut a) = acc {
            if flushed {
                a.affinity = Affinity::Back;
            }
            self.push(a);
        }
    }
}

/// Trim blank edges, cover stray non-blank rows, then merge undersized segments.
fn finish<'k>(mut segs: Vec<Seg<'k>>, lines: &Lines<'_>, cfg: &ChunkConfig) -> Vec<Seg<'k>> {
    let n = lines.len();
    segs.retain_mut(|s| {
        s.end = s.end.min(n.saturating_sub(1));
        while s.start <= s.end && lines.is_blank(s.start) {
            s.start += 1;
        }
        while s.end > s.start && lines.is_blank(s.end) {
            s.end -= 1;
        }
        s.start <= s.end && !lines.is_blank(s.start)
    });

    // Safety net: every non-blank row belongs to exactly one segment.
    let mut covered = vec![false; n];
    for s in &segs {
        covered[s.start..=s.end].iter_mut().for_each(|c| *c = true);
    }
    let mut r = 0;
    let mut extra = Vec::new();
    while r < n {
        if covered[r] || lines.is_blank(r) {
            r += 1;
            continue;
        }
        let start = r;
        let mut end = r;
        while r < n && !covered[r] && r - start < cfg.max_lines {
            if !lines.is_blank(r) {
                end = r;
            }
            r += 1;
        }
        extra.push(Seg {
            start,
            end,
            kind: "text",
            weight: end - start + 1,
            affinity: Affinity::Either,
        });
    }
    if !extra.is_empty() {
        segs.extend(extra);
        segs.sort_by_key(|s| s.start);
    }

    merge_small(&mut segs, cfg);
    segs
}

/// Merge each undersized segment into a neighbour while the merged span fits `max_lines`.
/// Spans only grow, so a segment rejected once can never become mergeable later.
fn merge_small(segs: &mut Vec<Seg<'_>>, cfg: &ChunkConfig) {
    let mut i = 0;
    while i < segs.len() {
        if segs[i].rows() >= cfg.min_lines {
            i += 1;
            continue;
        }
        let back = (i > 0)
            .then(|| segs[i].end - segs[i - 1].start + 1)
            .filter(|&m| m <= cfg.max_lines);
        let fwd = (i + 1 < segs.len())
            .then(|| segs[i + 1].end - segs[i].start + 1)
            .filter(|&m| m <= cfg.max_lines);
        let go_back = match (back, fwd, segs[i].affinity) {
            (None, None, _) => {
                i += 1;
                continue;
            }
            (Some(_), None, _) => true,
            (None, Some(_), _) => false,
            (Some(_), Some(_), Affinity::Back) => true,
            (Some(_), Some(_), Affinity::Fwd) => false,
            (Some(b), Some(f), Affinity::Either) => b <= f,
        };
        let seg = segs.remove(i);
        if go_back {
            segs[i - 1].absorb(&seg);
            i -= 1;
        } else {
            let aff = segs[i].affinity;
            segs[i].absorb(&seg);
            segs[i].affinity = aff;
        }
    }
}

/// Symbol paths (`impl Store for MoonStore > fn get`) for every def, built parent-first.
fn def_paths(defs: &[Def]) -> Vec<String> {
    let mut paths: Vec<String> = Vec::with_capacity(defs.len());
    for d in defs {
        let p = match d.parent {
            Some(p) => format!("{} > {}", paths[p], d.label),
            None => d.label.clone(),
        };
        paths.push(p);
    }
    paths
}

/// Symbol of a chunk: the deepest definition containing it, refined to its dominant direct
/// child definition when that child covers more than half of the chunk's non-blank rows.
fn chunk_symbol(defs: &[Def], paths: &[String], lines: &Lines<'_>, s: usize, e: usize) -> String {
    let mut enclosing: Option<usize> = None;
    for (i, d) in defs.iter().enumerate() {
        if d.start > s {
            break;
        }
        if d.end >= e {
            enclosing = Some(i);
        }
    }
    let total = lines.nonblank_count(s, e);
    let mut best: Option<(usize, usize)> = None;
    for (i, d) in defs.iter().enumerate().skip(enclosing.map_or(0, |x| x + 1)) {
        if d.start > e {
            break;
        }
        if d.parent != enclosing || d.end < s {
            continue;
        }
        let ov = lines.nonblank_count(d.start.max(s), d.end.min(e));
        if best.is_none_or(|(_, b)| ov > b) {
            best = Some((i, ov));
        }
    }
    match best {
        Some((i, ov)) if 2 * ov > total => paths[i].clone(),
        _ => enclosing.map(|i| paths[i].clone()).unwrap_or_default(),
    }
}

fn chunk_defines(defs: &[Def], s: usize, e: usize) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for d in defs {
        if d.start > e {
            break;
        }
        if d.name_row < s || d.name_row > e {
            continue;
        }
        if let Some(name) = &d.name
            && !out.contains(name)
        {
            out.push(name.clone());
        }
    }
    out
}

/// Chunk a parsed file.
pub(crate) fn chunk_tree(
    cfg: &ChunkConfig,
    lang: Lang,
    path: &str,
    lines: &Lines<'_>,
    tree: &Tree,
    table: &KindTable,
    defs: &[Def],
) -> Vec<Chunk> {
    if lines.len() == 0 {
        return Vec::new();
    }
    let mut seg = Segmenter {
        max: cfg.max_lines,
        header_max: (cfg.max_lines / 4).max(1),
        rows: lines.len(),
        table,
        out: Vec::new(),
    };
    seg.children(tree.root_node(), 0, None, 0, true);
    let segs = finish(seg.out, lines, cfg);
    let paths = def_paths(defs);
    segs.into_iter()
        .map(|s| Chunk {
            path: path.to_string(),
            start_line: (s.start + 1) as u32,
            end_line: (s.end + 1) as u32,
            lang,
            symbol: chunk_symbol(defs, &paths, lines, s.start, s.end),
            kind: s.kind.to_string(),
            defines: chunk_defines(defs, s.start, s.end),
            text: lines.text(s.start, s.end).to_string(),
        })
        .collect()
}
