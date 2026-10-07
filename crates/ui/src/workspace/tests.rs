use super::*;

fn s(id: &str) -> TabKey {
    TabKey::session(id)
}

/// Assert every documented invariant.
fn check(ws: &Workspace) {
    let order = ws.groups_in_reading_order();
    let tree: HashSet<_> = order.iter().copied().collect();
    assert_eq!(tree.len(), order.len(), "group repeated in tree");
    let map: HashSet<_> = ws.groups.keys().copied().collect();
    assert_eq!(tree, map, "tree and map disagree");
    let mut tabs = HashSet::new();
    for group in ws.groups.values() {
        for tab in &group.tabs {
            assert!(tabs.insert(tab.clone()), "duplicate tab {tab:?}");
        }
        assert!(group.active < group.tabs.len().max(1));
    }
    fn walk(node: &Node, parent: Option<Axis>) {
        if let Node::Split { axis, children } = node {
            assert!(
                children.len() >= 2,
                "split with {} children",
                children.len()
            );
            assert_ne!(Some(*axis), parent, "same-axis nesting");
            let sum: f32 = children.iter().map(|(_, f)| f).sum();
            assert!((sum - 1.0).abs() < 1e-4, "fractions sum {sum}");
            for (child, _) in children {
                walk(child, Some(*axis));
            }
        }
    }
    walk(&ws.root, None);
    assert!(ws.groups.contains_key(&ws.focused));
    if let Some(z) = ws.zoomed {
        assert!(ws.groups.contains_key(&z));
    }
    assert!(ws.groups.keys().all(|id| id.0 < ws.next_id));
}

/// Tabs per group in reading order.
fn shape(ws: &Workspace) -> Vec<Vec<TabKey>> {
    ws.groups_in_reading_order()
        .into_iter()
        .map(|id| ws.groups[&id].tabs.clone())
        .collect()
}

fn fractions(node: &Node) -> Vec<f32> {
    match node {
        Node::Split { children, .. } => children.iter().map(|(_, f)| *f).collect(),
        Node::Group(_) => vec![],
    }
}

fn approx(a: &[f32], b: &[f32]) -> bool {
    a.len() == b.len() && a.iter().zip(b).all(|(x, y)| (x - y).abs() < 1e-4)
}

fn with_tabs(preset: Preset, tabs: &[&str]) -> Workspace {
    let mut ws = Workspace::new();
    for t in tabs {
        ws.open(s(t));
    }
    ws.apply_preset(preset);
    check(&ws);
    ws
}

#[test]
fn new_is_one_empty_focused_group() {
    let ws = Workspace::new();
    check(&ws);
    assert_eq!(ws.group_count(), 1);
    assert!(ws.group(ws.focused()).unwrap().is_empty());
    assert_eq!(ws.focused_tab(), None);
}

#[test]
fn open_adds_to_focused_and_refocuses_existing() {
    let mut ws = Workspace::new();
    let g0 = ws.focused();
    ws.open(s("a"));
    ws.open(s("b"));
    assert_eq!(ws.focused_tab(), Some(&s("b")));
    let g1 = ws.split_group(g0, Edge::Right).unwrap();
    assert_eq!(ws.focused(), g1);
    ws.open(s("c"));
    assert_eq!(ws.find(&s("c")), Some((g1, 0)));
    // Already open: activate it where it is, don't duplicate.
    assert_eq!(ws.open(s("a")), g0);
    assert_eq!(ws.focused(), g0);
    assert_eq!(ws.focused_tab(), Some(&s("a")));
    assert_eq!(ws.tabs().count(), 3);
    check(&ws);
}

#[test]
fn open_in_dead_group_falls_back_to_focused() {
    let mut ws = Workspace::new();
    let g = ws.open_in(GroupId(99), s("a"));
    assert_eq!(g, ws.focused());
    check(&ws);
}

