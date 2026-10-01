use crate::{graphics::Rect, View, ViewId};
use slotmap::SlotMap;
use std::ops::{Index, IndexMut};

const MIN_VIEW_WIDTH: u16 = 3;
const MIN_VIEW_HEIGHT: u16 = 2;

// the dimensions are recomputed on window resize/tree change.
//
#[derive(Debug)]
pub struct Tree {
    root: ViewId,
    // (container, index inside the container)
    pub focus: ViewId,
    // fullscreen: bool,
    area: Rect,

    nodes: SlotMap<ViewId, Node>,

    // used for traversals
    stack: Vec<(ViewId, Rect)>,
}

#[derive(Debug)]
pub struct Node {
    parent: ViewId,
    content: Content,
}

#[derive(Debug)]
pub enum Content {
    View(Box<View>),
    Container(Box<Container>),
}

impl Node {
    pub fn container(layout: Layout) -> Self {
        Self {
            parent: ViewId::default(),
            content: Content::Container(Box::new(Container::new(layout))),
        }
    }

    pub fn view(view: View) -> Self {
        Self {
            parent: ViewId::default(),
            content: Content::View(Box::new(view)),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Layout {
    Horizontal,
    Vertical,
    // could explore stacked/tabbed
}

#[derive(Debug, Clone, Copy)]
pub enum Direction {
    Up,
    Down,
    Left,
    Right,
}

/// A divider between two adjacent slots. Generational IDs make stale mouse
/// drags harmless when a split is closed, moved, or transposed.
#[derive(Debug, Clone, Copy)]
pub struct Divider {
    container: ViewId,
    before: ViewId,
    after: ViewId,
    layout: Layout,
}

impl Divider {
    pub fn coordinate(self, column: u16, row: u16) -> u16 {
        match self.layout {
            Layout::Vertical => column,
            Layout::Horizontal => row,
        }
    }
}

#[derive(Debug)]
struct Child {
    view: ViewId,
    // Relative size requested by the user, independent of terminal size and
    // temporary minimum-size constraints. Sizes belong to slots, not buffers.
    weight: f64,
}

#[derive(Debug, Default)]
struct Children(Vec<Child>);

impl Children {
    fn len(&self) -> usize {
        self.0.len()
    }

    fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    fn iter(&self) -> impl DoubleEndedIterator<Item = &ViewId> + ExactSizeIterator {
        self.0.iter().map(|child| &child.view)
    }

    fn insert(&mut self, index: usize, view: ViewId) {
        // New splits get an equal share; existing splits retain their ratios.
        let weight = if self.is_empty() {
            1.0
        } else {
            self.0.iter().map(|child| child.weight).sum::<f64>() / self.len() as f64
        };
        self.0.insert(index, Child { view, weight });
    }

    fn push(&mut self, view: ViewId) {
        self.insert(self.len(), view);
    }

    fn pop(&mut self) -> Option<ViewId> {
        self.0.pop().map(|child| child.view)
    }

    fn remove(&mut self, index: usize) {
        self.0.remove(index);
    }
}

impl Index<usize> for Children {
    type Output = ViewId;

    fn index(&self, index: usize) -> &Self::Output {
        &self.0[index].view
    }
}

impl IndexMut<usize> for Children {
    fn index_mut(&mut self, index: usize) -> &mut Self::Output {
        &mut self.0[index].view
    }
}

#[derive(Debug)]
pub struct Container {
    layout: Layout,
    children: Children,
    area: Rect,
}

impl Container {
    pub fn new(layout: Layout) -> Self {
        Self {
            layout,
            children: Children::default(),
            area: Rect::default(),
        }
    }
}

impl Default for Container {
    fn default() -> Self {
        Self::new(Layout::Vertical)
    }
}

/// Allocate whole cells, conserving the available space exactly. Minimum sizes
/// constrain the rendered layout without overwriting the requested proportions.
fn allocate(total: u16, weights: &[f64], minimums: &[u16]) -> Vec<u16> {
    // Invalid sizing data must not leave holes in the layout.
    let equal_weights;
    let weights = if weights
        .iter()
        .any(|&weight| !weight.is_finite() || weight <= 0.0)
        || !weights.iter().sum::<f64>().is_finite()
    {
        equal_weights = vec![1.0; weights.len()];
        &equal_weights[..]
    } else {
        weights
    };
    let mut sizes = vec![0; weights.len()];
    let mut fixed = vec![false; weights.len()];
    let mut remaining = total;
    let enforce_minimums =
        minimums.iter().map(|&min| u64::from(min)).sum::<u64>() <= u64::from(total);
    loop {
        let weight_sum: f64 = weights
            .iter()
            .zip(&fixed)
            .filter(|(_, fixed)| !**fixed)
            .map(|(weight, _)| weight)
            .sum();
        if weight_sum == 0.0 {
            break;
        }
        let mut constrained = false;
        if enforce_minimums {
            // Use the same remaining budget for every decision in this pass.
            let budget = remaining;
            for (i, &weight) in weights.iter().enumerate() {
                if !fixed[i] && f64::from(budget) * weight / weight_sum < f64::from(minimums[i]) {
                    sizes[i] = minimums[i];
                    remaining -= sizes[i];
                    fixed[i] = true;
                    constrained = true;
                }
            }
        }
        if constrained {
            continue;
        }
        let mut cumulative = 0.0;
        let mut allocated = 0;
        for (i, &weight) in weights.iter().enumerate() {
            if fixed[i] {
                continue;
            }
            cumulative += weight;
            let next = (f64::from(remaining) * cumulative / weight_sum).round() as u16;
            sizes[i] = next - allocated;
            allocated = next;
        }
        break;
    }
    sizes
}

impl Tree {
    pub fn new(area: Rect) -> Self {
        let root = Node::container(Layout::Vertical);

        let mut nodes = SlotMap::with_key();
        let root = nodes.insert(root);

        // root is it's own parent
        nodes[root].parent = root;

        Self {
            root,
            focus: root,
            // fullscreen: false,
            area,
            nodes,
            stack: Vec::new(),
        }
    }

    pub fn insert(&mut self, view: View) -> ViewId {
        let focus = self.focus;
        let parent = self.nodes[focus].parent;
        let mut node = Node::view(view);
        node.parent = parent;
        let node = self.nodes.insert(node);
        self.get_mut(node).id = node;

        let container = match &mut self.nodes[parent] {
            Node {
                content: Content::Container(container),
                ..
            } => container,
            _ => unreachable!(),
        };

        // insert node after the current item if there is children already
        let pos = if container.children.is_empty() {
            0
        } else {
            let pos = container
                .children
                .iter()
                .position(|&child| child == focus)
                .unwrap();
            pos + 1
        };

        container.children.insert(pos, node);
        // focus the new node
        self.focus = node;

        // recalculate all the sizes
        self.recalculate();

        node
    }

    pub fn split(&mut self, view: View, layout: Layout) -> ViewId {
        let focus = self.focus;
        let parent = self.nodes[focus].parent;

        let node = Node::view(view);
        let node = self.nodes.insert(node);
        self.get_mut(node).id = node;

        let container = match &mut self.nodes[parent] {
            Node {
                content: Content::Container(container),
                ..
            } => container,
            _ => unreachable!(),
        };
        if container.layout == layout {
            // insert node after the current item if there is children already
            let pos = if container.children.is_empty() {
                0
            } else {
                let pos = container
                    .children
                    .iter()
                    .position(|&child| child == focus)
                    .unwrap();
                pos + 1
            };
            container.children.insert(pos, node);
            self.nodes[node].parent = parent;
        } else {
            let mut split = Node::container(layout);
            split.parent = parent;
            let split = self.nodes.insert(split);

            let container = match &mut self.nodes[split] {
                Node {
                    content: Content::Container(container),
                    ..
                } => container,
                _ => unreachable!(),
            };
            container.children.push(focus);
            container.children.push(node);
            self.nodes[focus].parent = split;
            self.nodes[node].parent = split;

            let container = match &mut self.nodes[parent] {
                Node {
                    content: Content::Container(container),
                    ..
                } => container,
                _ => unreachable!(),
            };

            let pos = container
                .children
                .iter()
                .position(|&child| child == focus)
                .unwrap();

            // replace focus on parent with split
            container.children[pos] = split;
        }

        // focus the new node
        self.focus = node;

        // recalculate all the sizes
        self.recalculate();

        node
    }

    /// Get a mutable reference to a [Container] by index.
    /// # Panics
    /// Panics if `index` is not in self.nodes, or if the node's content is not a [Content::Container].
    fn container_mut(&mut self, index: ViewId) -> &mut Container {
        match &mut self.nodes[index] {
            Node {
                content: Content::Container(container),
                ..
            } => container,
            _ => unreachable!(),
        }
    }

    fn remove_or_replace(&mut self, child: ViewId, replacement: Option<ViewId>) {
        let parent = self.nodes[child].parent;

        self.nodes.remove(child);

        let container = self.container_mut(parent);
        let pos = container
            .children
            .iter()
            .position(|&item| item == child)
            .unwrap();

        if let Some(new) = replacement {
            container.children[pos] = new;
            self.nodes[new].parent = parent;
        } else {
            container.children.remove(pos);
        }
    }

    pub fn remove(&mut self, index: ViewId) {
        if self.focus == index {
            // focus on something else
            self.focus = self.prev();
        }

        let parent = self.nodes[index].parent;
        let parent_is_root = parent == self.root;

        self.remove_or_replace(index, None);

        let parent_container = self.container_mut(parent);
        if parent_container.children.len() == 1 && !parent_is_root {
            // Lets merge the only child back to its grandparent so that Views
            // are equally spaced.
            let sibling = parent_container.children.pop().unwrap();
            self.remove_or_replace(parent, Some(sibling));
        }

        self.recalculate()
    }

    pub fn views(&self) -> impl Iterator<Item = (&View, bool)> {
        let focus = self.focus;
        self.nodes.iter().filter_map(move |(key, node)| match node {
            Node {
                content: Content::View(view),
                ..
            } => Some((view.as_ref(), focus == key)),
            _ => None,
        })
    }

    pub fn views_mut(&mut self) -> impl Iterator<Item = (&mut View, bool)> {
        let focus = self.focus;
        self.nodes
            .iter_mut()
            .filter_map(move |(key, node)| match node {
                Node {
                    content: Content::View(view),
                    ..
                } => Some((view.as_mut(), focus == key)),
                _ => None,
            })
    }

    /// Get reference to a [View] by index.
    /// # Panics
    ///
    /// Panics if `index` is not in self.nodes, or if the node's content is not [Content::View]. This can be checked with [Self::contains].
    pub fn get(&self, index: ViewId) -> &View {
        self.try_get(index).unwrap()
    }

    /// Try to get reference to a [View] by index. Returns `None` if node content is not a [`Content::View`].
    ///
    /// Does not panic if the view does not exists anymore.
    pub fn try_get(&self, index: ViewId) -> Option<&View> {
        match self.nodes.get(index) {
            Some(Node {
                content: Content::View(view),
                ..
            }) => Some(view),
            _ => None,
        }
    }

    /// Get a mutable reference to a [View] by index.
    /// # Panics
    ///
    /// Panics if `index` is not in self.nodes, or if the node's content is not [Content::View]. This can be checked with [Self::contains].
    pub fn get_mut(&mut self, index: ViewId) -> &mut View {
        match &mut self.nodes[index] {
            Node {
                content: Content::View(view),
                ..
            } => view,
            _ => unreachable!(),
        }
    }

    /// Check if tree contains a [Node] with a given index.
    pub fn contains(&self, index: ViewId) -> bool {
        self.nodes.contains_key(index)
    }

    pub fn is_empty(&self) -> bool {
        match &self.nodes[self.root] {
            Node {
                content: Content::Container(container),
                ..
            } => container.children.is_empty(),
            _ => unreachable!(),
        }
    }

    pub fn resize(&mut self, area: Rect) -> bool {
        if self.area != area {
            self.area = area;
            self.recalculate();
            return true;
        }
        false
    }

    pub fn recalculate(&mut self) {
        if self.is_empty() {
            // There are no more views, so the tree should focus itself again.
            self.focus = self.root;

            return;
        }

        self.stack.push((self.root, self.area));

        // take the area
        // fetch the node
        // a) node is view, give it whole area
        // b) node is container, calculate areas for each child and push them on the stack

        while let Some((key, area)) = self.stack.pop() {
            let Content::Container(container) = &self.nodes[key].content else {
                self.get_mut(key).area = area;
                continue;
            };
            let layout = container.layout;
            let children: Vec<_> = container.children.iter().copied().collect();
            let weights: Vec<_> = container
                .children
                .0
                .iter()
                .map(|child| child.weight)
                .collect();
            let minimums: Vec<_> = children
                .iter()
                .map(|&child| self.minimum_size(child, layout))
                .collect();
            let length = match layout {
                Layout::Vertical => area.width,
                Layout::Horizontal => area.height,
            };
            let gaps = if layout == Layout::Vertical {
                length.min(children.len().saturating_sub(1).min(u16::MAX as usize) as u16)
            } else {
                0
            };
            let sizes = allocate(length - gaps, &weights, &minimums);
            self.container_mut(key).area = area;
            let mut offset = 0;
            for (i, (&child, size)) in children.iter().zip(sizes).enumerate() {
                let child_area = match layout {
                    Layout::Vertical => Rect::new(area.x + offset, area.y, size, area.height),
                    Layout::Horizontal => Rect::new(area.x, area.y + offset, area.width, size),
                };
                self.stack.push((child, child_area));
                offset += size + u16::from(i < gaps as usize);
            }
        }
    }

    fn node_area(&self, id: ViewId) -> Rect {
        match &self.nodes[id].content {
            Content::View(view) => view.area,
            Content::Container(container) => container.area,
        }
    }

    // Include a statusline and one text row. Narrow views can clip their gutters;
    // keeping this minimum small also permits compact reference panes.
    fn minimum_size(&self, id: ViewId, axis: Layout) -> u16 {
        match &self.nodes[id].content {
            Content::View(_) => match axis {
                Layout::Vertical => MIN_VIEW_WIDTH,
                Layout::Horizontal => MIN_VIEW_HEIGHT,
            },
            Content::Container(container) => {
                let sizes = container
                    .children
                    .iter()
                    .map(|&child| self.minimum_size(child, axis));
                if container.layout == axis {
                    let gaps = if axis == Layout::Vertical {
                        container
                            .children
                            .len()
                            .saturating_sub(1)
                            .min(u16::MAX as usize) as u16
                    } else {
                        0
                    };
                    sizes.fold(gaps, u16::saturating_add)
                } else {
                    sizes.max().unwrap_or(0)
                }
            }
        }
    }

    fn divider(&self, container: ViewId, index: usize) -> Divider {
        let Content::Container(node) = &self.nodes[container].content else {
            unreachable!()
        };
        Divider {
            container,
            before: node.children[index],
            after: node.children[index + 1],
            layout: node.layout,
        }
    }

    /// Find a vertical separator or a horizontal split's bottom statusline.
    pub fn divider_at(&self, column: u16, row: u16) -> Option<Divider> {
        // At a crossing, the visible vertical separator takes precedence over
        // a statusline. Do not depend on the order of nodes in the slot map.
        let mut horizontal = None;
        for (id, node) in &self.nodes {
            let Content::Container(container) = &node.content else {
                continue;
            };
            if column < container.area.x
                || column >= container.area.right()
                || row < container.area.y
                || row >= container.area.bottom()
            {
                continue;
            }
            for index in 0..container.children.len().saturating_sub(1) {
                let before = self.node_area(container.children[index]);
                let after = self.node_area(container.children[index + 1]);
                let hit = match container.layout {
                    Layout::Vertical => column == before.right() && column < after.x,
                    Layout::Horizontal => before.height > 0 && row == before.bottom() - 1,
                };
                if hit {
                    let divider = self.divider(id, index);
                    if container.layout == Layout::Vertical {
                        return Some(divider);
                    }
                    horizontal = Some(divider);
                }
            }
        }
        horizontal
    }

    /// Set a divider to an absolute terminal column/row. Space is taken from
    /// neighboring slots in order, stopping at each subtree's minimum size.
    /// Returns false for stale dividers or when no movement is possible.
    pub fn set_divider_position(&mut self, divider: Divider, position: u16) -> bool {
        let Some(Node {
            content: Content::Container(container),
            ..
        }) = self.nodes.get(divider.container)
        else {
            return false;
        };
        if container.layout != divider.layout {
            return false;
        }
        let Some(index) = container
            .children
            .0
            .windows(2)
            .position(|pair| pair[0].view == divider.before && pair[1].view == divider.after)
        else {
            return false;
        };
        let before = self.node_area(divider.before);
        let current = match divider.layout {
            Layout::Vertical => before.right(),
            Layout::Horizontal => before.bottom().saturating_sub(1),
        };
        let delta = i32::from(position) - i32::from(current);
        if delta == 0 {
            return false;
        }
        let mut sizes: Vec<_> = container
            .children
            .iter()
            .map(|&child| {
                let area = self.node_area(child);
                match divider.layout {
                    Layout::Vertical => area.width,
                    Layout::Horizontal => area.height,
                }
            })
            .collect();
        let minimums: Vec<_> = container
            .children
            .iter()
            .map(|&child| self.minimum_size(child, divider.layout))
            .collect();
        // A terminal smaller than the combined minima is rendered safely, but
        // cannot be manually resized until there is enough space again.
        if sizes.iter().zip(&minimums).any(|(size, min)| size < min) {
            return false;
        }
        let mut remaining = delta.unsigned_abs();
        let recipient = if delta > 0 { index } else { index + 1 };
        let donors: Box<dyn Iterator<Item = usize>> = if delta > 0 {
            Box::new(index + 1..sizes.len())
        } else {
            Box::new((0..=index).rev())
        };
        for donor in donors {
            let take = remaining.min(u32::from(sizes[donor] - minimums[donor])) as u16;
            sizes[donor] -= take;
            sizes[recipient] += take;
            remaining -= u32::from(take);
            if remaining == 0 {
                break;
            }
        }
        if remaining == delta.unsigned_abs() {
            return false;
        }
        // Integer sizes become exact proportions of the current usable area.
        for (child, size) in self
            .container_mut(divider.container)
            .children
            .0
            .iter_mut()
            .zip(sizes)
        {
            child.weight = f64::from(size);
        }
        self.recalculate();
        true
    }

    /// Grow/shrink the focused view along an axis by terminal cells. Prefer its
    /// trailing divider; at the last slot use the preceding divider instead.
    pub fn resize_view(&mut self, axis: Layout, delta: i32) -> bool {
        let mut child = self.focus;
        loop {
            let parent = self.nodes[child].parent;
            if parent == child {
                return false;
            }
            let Content::Container(container) = &self.nodes[parent].content else {
                unreachable!()
            };
            if container.layout == axis && container.children.len() > 1 {
                // Shrinking a view stops at that view's minimum, even though a
                // mouse drag can move the divider farther by shrinking peers.
                let area = self.node_area(child);
                let size = match axis {
                    Layout::Vertical => area.width,
                    Layout::Horizontal => area.height,
                };
                let available = size.saturating_sub(self.minimum_size(child, axis));
                let delta = delta.max(-i32::from(available));
                let index = container
                    .children
                    .iter()
                    .position(|&id| id == child)
                    .unwrap();
                let (index, delta) = if index + 1 == container.children.len() {
                    (index - 1, delta.saturating_neg())
                } else {
                    (index, delta)
                };
                let divider = self.divider(parent, index);
                let before = self.node_area(divider.before);
                let current = match axis {
                    Layout::Vertical => before.right(),
                    Layout::Horizontal => before.bottom().saturating_sub(1),
                };
                let position = i32::from(current)
                    .saturating_add(delta)
                    .clamp(0, i32::from(u16::MAX)) as u16;
                return self.set_divider_position(divider, position);
            }
            child = parent;
        }
    }

    /// Restore equal proportions throughout the split tree.
    pub fn equalize(&mut self) {
        for node in self.nodes.values_mut() {
            if let Content::Container(container) = &mut node.content {
                for child in &mut container.children.0 {
                    child.weight = 1.0;
                }
            }
        }
        self.recalculate();
    }

    pub fn traverse(&self) -> Traverse<'_> {
        Traverse::new(self)
    }

    // Finds the split in the given direction if it exists
    pub fn find_split_in_direction(&self, id: ViewId, direction: Direction) -> Option<ViewId> {
        let parent = self.nodes[id].parent;
        // Base case, we found the root of the tree
        if parent == id {
            return None;
        }
        // Parent must always be a container
        let parent_container = match &self.nodes[parent].content {
            Content::Container(container) => container,
            Content::View(_) => unreachable!(),
        };

        match (direction, parent_container.layout) {
            (Direction::Up, Layout::Vertical)
            | (Direction::Left, Layout::Horizontal)
            | (Direction::Right, Layout::Horizontal)
            | (Direction::Down, Layout::Vertical) => {
                // The desired direction of movement is not possible within
                // the parent container so the search must continue closer to
                // the root of the split tree.
                self.find_split_in_direction(parent, direction)
            }
            (Direction::Up, Layout::Horizontal)
            | (Direction::Down, Layout::Horizontal)
            | (Direction::Left, Layout::Vertical)
            | (Direction::Right, Layout::Vertical) => {
                // It's possible to move in the desired direction within
                // the parent container so an attempt is made to find the
                // correct child.
                match self.find_child(id, &parent_container.children, direction) {
                    // Child is found, search is ended
                    Some(id) => Some(id),
                    // A child is not found. This could be because of either two scenarios
                    // 1. Its not possible to move in the desired direction, and search should end
                    // 2. A layout like the following with focus at X and desired direction Right
                    // | _ | x |   |
                    // | _ _ _ |   |
                    // | _ _ _ |   |
                    // The container containing X ends at X so no rightward movement is possible
                    // however there still exists another view/container to the right that hasn't
                    // been explored. Thus another search is done here in the parent container
                    // before concluding it's not possible to move in the desired direction.
                    None => self.find_split_in_direction(parent, direction),
                }
            }
        }
    }

    fn find_child(&self, id: ViewId, children: &Children, direction: Direction) -> Option<ViewId> {
        let mut child_id = match direction {
            // index wise in the child list the Up and Left represents a -1
            // thus reversed iterator.
            Direction::Up | Direction::Left => children
                .iter()
                .rev()
                .skip_while(|i| **i != id)
                .copied()
                .nth(1)?,
            // Down and Right => +1 index wise in the child list
            Direction::Down | Direction::Right => {
                children.iter().skip_while(|i| **i != id).copied().nth(1)?
            }
        };
        let (current_x, current_y) = match &self.nodes[self.focus].content {
            Content::View(current_view) => (current_view.area.left(), current_view.area.top()),
            Content::Container(_) => unreachable!(),
        };

        // If the child is a container the search finds the closest container child
        // visually based on screen location.
        while let Content::Container(container) = &self.nodes[child_id].content {
            match (direction, container.layout) {
                (_, Layout::Vertical) => {
                    // find closest split based on x because y is irrelevant
                    // in a vertical container (and already correct based on previous search)
                    child_id = *container.children.iter().min_by_key(|id| {
                        let x = match &self.nodes[**id].content {
                            Content::View(view) => view.area.left(),
                            Content::Container(container) => container.area.left(),
                        };
                        (current_x as i16 - x as i16).abs()
                    })?;
                }
                (_, Layout::Horizontal) => {
                    // find closest split based on y because x is irrelevant
                    // in a horizontal container (and already correct based on previous search)
                    child_id = *container.children.iter().min_by_key(|id| {
                        let y = match &self.nodes[**id].content {
                            Content::View(view) => view.area.top(),
                            Content::Container(container) => container.area.top(),
                        };
                        (current_y as i16 - y as i16).abs()
                    })?;
                }
            }
        }
        Some(child_id)
    }

    pub fn prev(&self) -> ViewId {
        // This function is very dumb, but that's because we don't store any parent links.
        // (we'd be able to go parent.prev_sibling() recursively until we find something)
        // For now that's okay though, since it's unlikely you'll be able to open a large enough
        // number of splits to notice.

        let mut views = self
            .traverse()
            .rev()
            .skip_while(|&(id, _view)| id != self.focus)
            .skip(1); // Skip focused value
        if let Some((id, _)) = views.next() {
            id
        } else {
            // extremely crude, take the last item
            let (key, _) = self.traverse().next_back().unwrap();
            key
        }
    }

    pub fn next(&self) -> ViewId {
        // This function is very dumb, but that's because we don't store any parent links.
        // (we'd be able to go parent.next_sibling() recursively until we find something)
        // For now that's okay though, since it's unlikely you'll be able to open a large enough
        // number of splits to notice.

        let mut views = self
            .traverse()
            .skip_while(|&(id, _view)| id != self.focus)
            .skip(1); // Skip focused value
        if let Some((id, _)) = views.next() {
            id
        } else {
            // extremely crude, take the first item again
            let (key, _) = self.traverse().next().unwrap();
            key
        }
    }

    pub fn transpose(&mut self) {
        let focus = self.focus;
        let parent = self.nodes[focus].parent;
        if let Content::Container(container) = &mut self.nodes[parent].content {
            container.layout = match container.layout {
                Layout::Vertical => Layout::Horizontal,
                Layout::Horizontal => Layout::Vertical,
            };
            self.recalculate();
        }
    }

    pub fn swap_split_in_direction(&mut self, direction: Direction) -> Option<()> {
        let focus = self.focus;
        let target = self.find_split_in_direction(focus, direction)?;
        let focus_parent = self.nodes[focus].parent;
        let target_parent = self.nodes[target].parent;

        if focus_parent == target_parent {
            let parent = focus_parent;
            let [parent, focus, target] = self.nodes.get_disjoint_mut([parent, focus, target])?;
            match (&mut parent.content, &mut focus.content, &mut target.content) {
                (
                    Content::Container(parent),
                    Content::View(focus_view),
                    Content::View(target_view),
                ) => {
                    let focus_pos = parent.children.iter().position(|id| focus_view.id == *id)?;
                    let target_pos = parent
                        .children
                        .iter()
                        .position(|id| target_view.id == *id)?;
                    // swap node positions so that traversal order is kept
                    parent.children[focus_pos] = target_view.id;
                    parent.children[target_pos] = focus_view.id;
                    // swap area so that views rendered at the correct location
                    std::mem::swap(&mut focus_view.area, &mut target_view.area);

                    Some(())
                }
                _ => unreachable!(),
            }
        } else {
            let [focus_parent, target_parent, focus, target] =
                self.nodes
                    .get_disjoint_mut([focus_parent, target_parent, focus, target])?;
            match (
                &mut focus_parent.content,
                &mut target_parent.content,
                &mut focus.content,
                &mut target.content,
            ) {
                (
                    Content::Container(focus_parent),
                    Content::Container(target_parent),
                    Content::View(focus_view),
                    Content::View(target_view),
                ) => {
                    let focus_pos = focus_parent
                        .children
                        .iter()
                        .position(|id| focus_view.id == *id)?;
                    let target_pos = target_parent
                        .children
                        .iter()
                        .position(|id| target_view.id == *id)?;
                    // re-parent target and focus nodes
                    std::mem::swap(
                        &mut focus_parent.children[focus_pos],
                        &mut target_parent.children[target_pos],
                    );
                    std::mem::swap(&mut focus.parent, &mut target.parent);
                    // swap area so that views rendered at the correct location
                    std::mem::swap(&mut focus_view.area, &mut target_view.area);

                    Some(())
                }
                _ => unreachable!(),
            }
        }
    }

    pub fn area(&self) -> Rect {
        self.area
    }
}

#[derive(Debug)]
pub struct Traverse<'a> {
    tree: &'a Tree,
    stack: Vec<ViewId>, // TODO: reuse the one we use on update
}

impl<'a> Traverse<'a> {
    fn new(tree: &'a Tree) -> Self {
        Self {
            tree,
            stack: vec![tree.root],
        }
    }
}

impl<'a> Iterator for Traverse<'a> {
    type Item = (ViewId, &'a View);

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            let key = self.stack.pop()?;

            let node = &self.tree.nodes[key];

            match &node.content {
                Content::View(view) => return Some((key, view)),
                Content::Container(container) => {
                    self.stack.extend(container.children.iter().rev());
                }
            }
        }
    }
}

