//! The bookmark (outline) tree, built from the backend's pre-order list.

use pl_geom::Point;
use pl_render::OutlineItem;

/// One bookmark with its children.
#[derive(Clone, Debug, PartialEq)]
pub struct BookmarkNode {
    pub title: String,
    /// Zero-based target page, when the bookmark has a resolvable destination.
    pub page: Option<u32>,
    /// Target position in page space (unrotated user space, y-up), per axis where given.
    pub x: Option<f64>,
    pub y: Option<f64>,
    /// Destination zoom as the file states it (1.0 = 100%); `None` or 0 means "keep".
    pub zoom: Option<f64>,
    pub children: Vec<BookmarkNode>,
}

impl BookmarkNode {
    /// The target point, when the destination gives both coordinates.
    pub fn point(&self) -> Option<Point> {
        Some(Point::new(self.x?, self.y?))
    }

    /// This node plus all its descendants.
    pub fn count(&self) -> usize {
        1 + self.children.iter().map(BookmarkNode::count).sum::<usize>()
    }
}

/// One row of a flattened tree (see [`BookmarkTree::flatten`]).
#[derive(Clone, Copy, Debug)]
pub struct BookmarkRow<'a> {
    /// 0 for top-level bookmarks.
    pub depth: usize,
    pub node: &'a BookmarkNode,
}

/// A document's bookmarks.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct BookmarkTree {
    pub roots: Vec<BookmarkNode>,
}

impl BookmarkTree {
    /// Builds the tree from a pre-order list with levels (0 = top). A level that jumps by more
    /// than one is treated as one deeper than its predecessor, so a malformed list still nests.
    pub fn from_outline(items: &[OutlineItem]) -> BookmarkTree {
        fn close(stack: &mut Vec<BookmarkNode>, roots: &mut Vec<BookmarkNode>, to_depth: usize) {
            while stack.len() > to_depth {
                let node = stack.pop().expect("non-empty");
                match stack.last_mut() {
                    Some(parent) => parent.children.push(node),
                    None => roots.push(node),
                }
            }
        }
        // `stack[d]` is the node currently open at depth `d`.
        let mut roots: Vec<BookmarkNode> = Vec::new();
        let mut stack: Vec<BookmarkNode> = Vec::new();
        for item in items {
            let depth = (item.level as usize).min(stack.len());
            close(&mut stack, &mut roots, depth);
            stack.push(BookmarkNode {
                title: item.title.clone(),
                page: item.page,
                x: item.x,
                y: item.y,
                zoom: item.zoom,
                children: Vec::new(),
            });
        }
        close(&mut stack, &mut roots, 0);
        BookmarkTree { roots }
    }

    pub fn is_empty(&self) -> bool {
        self.roots.is_empty()
    }

    /// Total number of bookmarks at every depth.
    pub fn len(&self) -> usize {
        self.roots.iter().map(BookmarkNode::count).sum()
    }

    /// Every bookmark in document order with its depth.
    pub fn flatten(&self) -> Vec<BookmarkRow<'_>> {
        fn walk<'a>(nodes: &'a [BookmarkNode], depth: usize, out: &mut Vec<BookmarkRow<'a>>) {
            for node in nodes {
                out.push(BookmarkRow { depth, node });
                walk(&node.children, depth + 1, out);
            }
        }
        let mut out = Vec::with_capacity(self.len());
        walk(&self.roots, 0, &mut out);
        out
    }

    /// The node at a path of child indices (`[1, 0]` = first child of the second root).
    pub fn get(&self, path: &[usize]) -> Option<&BookmarkNode> {
        let (first, rest) = path.split_first()?;
        let mut node = self.roots.get(*first)?;
        for i in rest {
            node = node.children.get(*i)?;
        }
        Some(node)
    }

    /// The last bookmark, in document order, with the greatest target page at or before `page`:
    /// the entry a bookmarks panel highlights for the current page. Returned as a child-index
    /// path for [`BookmarkTree::get`].
    pub fn path_for_page(&self, page: u32) -> Option<Vec<usize>> {
        fn walk(
            nodes: &[BookmarkNode],
            page: u32,
            path: &mut Vec<usize>,
            best: &mut Option<(u32, Vec<usize>)>,
        ) {
            for (i, node) in nodes.iter().enumerate() {
                path.push(i);
                if let Some(p) = node.page {
                    if p <= page && best.as_ref().is_none_or(|(b, _)| p >= *b) {
                        *best = Some((p, path.clone()));
                    }
                }
                walk(&node.children, page, path, best);
                path.pop();
            }
        }
        let mut best = None;
        walk(&self.roots, page, &mut Vec::new(), &mut best);
        best.map(|(_, path)| path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(title: &str, level: u32, page: u32) -> OutlineItem {
        OutlineItem {
            title: title.into(),
            level,
            page: Some(page),
            x: Some(72.0),
            y: Some(700.0),
            zoom: None,
        }
    }

    #[test]
    fn builds_a_nested_tree_in_document_order() {
        let tree = BookmarkTree::from_outline(&[
            item("A", 0, 0),
            item("A1", 1, 0),
            item("A1a", 2, 1),
            item("A2", 1, 2),
            item("B", 0, 3),
            item("B1", 1, 4),
        ]);
        assert_eq!(tree.roots.len(), 2);
        assert_eq!(tree.len(), 6);
        assert_eq!(tree.roots[0].children.len(), 2);
        assert_eq!(tree.roots[0].children[0].children[0].title, "A1a");
        assert_eq!(tree.get(&[1, 0]).unwrap().title, "B1");
        assert!(tree.get(&[2]).is_none());
        assert!(tree.get(&[]).is_none());
        let rows: Vec<(usize, &str)> = tree
            .flatten()
            .iter()
            .map(|r| (r.depth, r.node.title.as_str()))
            .collect();
        assert_eq!(
            rows,
            [
                (0, "A"),
                (1, "A1"),
                (2, "A1a"),
                (1, "A2"),
                (0, "B"),
                (1, "B1")
            ]
        );
        assert_eq!(tree.roots[0].point(), Some(Point::new(72.0, 700.0)));
    }

    #[test]
    fn a_level_jump_nests_one_deeper_and_an_empty_list_is_empty() {
        let tree =
            BookmarkTree::from_outline(&[item("A", 0, 0), item("deep", 5, 1), item("B", 0, 2)]);
        assert_eq!(tree.roots.len(), 2);
        assert_eq!(tree.roots[0].children[0].title, "deep");
        assert!(BookmarkTree::from_outline(&[]).is_empty());
        // A list that starts below the top level still produces a root.
        let odd = BookmarkTree::from_outline(&[item("x", 3, 0)]);
        assert_eq!(odd.roots.len(), 1);
    }

    #[test]
    fn path_for_page_picks_the_last_bookmark_at_or_before_the_page() {
        let tree = BookmarkTree::from_outline(&[
            item("S1", 0, 0),
            item("p1", 1, 0),
            item("p2", 1, 1),
            item("S2", 0, 6),
            item("p7", 1, 6),
            item("p8", 1, 7),
        ]);
        assert_eq!(tree.path_for_page(1), Some(vec![0, 1]));
        assert_eq!(tree.path_for_page(5), Some(vec![0, 1]));
        assert_eq!(tree.path_for_page(6), Some(vec![1, 0]));
        assert_eq!(tree.path_for_page(99), Some(vec![1, 1]));
        assert_eq!(BookmarkTree::default().path_for_page(0), None);
    }
}