#[test]
fn preset_shapes() {
    use Axis::{Horizontal as H, Vertical as V};
    let top = |ws: &Workspace| match &ws.root {
        Node::Split { axis, children } => Some((*axis, children.len())),
        Node::Group(_) => None,
    };
    let ws = with_tabs(Preset::Single, &[]);
    assert_eq!((ws.group_count(), top(&ws)), (1, None));
    for (preset, axis, n) in [
        (Preset::Columns2, H, 2),
        (Preset::Rows2, V, 2),
        (Preset::Columns3, H, 3),
        (Preset::Rows3, V, 3),
    ] {
        let ws = with_tabs(preset, &[]);
        assert_eq!(ws.group_count(), n);
        assert_eq!(top(&ws), Some((axis, n)));
        let even = vec![1.0 / n as f32; n];
        assert!(approx(&fractions(&ws.root), &even));
    }
    for (preset, n) in [(Preset::Grid2x2, 2), (Preset::Grid3x3, 3)] {
        let ws = with_tabs(preset, &[]);
        assert_eq!(ws.group_count(), n * n);
        assert_eq!(top(&ws), Some((V, n)));
        let rects = ws.group_rects();
        let cell = 1.0 / n as f32;
        for (i, (_, r)) in rects.iter().enumerate() {
            let (row, col) = (i / n, i % n);
            assert!((r.x - col as f32 * cell).abs() < 1e-4);
            assert!((r.y - row as f32 * cell).abs() < 1e-4);
            assert!((r.w - cell).abs() < 1e-4 && (r.h - cell).abs() < 1e-4);
        }
    }
    let ws = with_tabs(Preset::TwoStackedPlusOne, &[]);
    let rects: Vec<Rect> = ws.group_rects().into_iter().map(|(_, r)| r).collect();
    assert_eq!(rects.len(), 3);
    assert!(rects[0].x == 0.0 && rects[0].y == 0.0 && rects[0].h == 0.5);
    assert!(rects[1].x == 0.0 && rects[1].y == 0.5);
    assert!(rects[2].x == 0.5 && rects[2].h == 1.0);
    let ws = with_tabs(Preset::OnePlusTwoStacked, &[]);
    let rects: Vec<Rect> = ws.group_rects().into_iter().map(|(_, r)| r).collect();
    assert!(rects[0].x == 0.0 && rects[0].h == 1.0);
    assert!(rects[1].x == 0.5 && rects[1].y == 0.0);
    assert!(rects[2].x == 0.5 && rects[2].y == 0.5);
    assert_eq!(Preset::ALL.len(), 9);
}

#[test]
fn preset_distributes_tabs_and_keeps_focus() {
    // Fewer tabs than groups: one each, rest empty.
    let mut ws = Workspace::new();
    ws.open(s("a"));
    ws.open(s("b"));
    ws.activate(ws.focused(), 0);
    ws.apply_preset(Preset::Columns3);
    check(&ws);
    assert_eq!(shape(&ws), vec![vec![s("a")], vec![s("b")], vec![]]);
    assert_eq!(ws.focused_tab(), Some(&s("a")));

    // More tabs than groups: leftovers join the last group, focus follows.
    let mut ws = Workspace::new();
    for t in ["a", "b", "c", "d"] {
        ws.open(s(t));
    }
    ws.activate(ws.focused(), 2);
    let before = ws.focused();
    ws.apply_preset(Preset::Columns2);
    check(&ws);
    assert_eq!(shape(&ws), vec![vec![s("a")], vec![s("b"), s("c"), s("d")]]);
    assert_eq!(ws.focused_tab(), Some(&s("c")));
    // Existing group ids are reused in reading order.
    assert_eq!(ws.groups_in_reading_order()[0], before);

    // Back to single: everything in one group, focused tab still active.
    ws.apply_preset(Preset::Single);
    check(&ws);
    assert_eq!(shape(&ws), vec![vec![s("a"), s("b"), s("c"), s("d")]]);
    assert_eq!(ws.focused_tab(), Some(&s("c")));
}