impl DoubleEndedIterator for Traverse<'_> {
    fn next_back(&mut self) -> Option<Self::Item> {
        loop {
            let key = self.stack.pop()?;

            let node = &self.tree.nodes[key];

            match &node.content {
                Content::View(view) => return Some((key, view)),
                Content::Container(container) => {
                    self.stack.extend(container.children.iter());
                }
            }
        }
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::editor::GutterConfig;
    use crate::DocumentId;

    fn new_view() -> View {
        View::new(DocumentId::default(), GutterConfig::default())
    }

    fn split_tree(width: u16, height: u16, count: usize, layout: Layout) -> (Tree, Vec<ViewId>) {
        let mut tree = Tree::new(Rect::new(0, 0, width, height));
        let mut ids = vec![tree.insert(new_view())];
        for _ in 1..count {
            ids.push(tree.split(new_view(), layout));
        }
        (tree, ids)
    }

    fn areas(tree: &Tree) -> Vec<Rect> {
        tree.traverse().map(|(_, view)| view.area).collect()
    }

    fn assert_within_parent(tree: &Tree) {
        for (_, node) in &tree.nodes {
            let Content::Container(container) = &node.content else {
                continue;
            };
            let mut last_end = match container.layout {
                Layout::Vertical => container.area.x,
                Layout::Horizontal => container.area.y,
            };
            for &child in container.children.iter() {
                let area = tree.node_area(child);
                assert!(area.x >= container.area.x && area.right() <= container.area.right());
                assert!(area.y >= container.area.y && area.bottom() <= container.area.bottom());
                match container.layout {
                    Layout::Vertical => {
                        assert!(area.x >= last_end);
                        assert_eq!(area.height, container.area.height);
                        last_end = area.right();
                    }
                    Layout::Horizontal => {
                        assert_eq!(area.y, last_end);
                        assert_eq!(area.width, container.area.width);
                        last_end = area.bottom();
                    }
                }
            }
            if !container.children.is_empty() {
                assert_eq!(
                    last_end,
                    match container.layout {
                        Layout::Vertical => container.area.right(),
                        Layout::Horizontal => container.area.bottom(),
                    }
                );
            }
        }
    }

    #[test]
    fn resize_exact_cells_and_equalize() {
        for axis in [Layout::Vertical, Layout::Horizontal] {
            let (mut tree, ids) = split_tree(121, 60, 3, axis);
            let initial = areas(&tree);
            tree.focus = ids[0];
            assert!(tree.resize_view(axis, 1));
            let before = initial[0];
            let after = tree.get(ids[0]).area;
            match axis {
                Layout::Vertical => assert_eq!(after.width, before.width + 1),
                Layout::Horizontal => assert_eq!(after.height, before.height + 1),
            }
            assert!(tree.resize_view(axis, -1));
            assert_eq!(areas(&tree), initial);
            tree.focus = ids[2];
            assert!(tree.resize_view(axis, 7));
            assert!(tree.resize_view(axis, -7));
            assert_eq!(areas(&tree), initial);
            tree.resize_view(axis, 13);
            tree.equalize();
            assert_eq!(areas(&tree), initial);
        }
    }

    #[test]
    fn resize_cascades_and_clamps_all_donors() {
        let (mut tree, ids) = split_tree(180, 40, 4, Layout::Vertical);
        tree.focus = ids[0];
        assert!(tree.resize_view(Layout::Vertical, i32::MAX));
        assert_eq!(tree.get(ids[0]).area.width, 168);
        for &id in &ids[1..] {
            assert_eq!(tree.get(id).area.width, MIN_VIEW_WIDTH);
        }
        assert!(!tree.resize_view(Layout::Vertical, 1));
        assert_within_parent(&tree);
        assert!(tree.resize_view(Layout::Vertical, i32::MIN));
        assert_eq!(tree.get(ids[0]).area.width, MIN_VIEW_WIDTH);
        assert_within_parent(&tree);
    }

    #[test]
    fn shrinking_middle_view_does_not_shrink_earlier_views() {
        let (mut tree, ids) = split_tree(180, 40, 4, Layout::Vertical);
        let initial = areas(&tree);
        tree.focus = ids[2];
        assert!(tree.resize_view(Layout::Vertical, i32::MIN));
        assert_eq!(tree.get(ids[0]).area, initial[0]);
        assert_eq!(tree.get(ids[1]).area, initial[1]);
        assert_eq!(tree.get(ids[2]).area.width, MIN_VIEW_WIDTH);
        assert!(!tree.resize_view(Layout::Vertical, -1));
        assert_within_parent(&tree);
    }

    quickcheck::quickcheck! {
        fn arbitrary_layout_edits_remain_inside_parent(actions: Vec<u8>) -> bool {
            let (mut tree, _) = split_tree(120, 40, 1, Layout::Vertical);
            for (step, action) in actions.into_iter().take(100).enumerate() {
                let axis = if action % 2 == 0 { Layout::Vertical } else { Layout::Horizontal };
                match action % 8 {
                    0 | 1 => { tree.split(new_view(), axis); }
                    2 if tree.views().count() > 1 => tree.remove(tree.focus),
                    3 => { tree.resize_view(axis, i32::from(action) - 128); }
                    4 => { tree.resize(Rect::new(2, 3, u16::from(action), step as u16)); }
                    5 => tree.transpose(),
                    6 => tree.focus = tree.prev(),
                    _ => tree.equalize(),
                }
                assert_within_parent(&tree);
            }
            true
        }
    }

    #[test]
    fn resize_respects_nested_minimums() {
        let (mut tree, ids) = split_tree(100, 30, 2, Layout::Vertical);
        tree.focus = ids[0];
        let bottom_left = tree.split(new_view(), Layout::Horizontal);
        let bottom_right = tree.split(new_view(), Layout::Vertical);
        tree.focus = ids[1];
        tree.resize_view(Layout::Vertical, 1000);
        assert_eq!(tree.get(ids[0]).area.width, 7);
        assert_eq!(tree.get(bottom_left).area.width, 3);
        assert_eq!(tree.get(bottom_right).area.width, 3);
        // Height changes find the horizontal ancestor, resizing the whole row.
        tree.focus = bottom_right;
        tree.resize_view(Layout::Horizontal, 1000);
        assert_eq!(tree.get(ids[0]).area.height, 2);
        assert_eq!(tree.get(bottom_left).area.height, 28);
        assert_eq!(tree.get(bottom_right).area.height, 28);
        assert_within_parent(&tree);
    }

    #[test]
    fn tiny_terminal_preserves_requested_layout() {
        let (mut tree, ids) = split_tree(180, 60, 4, Layout::Vertical);
        tree.focus = ids[1];
        tree.resize_view(Layout::Vertical, 80);
        tree.split(new_view(), Layout::Horizontal);
        tree.resize_view(Layout::Horizontal, 20);
        let initial = areas(&tree);
        for width in 0..15 {
            for height in 0..8 {
                tree.resize(Rect::new(2, 3, width, height));
                assert_within_parent(&tree);
            }
        }
        tree.resize(Rect::new(0, 0, 180, 60));
        assert_eq!(areas(&tree), initial);
        // More than twenty children is supported, even with too little space.
        for _ in 0..30 {
            tree.split(new_view(), Layout::Vertical);
        }
        for width in [0, 1, 10, 80, 180] {
            tree.resize(Rect::new(0, 0, width, 20));
            assert_within_parent(&tree);
        }
    }

    #[test]
    fn divider_hit_testing_and_absolute_drag() {
        let (mut tree, ids) = split_tree(101, 30, 2, Layout::Vertical);
        tree.resize(Rect::new(4, 7, 101, 30));
        let divider = tree.divider_at(54, 10).unwrap();
        assert!(tree.divider_at(53, 10).is_none());
        assert!(tree.divider_at(105, 10).is_none());
        assert!(tree.set_divider_position(divider, 60));
        assert_eq!(tree.get(ids[0]).area.right(), 60);
        tree.set_divider_position(divider, u16::MAX);
        assert_eq!(tree.get(ids[1]).area.width, MIN_VIEW_WIDTH);
        // Absolute positions don't accumulate drag errors after clamping.
        tree.set_divider_position(divider, 59);
        assert_eq!(tree.get(ids[0]).area.right(), 59);
        tree.transpose();
        assert!(!tree.set_divider_position(divider, 30));
        let border_row = tree.get(ids[0]).area.bottom() - 1;
        let divider = tree.divider_at(10, border_row).unwrap();
        tree.set_divider_position(divider, 20);
        assert_eq!(tree.get(ids[0]).area.bottom(), 21);
        tree.remove(ids[0]);
        assert!(!tree.set_divider_position(divider, 10));
    }

    #[test]
    fn divider_crossings_prefer_vertical_separator() {
        let (mut tree, ids) = split_tree(101, 30, 2, Layout::Horizontal);
        tree.focus = ids[0];
        tree.split(new_view(), Layout::Vertical);
        let area = tree.get(ids[0]).area;
        let divider = tree.divider_at(area.right(), area.bottom() - 1).unwrap();
        assert_eq!(divider.layout, Layout::Vertical);
        tree.set_divider_position(divider, area.right() + 10);
        assert_eq!(tree.get(ids[0]).area.width, area.width + 10);
        assert_eq!(tree.get(ids[0]).area.height, area.height);
    }

    #[test]
    fn transpose_and_swap_preserve_slot_proportions() {
        let (mut tree, ids) = split_tree(101, 100, 2, Layout::Vertical);
        tree.focus = ids[0];
        tree.resize_view(Layout::Vertical, 30);
        let original = areas(&tree);
        tree.transpose();
        assert_eq!(tree.get(ids[0]).area.height, 80);
        tree.transpose();
        assert_eq!(areas(&tree), original);
        tree.swap_split_in_direction(Direction::Right);
        assert_eq!(tree.get(ids[1]).area, original[0]);
        assert_eq!(tree.get(ids[0]).area, original[1]);
        tree.recalculate();
        assert_eq!(tree.get(ids[1]).area, original[0]);
        assert_eq!(tree.get(ids[0]).area, original[1]);
    }

    #[test]
    fn split_remove_and_collapse_preserve_proportions() {
        let (mut tree, ids) = split_tree(121, 40, 2, Layout::Vertical);
        tree.focus = ids[0];
        tree.resize_view(Layout::Vertical, 20);
        let initial = areas(&tree);
        let nested = tree.split(new_view(), Layout::Horizontal);
        tree.remove(nested);
        assert_eq!(areas(&tree), initial);
        tree.focus = ids[0];
        let added = tree.split(new_view(), Layout::Vertical);
        assert_within_parent(&tree);
        tree.remove(added);
        assert_eq!(areas(&tree), initial);
        tree.remove(ids[0]);
        assert_eq!(tree.get(ids[1]).area, tree.area());
        tree.remove(ids[1]);
        assert!(tree.is_empty());
        assert!(!tree.resize_view(Layout::Vertical, 10));
    }

    #[test]
    fn allocation_conserves_cells_and_honors_minimums() {
        for total in 0..200 {
            for weights in [
                vec![1.0, 1.0, 1.0],
                vec![1.0, 80.0, 3.0],
                vec![10000.0, 1.0, 1.0],
            ] {
                let minimums = [3, 7, 11];
                let sizes = allocate(total, &weights, &minimums);
                assert_eq!(sizes.iter().sum::<u16>(), total);
                if total >= 21 {
                    assert!(sizes.iter().zip(minimums).all(|(&size, min)| size >= min));
                }
            }
        }
        for weights in [
            [0.0, 0.0],
            [f64::NAN, 1.0],
            [-1.0, 2.0],
            [f64::MAX, f64::MAX],
        ] {
            assert_eq!(allocate(101, &weights, &[3, 3]), vec![51, 50]);
        }
    }

    #[test]
    fn find_split_in_direction() {
        let mut tree = Tree::new(Rect {
            x: 0,
            y: 0,
            width: 180,
            height: 80,
        });
        let mut view = View::new(DocumentId::default(), GutterConfig::default());
        view.area = Rect::new(0, 0, 180, 80);
        tree.insert(view);

        let l0 = tree.focus;
        let view = View::new(DocumentId::default(), GutterConfig::default());
        tree.split(view, Layout::Vertical);
        let r0 = tree.focus;

        tree.focus = l0;
        let view = View::new(DocumentId::default(), GutterConfig::default());
        tree.split(view, Layout::Horizontal);
        let l1 = tree.focus;

        tree.focus = l0;
        let view = View::new(DocumentId::default(), GutterConfig::default());
        tree.split(view, Layout::Vertical);

        // Tree in test
        // | L0  | L2 |    |
        // |    L1    | R0 |
        let l2 = tree.focus;
        assert_eq!(Some(l0), tree.find_split_in_direction(l2, Direction::Left));
        assert_eq!(Some(l1), tree.find_split_in_direction(l2, Direction::Down));
        assert_eq!(Some(r0), tree.find_split_in_direction(l2, Direction::Right));
        assert_eq!(None, tree.find_split_in_direction(l2, Direction::Up));

        tree.focus = l1;
        assert_eq!(None, tree.find_split_in_direction(l1, Direction::Left));
        assert_eq!(None, tree.find_split_in_direction(l1, Direction::Down));
        assert_eq!(Some(r0), tree.find_split_in_direction(l1, Direction::Right));
        assert_eq!(Some(l0), tree.find_split_in_direction(l1, Direction::Up));

        tree.focus = l0;
        assert_eq!(None, tree.find_split_in_direction(l0, Direction::Left));
        assert_eq!(Some(l1), tree.find_split_in_direction(l0, Direction::Down));
        assert_eq!(Some(l2), tree.find_split_in_direction(l0, Direction::Right));
        assert_eq!(None, tree.find_split_in_direction(l0, Direction::Up));

        tree.focus = r0;
        assert_eq!(Some(l2), tree.find_split_in_direction(r0, Direction::Left));
        assert_eq!(None, tree.find_split_in_direction(r0, Direction::Down));
        assert_eq!(None, tree.find_split_in_direction(r0, Direction::Right));
        assert_eq!(None, tree.find_split_in_direction(r0, Direction::Up));
    }

    #[test]
    fn swap_split_in_direction() {
        let mut tree = Tree::new(Rect {
            x: 0,
            y: 0,
            width: 180,
            height: 80,
        });

        let doc_l0 = DocumentId::default();
        let mut view = View::new(doc_l0, GutterConfig::default());
        view.area = Rect::new(0, 0, 180, 80);
        tree.insert(view);

        let l0 = tree.focus;

        let doc_r0 = DocumentId::default();
        let view = View::new(doc_r0, GutterConfig::default());
        tree.split(view, Layout::Vertical);
        let r0 = tree.focus;

        tree.focus = l0;

        let doc_l1 = DocumentId::default();
        let view = View::new(doc_l1, GutterConfig::default());
        tree.split(view, Layout::Horizontal);
        let l1 = tree.focus;

        tree.focus = l0;

        let doc_l2 = DocumentId::default();
        let view = View::new(doc_l2, GutterConfig::default());
        tree.split(view, Layout::Vertical);
        let l2 = tree.focus;

        // Views in test
        // | L0  | L2 |    |
        // |    L1    | R0 |

        // Document IDs in test
        // | l0  | l2 |    |
        // |    l1    | r0 |

        fn doc_id(tree: &Tree, view_id: ViewId) -> Option<DocumentId> {
            if let Content::View(view) = &tree.nodes[view_id].content {
                Some(view.doc)
            } else {
                None
            }
        }

        tree.focus = l0;
        // `*` marks the view in focus from view table (here L0)
        // | l0*  | l2 |    |
        // |    l1     | r0 |
        tree.swap_split_in_direction(Direction::Down);
        // | l1   | l2 |    |
        // |    l0*    | r0 |
        assert_eq!(tree.focus, l0);
        assert_eq!(doc_id(&tree, l0), Some(doc_l1));
        assert_eq!(doc_id(&tree, l1), Some(doc_l0));
        assert_eq!(doc_id(&tree, l2), Some(doc_l2));
        assert_eq!(doc_id(&tree, r0), Some(doc_r0));

        tree.swap_split_in_direction(Direction::Right);

        // | l1  | l2 |     |
        // |    r0    | l0* |
        assert_eq!(tree.focus, l0);
        assert_eq!(doc_id(&tree, l0), Some(doc_l1));
        assert_eq!(doc_id(&tree, l1), Some(doc_r0));
        assert_eq!(doc_id(&tree, l2), Some(doc_l2));
        assert_eq!(doc_id(&tree, r0), Some(doc_l0));

        // cannot swap, nothing changes
        tree.swap_split_in_direction(Direction::Up);
        // | l1  | l2 |     |
        // |    r0    | l0* |
        assert_eq!(tree.focus, l0);
        assert_eq!(doc_id(&tree, l0), Some(doc_l1));
        assert_eq!(doc_id(&tree, l1), Some(doc_r0));
        assert_eq!(doc_id(&tree, l2), Some(doc_l2));
        assert_eq!(doc_id(&tree, r0), Some(doc_l0));

        // cannot swap, nothing changes
        tree.swap_split_in_direction(Direction::Down);
        // | l1  | l2 |     |
        // |    r0    | l0* |
        assert_eq!(tree.focus, l0);
        assert_eq!(doc_id(&tree, l0), Some(doc_l1));
        assert_eq!(doc_id(&tree, l1), Some(doc_r0));
        assert_eq!(doc_id(&tree, l2), Some(doc_l2));
        assert_eq!(doc_id(&tree, r0), Some(doc_l0));

        tree.focus = l2;
        // | l1  | l2* |    |
        // |    r0     | l0 |

        tree.swap_split_in_direction(Direction::Down);
        // | l1  | r0  |    |
        // |    l2*    | l0 |
        assert_eq!(tree.focus, l2);
        assert_eq!(doc_id(&tree, l0), Some(doc_l1));
        assert_eq!(doc_id(&tree, l1), Some(doc_l2));
        assert_eq!(doc_id(&tree, l2), Some(doc_r0));
        assert_eq!(doc_id(&tree, r0), Some(doc_l0));

        tree.swap_split_in_direction(Direction::Up);
        // | l2* | r0 |    |
        // |    l1    | l0 |
        assert_eq!(tree.focus, l2);
        assert_eq!(doc_id(&tree, l0), Some(doc_l2));
        assert_eq!(doc_id(&tree, l1), Some(doc_l1));
        assert_eq!(doc_id(&tree, l2), Some(doc_r0));
        assert_eq!(doc_id(&tree, r0), Some(doc_l0));
    }

    #[test]
    fn all_vertical_views_have_same_width() {
        let tree_area_width = 180;
        let mut tree = Tree::new(Rect {
            x: 0,
            y: 0,
            width: tree_area_width,
            height: 80,
        });
        let mut view = View::new(DocumentId::default(), GutterConfig::default());
        view.area = Rect::new(0, 0, 180, 80);
        tree.insert(view);

        let view = View::new(DocumentId::default(), GutterConfig::default());
        tree.split(view, Layout::Vertical);

        let view = View::new(DocumentId::default(), GutterConfig::default());
        tree.split(view, Layout::Horizontal);

        tree.remove(tree.focus);

        let view = View::new(DocumentId::default(), GutterConfig::default());
        tree.split(view, Layout::Vertical);

        // Make sure that we only have one level in the tree.
        assert_eq!(3, tree.views().count());
        assert_eq!(
            vec![
                59, 60, 59 // Both separators are excluded; distribute rounding across slots.
            ],
            tree.views()
                .map(|(view, _)| view.area.width)
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn vsplit_gap_rounding() {
        let (tree_area_width, tree_area_height) = (80, 24);
        let mut tree = Tree::new(Rect {
            x: 0,
            y: 0,
            width: tree_area_width,
            height: tree_area_height,
        });
        let mut view = View::new(DocumentId::default(), GutterConfig::default());
        view.area = Rect::new(0, 0, tree_area_width, tree_area_height);
        tree.insert(view);

        for _ in 0..9 {
            let view = View::new(DocumentId::default(), GutterConfig::default());
            tree.split(view, Layout::Vertical);
        }

        assert_eq!(10, tree.views().count());
        assert_eq!(
            vec![7, 7, 7, 7, 8, 7, 7, 7, 7, 7],
            tree.views()
                .map(|(view, _)| view.area.width)
                .collect::<Vec<_>>()
        );
    }
}