#[test]
fn close_collapses_group_and_moves_focus() {
    let mut ws = with_tabs(Preset::Columns3, &["a", "b", "c"]);
    let ids = ws.groups_in_reading_order();
    ws.focus(ids[1]);
    assert!(ws.close(&s("b")));
    check(&ws);
    assert_eq!(shape(&ws), vec![vec![s("a")], vec![s("c")]]);
    // The left neighbour absorbs the space and the focus.
    assert_eq!(ws.focused(), ids[0]);
    assert!(approx(&fractions(&ws.root), &[2.0 / 3.0, 1.0 / 3.0]));

    // Collapsing to one group replaces the split with the group.
    assert!(ws.close(&s("a")));
    check(&ws);
    assert_eq!(ws.root, Node::Group(ids[2]));
    assert_eq!(ws.focused(), ids[2]);

    // The last group may be empty.
    assert!(ws.close(&s("c")));
    check(&ws);
    assert_eq!(ws.group_count(), 1);
    assert!(!ws.close(&s("c")));
}

#[test]
fn close_keeps_neighbour_active_tab() {
    let mut ws = Workspace::new();
    for t in ["a", "b", "c"] {
        ws.open(s(t));
    }
    let g = ws.focused();
    ws.activate(g, 1);
    ws.close(&s("a"));
    assert_eq!(ws.focused_tab(), Some(&s("b")));
    ws.close(&s("b"));
    assert_eq!(ws.focused_tab(), Some(&s("c")));
    check(&ws);
}

#[test]
fn close_in_nested_split_collapses_to_parent() {
    // 2x2; closing one cell turns its row into a single group, which must
    // not leave a vertical split nested in a vertical split.
    let mut ws = with_tabs(Preset::Grid2x2, &["a", "b", "c", "d"]);
    ws.close(&s("b"));
    check(&ws);
    let Node::Split { axis, children } = &ws.root else {
        panic!("expected split");
    };
    assert_eq!(*axis, Axis::Vertical);
    assert!(matches!(children[0].0, Node::Group(_)));
    assert_eq!(shape(&ws), vec![vec![s("a")], vec![s("c")], vec![s("d")]]);
}

#[test]
fn move_to_edge_creates_split() {
    let mut ws = Workspace::new();
    ws.open(s("a"));
    ws.open(s("b"));
    let g0 = ws.focused();
    assert!(ws.move_tab(&s("b"), g0, Drop::Edge(Edge::Right)));
    check(&ws);
    assert_eq!(shape(&ws), vec![vec![s("a")], vec![s("b")]]);
    let Node::Split { axis, .. } = &ws.root else {
        panic!()
    };
    assert_eq!(*axis, Axis::Horizontal);
    assert_eq!(ws.focused_tab(), Some(&s("b")));

    // Only tab onto its own edge: no-op.
    let g1 = ws.find(&s("b")).unwrap().0;
    let snapshot = ws.clone();
    assert!(!ws.move_tab(&s("b"), g1, Drop::Edge(Edge::Bottom)));
    assert_eq!(ws, snapshot);

    // Moving the only tab out removes its group.
    assert!(ws.move_tab(&s("b"), g0, Drop::Edge(Edge::Top)));
    check(&ws);
    assert_eq!(shape(&ws), vec![vec![s("b")], vec![s("a")]]);
    let Node::Split { axis, .. } = &ws.root else {
        panic!()
    };
    assert_eq!(*axis, Axis::Vertical);
}

#[test]
fn move_to_center_joins_group() {
    let mut ws = with_tabs(Preset::Columns2, &["a", "b"]);
    let ids = ws.groups_in_reading_order();
    assert!(ws.move_tab(&s("a"), ids[1], Drop::Center));
    check(&ws);
    assert_eq!(shape(&ws), vec![vec![s("b"), s("a")]]);
    assert_eq!(ws.focused_tab(), Some(&s("a")));
    // Own centre just activates.
    assert!(!ws.move_tab(&s("b"), ids[1], Drop::Center));
    assert_eq!(ws.focused_tab(), Some(&s("b")));
    // Unknown tab / group.
    assert!(!ws.move_tab(&s("zz"), ids[1], Drop::Center));
    assert!(!ws.move_tab(&s("a"), GroupId(999), Drop::Center));
}

#[test]
fn move_within_reorders_and_activates() {
    let mut ws = Workspace::new();
    for t in ["a", "b", "c"] {
        ws.open(s(t));
    }
    let g = ws.focused();
    assert!(ws.move_within(g, 2, 0));
    check(&ws);
    assert_eq!(shape(&ws), vec![vec![s("c"), s("a"), s("b")]]);
    assert_eq!(ws.focused_tab(), Some(&s("c")));
    assert!(ws.move_within(g, 0, 2));
    assert_eq!(shape(&ws), vec![vec![s("a"), s("b"), s("c")]]);
    // Same slot just activates; out of range / unknown group: no-op.
    assert!(!ws.move_within(g, 1, 1));
    assert_eq!(ws.focused_tab(), Some(&s("b")));
    let snapshot = ws.clone();
    assert!(!ws.move_within(g, 0, 3));
    assert!(!ws.move_within(GroupId(999), 0, 1));
    assert_eq!(ws, snapshot);
}

#[test]
fn move_to_index_joins_at_position() {
    let mut ws = with_tabs(Preset::Columns2, &["a", "b"]);
    let before = shape(&ws);
    assert_eq!(before.len(), 2);
    assert_eq!(before[1].len(), 1);
    let ids = ws.groups_in_reading_order();
    let lone = before[1][0].clone();
    // Into the first group's bar at the front; its old group collapses.
    assert!(ws.move_to_index(&lone, ids[0], 0));
    check(&ws);
    assert_eq!(ws.group_count(), 1);
    assert_eq!(ws.group(ids[0]).unwrap().tabs()[0], lone);
    assert_eq!(ws.focused_tab(), Some(&lone));
    // Past the end clamps (append); within the group it reorders.
    let first = ws.group(ids[0]).unwrap().tabs()[0].clone();
    assert!(ws.move_to_index(&first, ids[0], 99));
    check(&ws);
    assert_eq!(ws.group(ids[0]).unwrap().tabs().last(), Some(&first));
    assert_eq!(ws.focused_tab(), Some(&first));
    // Unknown tab / group.
    assert!(!ws.move_to_index(&s("zz"), ids[0], 0));
    assert!(!ws.move_to_index(&first, GroupId(999), 0));
}

#[test]
fn move_to_index_keeps_a_nonempty_source() {
    let mut ws = Workspace::new();
    ws.open(s("a"));
    ws.open(s("b"));
    let left = ws.focused();
    let right = ws.split_group(left, Edge::Right).unwrap();
    assert!(ws.move_to_index(&s("a"), right, 0));
    check(&ws);
    assert_eq!(shape(&ws), vec![vec![s("b")], vec![s("a")]]);
    assert_eq!(ws.focused(), right);
    assert_eq!(ws.group(left).unwrap().active_tab(), Some(&s("b")));
}

#[test]
fn same_axis_splits_flatten() {
    let mut ws = Workspace::new();
    let g0 = ws.focused();
    let g1 = ws.split_group(g0, Edge::Right).unwrap();
    let g2 = ws.split_group(g1, Edge::Right).unwrap();
    check(&ws);
    let Node::Split { axis, children } = &ws.root else {
        panic!()
    };
    assert_eq!(*axis, Axis::Horizontal);
    assert_eq!(children.len(), 3);
    // Splitting halves the target.
    assert!(approx(&fractions(&ws.root), &[0.5, 0.25, 0.25]));
    assert_eq!(ws.groups_in_reading_order(), vec![g0, g1, g2]);

    // Directly normalize a nested same-axis tree.
    let mut ws = Workspace::new();
    let [a, b, c] = [GroupId(10), GroupId(11), GroupId(12)];
    for id in [a, b, c] {
        ws.groups.insert(id, Group::default());
    }
    ws.root = Node::Split {
        axis: Axis::Vertical,
        children: vec![
            (Node::Group(a), 0.5),
            (
                Node::Split {
                    axis: Axis::Vertical,
                    children: vec![(Node::Group(b), 0.4), (Node::Group(c), 0.6)],
                },
                0.5,
            ),
        ],
    };
    ws.normalize();
    check(&ws);
    assert!(approx(&fractions(&ws.root), &[0.5, 0.2, 0.3]));
    assert!(
        !ws.groups.contains_key(&GroupId(0)),
        "unreferenced group dropped"
    );
}

#[test]
fn two_stacked_plus_one_via_ops() {
    let mut ws = Workspace::new();
    ws.open(s("a"));
    ws.open(s("b"));
    ws.open(s("c"));
    let g0 = ws.focused();
    ws.move_tab(&s("c"), g0, Drop::Edge(Edge::Right));
    ws.move_tab(&s("b"), g0, Drop::Edge(Edge::Bottom));
    check(&ws);
    let ops = ws.clone();
    let preset = with_tabs(Preset::TwoStackedPlusOne, &["a", "b", "c"]);
    assert_eq!(shape(&ops), shape(&preset));
    let rects =
        |ws: &Workspace| -> Vec<Rect> { ws.group_rects().into_iter().map(|(_, r)| r).collect() };
    assert_eq!(rects(&ops), rects(&preset));
}

#[test]
fn resize_clamps_and_equalize() {
    let mut ws = with_tabs(Preset::Columns3, &[]);
    assert!(ws.resize(&[], 0, 0.1, 0.1));
    assert!(approx(
        &fractions(&ws.root),
        &[1.0 / 3.0 + 0.1, 1.0 / 3.0 - 0.1, 1.0 / 3.0]
    ));
    // Clamped at the minimum.
    assert!(ws.resize(&[], 0, 5.0, 0.1));
    let f = fractions(&ws.root);
    assert!((f[1] - 0.1).abs() < 1e-4 && (f[0] + f[1] - 2.0 / 3.0).abs() < 1e-4);
    assert!(!ws.resize(&[], 0, 1.0, 0.1), "already at the limit");
    assert!(!ws.resize(&[], 1, -5.0, 0.1), "child 1 is at its minimum");
    assert!(ws.resize(&[], 1, 5.0, 0.1));
    assert!((fractions(&ws.root)[2] - 0.1).abs() < 1e-4);
    check(&ws);
    // Bad input.
    assert!(!ws.resize(&[], 2, 0.1, 0.1));
    assert!(!ws.resize(&[0], 0, 0.1, 0.1));
    assert!(!ws.resize(&[], 0, f32::NAN, 0.1));
    // An impossible minimum can't panic or invert.
    ws.resize(&[], 0, 0.3, 0.9);
    check(&ws);
    assert!(ws.equalize(&[]));
    assert!(approx(&fractions(&ws.root), &[1.0 / 3.0; 3]));

    // Nested path.
    let mut ws = with_tabs(Preset::Grid2x2, &[]);
    assert!(ws.resize(&[1], 0, 0.2, 0.05));
    assert!(approx(&fractions(ws.root.at(&[1]).unwrap()), &[0.7, 0.3]));
    check(&ws);
}

#[test]
fn zoom() {
    let mut ws = with_tabs(Preset::Columns2, &["a", "b"]);
    let ids = ws.groups_in_reading_order();
    assert!(ws.toggle_zoom(ids[1]));
    assert_eq!(ws.zoomed(), Some(ids[1]));
    assert_eq!(ws.focused(), ids[1]);
    assert_eq!(ws.visible_tabs(), vec![&s("b")]);
    assert!(!ws.toggle_zoom(ids[1]));
    assert_eq!(ws.visible_tabs(), vec![&s("a"), &s("b")]);
    // Removing the zoomed group clears zoom.
    ws.toggle_zoom(ids[1]);
    ws.close(&s("b"));
    assert_eq!(ws.zoomed(), None);
    check(&ws);
    // Splitting clears zoom.
    ws.toggle_zoom(ids[0]);
    ws.split_group(ids[0], Edge::Right);
    assert_eq!(ws.zoomed(), None);
}

#[test]
fn retain_tabs_prunes_and_collapses() {
    let mut ws = with_tabs(Preset::Columns3, &["a", "b", "c", "d"]);
    // Last group holds c, d with d active.
    let last = ws.groups_in_reading_order()[2];
    ws.activate(last, 1);
    assert!(ws.retain_tabs(|t| t != &s("b") && t != &s("c")));
    check(&ws);
    assert_eq!(shape(&ws), vec![vec![s("a")], vec![s("d")]]);
    assert_eq!(ws.group(last).unwrap().active_tab(), Some(&s("d")));
    assert!(!ws.retain_tabs(|_| true));
    assert!(ws.retain_tabs(|_| false));
    check(&ws);
    assert_eq!(ws.group_count(), 1);
    assert_eq!(ws.tabs().count(), 0);
}

#[test]
fn new_session_tab_and_replace() {
    let mut ws = Workspace::new();
    let canvas = ws.new_session_tab();
    let other = ws.new_session_tab();
    assert_ne!(canvas, other);
    ws.open(canvas.clone());
    assert!(ws.replace_tab(&canvas, s("chat")));
    assert_eq!(ws.focused_tab(), Some(&s("chat")));
    assert!(!ws.contains(&canvas));
    // Replacing with an already-open session focuses it and drops the old.
    let g0 = ws.focused();
    let canvas = ws.new_session_tab();
    ws.open_split(canvas.clone(), g0, Edge::Right).unwrap();
    assert!(ws.replace_tab(&canvas, s("chat")));
    check(&ws);
    assert_eq!(shape(&ws), vec![vec![s("chat")]]);
    assert_eq!(s("chat").chat_id(), Some("chat"));
    assert_eq!(canvas.chat_id(), None);
}

#[test]
fn open_split_places_or_moves() {
    let mut ws = Workspace::new();
    ws.open(s("a"));
    let g0 = ws.focused();
    let g1 = ws.open_split(s("b"), g0, Edge::Right).unwrap();
    assert_eq!(ws.focused(), g1);
    assert_eq!(shape(&ws), vec![vec![s("a")], vec![s("b")]]);
    // Already open elsewhere: moves.
    let g2 = ws.open_split(s("a"), g1, Edge::Bottom).unwrap();
    check(&ws);
    assert_eq!(ws.find(&s("a")), Some((g2, 0)));
    assert_eq!(ws.group_count(), 2);
    assert!(ws.open_split(s("c"), GroupId(999), Edge::Left).is_none());
}

#[test]
fn serde_round_trip() {
    let mut ws = with_tabs(Preset::Grid2x2, &["a", "b", "c"]);
    let canvas = ws.new_session_tab();
    ws.open(canvas);
    ws.resize(&[0], 0, 0.1, 0.05);
    ws.toggle_zoom(ws.groups_in_reading_order()[1]);
    let json = serde_json::to_string(&ws).unwrap();
    assert!(json.contains("\"nextId\""), "camelCase: {json}");
    let back: Workspace = serde_json::from_str(&json).unwrap();
    assert_eq!(back, ws);
    check(&back);
}

#[test]
fn repair_corrupted_input() {
    // Missing everything.
    let ws: Workspace = serde_json::from_str("{}").unwrap();
    check(&ws);
    assert_eq!(ws.group_count(), 1);

    // Dangling tree ids, unreferenced groups, duplicate tabs and leaf ids,
    // bad fractions, single-child and same-axis splits, dead focus/zoom,
    // out-of-range active, stale nextId.
    let json = r#"{
            "root": {"split": {"axis": "horizontal", "children": [
                [{"group": 1}, -3.0],
                [{"split": {"axis": "horizontal", "children": [
                    [{"group": 2}, 1.0], [{"group": 7}, 1.0]
                ]}}, 0.0],
                [{"split": {"axis": "vertical", "children": [[{"group": 3}, 1.0]]}}, 2.0],
                [{"group": 1}, 1.0]
            ]}},
            "groups": {
                "1": {"tabs": [{"session": "a"}, {"session": "b"}], "active": 9},
                "2": {"tabs": [{"session": "b"}, {"newSession": 40}], "active": 0},
                "3": {"tabs": [{"session": "a"}]},
                "5": {"tabs": [{"session": "x"}]}
            },
            "focused": 42,
            "zoomed": 7,
            "nextId": 0
        }"#;
    let ws: Workspace = serde_json::from_str(json).unwrap();
    check(&ws);
    assert_eq!(
        ws.groups_in_reading_order(),
        vec![GroupId(1), GroupId(2), GroupId(3)]
    );
    assert_eq!(
        shape(&ws),
        vec![vec![s("a"), s("b")], vec![TabKey::NewSession(40)], vec![]]
    );
    assert_eq!(ws.group(GroupId(1)).unwrap().active(), 1);
    assert_eq!(ws.focused(), GroupId(1));
    assert_eq!(ws.zoomed(), None);
    assert!(ws.next_id > 40);
    let Node::Split { axis, children } = &ws.root else {
        panic!()
    };
    assert_eq!(*axis, Axis::Horizontal);
    assert_eq!(children.len(), 3);

    // A tree with no live groups gets a fresh one.
    let ws: Workspace = serde_json::from_str(r#"{"root": {"group": 3}, "groups": {}}"#).unwrap();
    check(&ws);
    assert_eq!(ws.group_count(), 1);
}

#[test]
fn neighbours_in_grids() {
    let ws = with_tabs(Preset::Grid2x2, &[]);
    let g = ws.groups_in_reading_order();
    assert_eq!(ws.neighbour(g[0], Edge::Right), Some(g[1]));
    assert_eq!(ws.neighbour(g[0], Edge::Bottom), Some(g[2]));
    assert_eq!(ws.neighbour(g[3], Edge::Left), Some(g[2]));
    assert_eq!(ws.neighbour(g[3], Edge::Top), Some(g[1]));
    assert_eq!(ws.neighbour(g[0], Edge::Left), None);
    assert_eq!(ws.neighbour(g[0], Edge::Top), None);

    let ws = with_tabs(Preset::Grid3x3, &[]);
    let g = ws.groups_in_reading_order();
    let centre = g[4];
    assert_eq!(ws.neighbour(centre, Edge::Left), Some(g[3]));
    assert_eq!(ws.neighbour(centre, Edge::Right), Some(g[5]));
    assert_eq!(ws.neighbour(centre, Edge::Top), Some(g[1]));
    assert_eq!(ws.neighbour(centre, Edge::Bottom), Some(g[7]));
    assert_eq!(ws.neighbour(g[8], Edge::Right), None);
    assert_eq!(ws.neighbour(g[2], Edge::Bottom), Some(g[5]));

    // Uneven: from the tall right tile, the longest shared border wins;
    // ties go to reading order.
    let mut ws = with_tabs(Preset::TwoStackedPlusOne, &[]);
    let g = ws.groups_in_reading_order();
    assert_eq!(ws.neighbour(g[2], Edge::Left), Some(g[0]));
    ws.resize(&[0], 0, 0.2, 0.05);
    assert_eq!(ws.neighbour(g[2], Edge::Left), Some(g[0]));
    ws.resize(&[0], 0, -0.4, 0.05);
    assert_eq!(ws.neighbour(g[2], Edge::Left), Some(g[1]));
    assert_eq!(ws.neighbour(g[1], Edge::Right), Some(g[2]));
    assert_eq!(ws.neighbour(GroupId(999), Edge::Right), None);
}

#[test]
fn close_group_hands_tabs_and_focus_to_the_heir() {
    let mut ws = Workspace::new();
    ws.open(s("a"));
    let right = ws.open_split(s("b"), ws.focused(), Edge::Right).unwrap();
    ws.open_in(right, s("c"));
    ws.activate(right, 0);
    let left = ws.groups_in_reading_order()[0];
    assert!(ws.close_group(right));
    check(&ws);
    assert_eq!(ws.group_count(), 1);
    assert_eq!(shape(&ws), vec![vec![s("a"), s("b"), s("c")]]);
    // The heir keeps its own active tab and takes focus.
    assert_eq!(ws.focused(), left);
    assert_eq!(ws.focused_tab(), Some(&s("a")));
    // The last group never closes.
    assert!(!ws.close_group(left));
    check(&ws);
}

#[test]
fn close_group_into_an_empty_heir_keeps_the_moved_active_tab() {
    let mut ws = Workspace::new();
    let left = ws.focused();
    let right = ws.split_group(left, Edge::Right).unwrap();
    ws.open_in(right, s("a"));
    ws.open_in(right, s("b"));
    ws.activate(right, 0);
    // Focus elsewhere survives closing an unfocused group.
    ws.focus(left);
    assert!(ws.close_group(right));
    check(&ws);
    assert_eq!(ws.focused(), left);
    assert_eq!(ws.focused_tab(), Some(&s("a")));
    assert!(!ws.close_group(GroupId(999)));
}
