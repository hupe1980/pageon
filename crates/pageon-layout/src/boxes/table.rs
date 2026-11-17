//! Adapted from Blitz (blitz-dom `layout/table.rs`, MIT OR Apache-2.0).
//!
//! Tables laid out on Taffy's grid: cells are grid items, rows and columns grid tracks.

use std::cell::RefCell;
use std::rc::Rc;

use html5ever::local_name;
use style::Atom;
use style::computed_values::border_collapse::T as BorderCollapse;
use style::computed_values::table_layout::T as TableLayout;
use style::properties::style_structs::Border;
use style::servo_arc::Arc as ServoArc;
use style::typed_om::NumericBaseType;
use style::values::computed::calc::CalcPercentageLeaf;
use style::values::computed::length_percentage::{
    CalcLengthPercentage, CalcNode, ComputedLeaf, Unpacked as UnpackedLengthPercentage,
};
use style::values::computed::{BorderStyle, Length, LengthPercentage};
use style::values::generics::Optional;
use style::values::specified::box_::{DisplayInside, DisplayOutside};
use style_traits::values::specified::AllowedNumericType;
use taffy::{
    DetailedGridInfo, LayoutPartialTree as _, ResolveOrZero, TrackSizingFunction, style_helpers,
};

use super::layout::{Pass, resolve_calc_value};
use super::tree::{BoxId, Kind, Tree};

/// A table's grid as Taffy sees it: the table's cells as its children.
pub(crate) struct TableTreeWrapper<'a, 'p> {
    pub(crate) pass: &'a mut Pass<'p>,
    pub(crate) ctx: Rc<TableContext>,
}

/// A table's grid: its cells, rows and columns. Not `Clone`: `style` may hold pointers into
/// `calc_values`.
pub(crate) struct TableContext {
    pub(crate) style: taffy::Style<Atom>,
    pub(crate) cells: Vec<TableCell>,
    /// Absolutely positioned boxes among the table's row groups and rows: no part of the grid.
    pub(crate) oof_children: Vec<(BoxId, taffy::Position)>,
    pub(crate) rows: Vec<TableRow>,
    pub(crate) columns: Vec<TableColumn>,
    pub(crate) computed_grid_info: RefCell<Option<DetailedGridInfo<Atom>>>,
    pub(crate) border_style: Option<ServoArc<Border>>,
    pub(crate) border_collapse: BorderCollapse,
    /// Storage for the `calc()` track sizes made here: Taffy holds pointers to them.
    #[allow(dead_code)]
    calc_values: Vec<LengthPercentage>,
}

#[derive(Debug, Clone)]
pub(crate) struct TableCell {
    pub(crate) node_id: BoxId,
    /// The row (index into `TableContext::rows`) the cell starts in.
    pub(crate) row: u16,
    /// The column the cell starts in.
    pub(crate) column: u16,
    style: taffy::Style<Atom>,
}

/// The column the next cell goes in, as Taffy's dense auto-placement places it (skipping
/// columns still taken by row-spanning cells of earlier rows).
#[derive(Debug, Default)]
struct ColumnCursor {
    col: u16,
    num_columns: u16,
    /// Per column, how many more rows a row-spanning cell takes it.
    rowspans: Vec<u16>,
}

impl ColumnCursor {
    fn start_row(&mut self) {
        self.col = 0;
        for remaining in &mut self.rowspans {
            *remaining = remaining.saturating_sub(1);
        }
    }

    fn next_free(&mut self) -> u16 {
        while self.rowspans.get(self.col as usize).is_some_and(|r| *r > 0) {
            self.col += 1;
        }
        self.col
    }

    fn place(&mut self, colspan: u16, rowspan: u16) {
        let end = self.col + colspan;
        if self.rowspans.len() < end as usize {
            self.rowspans.resize(end as usize, 0);
        }
        for remaining in &mut self.rowspans[self.col as usize..end as usize] {
            *remaining = rowspan - 1;
        }
        self.col = end;
        self.num_columns = self.num_columns.max(end);
    }
}

#[derive(Debug, Clone)]
pub(crate) struct TableColumn {
    pub(crate) node_id: BoxId,
    /// The column group it is in.
    pub(crate) group: Option<BoxId>,
}

#[derive(Debug, Clone)]
pub(crate) struct TableRow {
    pub(crate) node_id: BoxId,
    /// The row group it is in.
    pub(crate) group: Option<BoxId>,
    /// The row's `height`, when a length.
    pub(crate) height: Option<f32>,
    /// The row, or a cell starting and ending in it, has a length `height`: such rows get no
    /// excess table height while unconstrained rows can take it.
    pub(crate) is_constrained: bool,
}

/// The used width of a border side (a `none`/`hidden` border has none).
fn side_width(width: app_units::Au, style: BorderStyle) -> f32 {
    if style.none_or_hidden() {
        0.0
    } else {
        width.to_f32_px()
    }
}

/// A `calc(<percent> + <length>)` track, its value kept in `calc_values`.
fn percent_plus_length(
    calc_values: &mut Vec<LengthPercentage>,
    percent: f32,
    length: f32,
) -> TrackSizingFunction {
    let node = CalcNode::Sum(
        vec![
            CalcNode::Leaf(ComputedLeaf::Percentage(CalcPercentageLeaf::new(
                percent,
                Optional::Some(NumericBaseType::Length),
            ))),
            CalcNode::Leaf(ComputedLeaf::Length(Length::new(length))),
        ]
        .into(),
    );
    let value = LengthPercentage::new_calc(node, AllowedNumericType::NonNegative);
    let dim = match value.unpack() {
        UnpackedLengthPercentage::Calc(calc) => {
            let ptr = calc as *const CalcLengthPercentage as *const ();
            // SAFETY: the pointer is to the heap allocation `value` owns, kept alive in
            // `calc_values` for as long as the table context (and its style) exists; Taffy
            // hands it back to `resolve_calc_value`, which reads it as that type.
            #[allow(unsafe_code)]
            unsafe {
                taffy::Dimension::from_raw(taffy::CompactLength::calc(ptr))
            }
        }
        UnpackedLengthPercentage::Length(len) => style_helpers::length(len.px()),
        UnpackedLengthPercentage::Percentage(p) => style_helpers::percent(p.0),
    };
    calc_values.push(value);
    dim.into()
}

fn table_taffy_style(style: &style::properties::ComputedValues) -> taffy::Style<Atom> {
    stylo_taffy::to_taffy_style_in(style, style::logical_geometry::WritingMode::empty())
}

/// The grid of the table `root`, and its layout children (cells, then out-of-flow boxes).
pub(crate) fn build_table_context(tree: &Tree, root: BoxId) -> (TableContext, Vec<BoxId>) {
    let mut cells: Vec<TableCell> = Vec::new();
    let mut oof_children: Vec<(BoxId, taffy::Position)> = Vec::new();
    let mut rows: Vec<TableRow> = Vec::new();
    let mut row = 0u16;
    let mut cursor = ColumnCursor::default();

    let children = tree[root].children.clone();
    let Some(stylo_styles) = tree[root].primary_styles() else {
        panic!("a table without style");
    };
    let mut style = table_taffy_style(stylo_styles);
    style.item_is_table = true;
    // Dense row flow: each cell takes the first free column of its row, so cells of later rows
    // fill columns freed by row-spanning cells.
    style.grid_auto_flow = taffy::GridAutoFlow::RowDense;
    style.grid_auto_columns = Vec::new();
    style.grid_auto_rows = Vec::new();

    // The fixed table layout needs a width.
    let is_fixed = match stylo_styles.clone_table_layout() {
        TableLayout::Fixed => !style.size.width.is_auto(),
        TableLayout::Auto => false,
    };
    let border_collapse = stylo_styles.clone_border_collapse();
    let border_spacing = stylo_styles.clone_border_spacing().0;

    let mut columns: Vec<TableColumn> = Vec::new();
    let mut column_sizes: Vec<TrackSizingFunction> = Vec::new();
    for &c in &children {
        collect_columns(tree, c, &mut columns, &mut column_sizes);
    }
    // Percentage column widths apply in the fixed layout only.
    if !is_fixed {
        for column in &mut column_sizes {
            if column.max.into_raw().tag() == taffy::CompactLength::PERCENT_TAG {
                *column = style_helpers::auto();
            }
        }
    }
    let mut first_cell_border: Option<ServoArc<Border>> = None;
    // Percentage widths on first-row cells: (column, percentage, padding + border).
    let mut percent_columns: Vec<(u16, f32, f32)> = Vec::new();
    let mut calc_values: Vec<LengthPercentage> = Vec::new();
    // Header groups first, footer groups last.
    let row_group_order = |c: BoxId| -> u8 {
        match tree[c].primary_styles().map(|s| s.clone_display().inside()) {
            Some(DisplayInside::TableHeaderGroup) => 0,
            Some(DisplayInside::TableFooterGroup) => 2,
            _ => 1,
        }
    };
    let mut cx = Cells {
        is_fixed,
        border_collapse,
        row: &mut row,
        cursor: &mut cursor,
        cells: &mut cells,
        oof_children: &mut oof_children,
        rows: &mut rows,
        columns: &mut column_sizes,
        first_cell_border: &mut first_cell_border,
        percent_columns: &mut percent_columns,
    };
    for order in 0..3 {
        for &c in &children {
            if row_group_order(c) == order {
                collect_table_cells(tree, c, &mut cx);
            }
        }
    }
    let remaining_column = if is_fixed {
        style_helpers::minmax(style_helpers::length(0.0), style_helpers::fr(1.0))
    } else {
        style_helpers::auto()
    };
    // Trailing `<col>` columns without cells or a definite width are dropped (no spacing).
    while column_sizes.len() > cursor.num_columns as usize
        && column_sizes
            .last()
            .is_some_and(|c| c.min.into_raw().tag() == taffy::CompactLength::AUTO_TAG)
    {
        column_sizes.pop();
        columns.pop();
    }
    let num_columns = cursor.num_columns.max(column_sizes.len() as u16);
    column_sizes.resize(num_columns as usize, remaining_column);
    if is_fixed {
        // Percentages resolve against the content width less the spacing between columns;
        // Taffy resolves them against the inner width, so that spacing is folded in as
        // `calc(N% - N * spacing)`.
        let spacing_x = match border_collapse {
            BorderCollapse::Separate => border_spacing.width.px(),
            BorderCollapse::Collapse => 0.0,
        };
        let inner_spacing = spacing_x * num_columns.saturating_sub(1) as f32;
        let mut percent_track = |percent: f32, extra: f32| -> TrackSizingFunction {
            let extra = extra - percent * inner_spacing;
            if extra == 0.0 {
                style_helpers::percent(percent)
            } else {
                percent_plus_length(&mut calc_values, percent, extra)
            }
        };
        for column in &mut column_sizes {
            if column.max.is_auto() {
                *column = remaining_column;
            } else if column.max.into_raw().tag() == taffy::CompactLength::PERCENT_TAG {
                *column = percent_track(column.max.into_raw().value(), 0.0);
            }
        }
        // A cell's percentage width is of its content box: its padding and border are added.
        for (col, percent, extra) in percent_columns {
            if let Some(column) = column_sizes.get_mut(col as usize) {
                *column = percent_track(percent, extra);
            }
        }
    }

    style.grid_template_columns = column_sizes.into_iter().map(Into::into).collect();
    // A row's `height` is a minimum. Excess table height goes to unconstrained rows if any
    // (so constrained rows are `max-content` at most then), else to all rows.
    let has_unconstrained_rows = rows.iter().any(|r| !r.is_constrained);
    style.grid_template_rows = rows
        .iter()
        .map(|r| {
            let min = match r.height {
                Some(h) => style_helpers::length(h),
                None => style_helpers::auto(),
            };
            let max = if r.is_constrained && has_unconstrained_rows {
                style_helpers::max_content()
            } else {
                style_helpers::auto()
            };
            style_helpers::minmax(min, max)
        })
        .collect();

    style.gap = match border_collapse {
        BorderCollapse::Separate => {
            // Separated borders: `border-spacing` also lies between the table's border and the
            // outer cells.
            let spacing_x = border_spacing.width.px();
            let spacing_y = border_spacing.height.px();
            let padding = style.padding.resolve_or_zero(None, resolve_calc_value);
            style.padding = taffy::Rect {
                left: style_helpers::length(padding.left + spacing_x),
                right: style_helpers::length(padding.right + spacing_x),
                top: style_helpers::length(padding.top + spacing_y),
                bottom: style_helpers::length(padding.bottom + spacing_y),
            };
            taffy::Size {
                width: style_helpers::length(spacing_x),
                height: style_helpers::length(spacing_y),
            }
        }
        BorderCollapse::Collapse => first_cell_border
            .as_ref()
            .map(|border| {
                let x = side_width(border.border_left_width.0, border.border_left_style).max(
                    side_width(border.border_right_width.0, border.border_right_style),
                );
                let y = side_width(border.border_top_width.0, border.border_top_style).max(
                    side_width(border.border_bottom_width.0, border.border_bottom_style),
                );
                taffy::Size {
                    width: style_helpers::length(x),
                    height: style_helpers::length(y),
                }
            })
            .unwrap_or(taffy::Size::ZERO.map(style_helpers::length)),
    };
    if border_collapse == BorderCollapse::Collapse {
        style.border = taffy::Rect {
            left: style.gap.width,
            right: style.gap.width,
            top: style.gap.height,
            bottom: style.gap.height,
        };
    }

    let layout_children = cells
        .iter()
        .map(|c| c.node_id)
        .chain(oof_children.iter().map(|(id, _)| *id))
        .collect();
    (
        TableContext {
            style,
            cells,
            oof_children,
            rows,
            columns,
            computed_grid_info: RefCell::new(None),
            border_collapse,
            border_style: first_cell_border,
            calc_values,
        },
        layout_children,
    )
}

/// `<col>` (and `display: table-column`) boxes with their widths, which win over cell widths
/// in the fixed layout; columns without one are `auto`.
fn collect_columns(
    tree: &Tree,
    id: BoxId,
    columns: &mut Vec<TableColumn>,
    column_sizes: &mut Vec<TrackSizingFunction>,
) {
    let n = &tree[id];
    if n.kind != Kind::Element {
        return;
    }
    let Some(display) = n.display_style() else {
        return;
    };
    match display.inside() {
        DisplayInside::TableColumnGroup => {
            let first = columns.len();
            for &c in &n.children {
                collect_columns(tree, c, columns, column_sizes);
            }
            for column in &mut columns[first..] {
                column.group.get_or_insert(id);
            }
        }
        DisplayInside::TableColumn => {
            let Some(s) = n.primary_styles() else { return };
            let style = table_taffy_style(s);
            let span: u16 = n
                .attr(&local_name!("span"))
                .and_then(|v| v.parse::<u16>().ok())
                .map(|v| v.max(1))
                .unwrap_or(1);
            let column: TrackSizingFunction = match style.size.width.tag() {
                taffy::CompactLength::LENGTH_TAG => {
                    // A definite `max-width` clamps it; zero is `auto`.
                    let mut width = style.size.width.value();
                    if style.max_size.width.into_raw().tag() == taffy::CompactLength::LENGTH_TAG {
                        width = width.min(style.max_size.width.into_raw().value());
                    }
                    if width > 0.0 {
                        style_helpers::length(width)
                    } else {
                        style_helpers::auto()
                    }
                }
                taffy::CompactLength::PERCENT_TAG => {
                    style_helpers::percent(style.size.width.value())
                }
                // `calc()` widths on columns are `auto`, as in browsers.
                _ => style_helpers::auto(),
            };
            // A definite `min-width` is a floor.
            let column = match style.min_size.width.into_raw().tag() {
                taffy::CompactLength::LENGTH_TAG if column.max.is_auto() => style_helpers::minmax(
                    style_helpers::length(style.min_size.width.into_raw().value()),
                    style_helpers::auto(),
                ),
                _ => column,
            };
            for _ in 0..span {
                columns.push(TableColumn {
                    node_id: id,
                    group: None,
                });
                column_sizes.push(column);
            }
        }
        _ => {}
    }
}

/// What collecting a table's cells builds up.
struct Cells<'a> {
    is_fixed: bool,
    border_collapse: BorderCollapse,
    row: &'a mut u16,
    cursor: &'a mut ColumnCursor,
    cells: &'a mut Vec<TableCell>,
    oof_children: &'a mut Vec<(BoxId, taffy::Position)>,
    rows: &'a mut Vec<TableRow>,
    columns: &'a mut Vec<TrackSizingFunction>,
    first_cell_border: &'a mut Option<ServoArc<Border>>,
    percent_columns: &'a mut Vec<(u16, f32, f32)>,
}

fn collect_table_cells(tree: &Tree, id: BoxId, cx: &mut Cells<'_>) {
    let n = &tree[id];
    if n.kind != Kind::Element {
        return;
    }
    let Some(display) = n.display_style() else {
        return;
    };
    if display.outside() == DisplayOutside::None {
        return;
    }
    let Some(stylo_style) = n.primary_styles() else {
        return;
    };
    // Absolutely positioned boxes are laid out by their containing block, not the grid.
    let position = stylo_style.clone_position();
    if position.is_absolutely_positioned() {
        cx.oof_children
            .push((id, stylo_taffy::convert::position(position)));
        return;
    }
    match display.inside() {
        DisplayInside::TableRowGroup
        | DisplayInside::TableHeaderGroup
        | DisplayInside::TableFooterGroup
        | DisplayInside::Contents => {
            let first_row = cx.rows.len();
            for &c in &n.children {
                collect_table_cells(tree, c, cx);
            }
            if display.inside() != DisplayInside::Contents {
                for row in &mut cx.rows[first_row..] {
                    row.group.get_or_insert(id);
                }
            }
        }
        DisplayInside::TableRow => {
            *cx.row += 1;
            cx.cursor.start_row();
            let height = {
                let h = stylo_taffy::convert::dimension(&stylo_style.clone_height());
                (h.tag() == taffy::CompactLength::LENGTH_TAG).then(|| h.value())
            };
            cx.rows.push(TableRow {
                node_id: id,
                group: None,
                height,
                is_constrained: height.is_some(),
            });
            for &c in &n.children {
                collect_table_cells(tree, c, cx);
            }
        }
        DisplayInside::TableCell => {
            let colspan: u16 = n
                .attr(&local_name!("colspan"))
                .and_then(|v| v.parse().ok())
                .unwrap_or(1);
            let rowspan: u16 = n
                .attr(&local_name!("rowspan"))
                .and_then(|v| v.parse::<u16>().ok())
                .map(|v| v.clamp(1, 65534))
                .unwrap_or(1);
            let mut style = table_taffy_style(stylo_style);
            let col = cx.cursor.next_free();
            // Collapsed borders are laid out as gutters between the cells (the table's gap).
            if cx.border_collapse == BorderCollapse::Collapse {
                style.border = taffy::Rect::ZERO.map(style_helpers::length);
            }
            if cx.first_cell_border.is_none() {
                *cx.first_cell_border = Some(stylo_style.clone_border());
            }
            // In the fixed layout cell contents do not size the columns.
            if cx.is_fixed {
                style.min_size.width = style_helpers::length(0.0);
            }
            // `<col>` widths win over cell widths.
            let col_needs_width =
                (col as usize) >= cx.columns.len() || cx.columns[col as usize].max.is_auto();
            if *cx.row == 1 && col_needs_width {
                let pb = |style: &taffy::Style<Atom>| {
                    let padding = style.padding.resolve_or_zero(None, resolve_calc_value);
                    let border = style.border.resolve_or_zero(None, resolve_calc_value);
                    padding.left + padding.right + border.left + border.right
                };
                let column: TrackSizingFunction = match style.size.width.tag() {
                    taffy::CompactLength::LENGTH_TAG => {
                        let len = style.size.width.value();
                        match style.box_sizing {
                            taffy::BoxSizing::ContentBox => style_helpers::length(len + pb(&style)),
                            taffy::BoxSizing::BorderBox => style_helpers::length(len),
                        }
                    }
                    taffy::CompactLength::PERCENT_TAG => {
                        if cx.is_fixed {
                            let extra = match style.box_sizing {
                                taffy::BoxSizing::ContentBox => pb(&style),
                                taffy::BoxSizing::BorderBox => 0.0,
                            };
                            // Resolved once the column count is known.
                            cx.percent_columns
                                .push((col, style.size.width.value(), extra));
                            style_helpers::percent(style.size.width.value())
                        } else {
                            style_helpers::auto()
                        }
                    }
                    taffy::CompactLength::AUTO_TAG => style_helpers::auto(),
                    // Otherwise a `calc()`: Taffy resolves it against the table's inner width.
                    _ => style.size.width.into(),
                };
                if (col as usize) < cx.columns.len() {
                    if !column.max.is_auto() {
                        cx.columns[col as usize] = column;
                    }
                } else {
                    cx.columns.resize(col as usize, style_helpers::auto());
                    cx.columns.push(column);
                }
            }
            // Margins do not apply to table-internal boxes.
            style.margin = taffy::Rect::ZERO.map(style_helpers::length);
            // Taffy places the column (dense row flow skips columns taken by row spans).
            style.grid_column = taffy::Line {
                start: style_helpers::auto(),
                end: style_helpers::span(colspan),
            };
            style.grid_row = taffy::Line {
                start: style_helpers::line(*cx.row as i16),
                end: style_helpers::span(rowspan),
            };
            if rowspan == 1
                && style.size.height.tag() == taffy::CompactLength::LENGTH_TAG
                && let Some(row) = cx.rows.last_mut()
            {
                row.is_constrained = true;
            }
            style.size.width = style_helpers::auto();
            cx.cells.push(TableCell {
                node_id: id,
                row: cx.row.saturating_sub(1),
                column: col,
                style,
            });
            cx.cursor.place(colspan, rowspan);
        }
        // Captions and other boxes in a table: ignored.
        _ => {}
    }
}

pub(crate) struct RangeIter(std::ops::Range<usize>);

impl Iterator for RangeIter {
    type Item = taffy::NodeId;

    fn next(&mut self) -> Option<Self::Item> {
        self.0.next().map(taffy::NodeId::from)
    }
}

impl taffy::TraversePartialTree for TableTreeWrapper<'_, '_> {
    type ChildIter<'a>
        = RangeIter
    where
        Self: 'a;

    fn child_ids(&self, _node_id: taffy::NodeId) -> Self::ChildIter<'_> {
        RangeIter(0..self.ctx.cells.len())
    }

    fn child_count(&self, _node_id: taffy::NodeId) -> usize {
        self.ctx.cells.len()
    }

    fn get_child_id(&self, _node_id: taffy::NodeId, index: usize) -> taffy::NodeId {
        index.into()
    }
}

impl taffy::TraverseTree for TableTreeWrapper<'_, '_> {}

impl taffy::LayoutPartialTree for TableTreeWrapper<'_, '_> {
    type CoreContainerStyle<'a>
        = &'a taffy::Style<Atom>
    where
        Self: 'a;

    type CustomIdent = Atom;

    fn get_core_container_style(&self, _node_id: taffy::NodeId) -> &taffy::Style<Atom> {
        &self.ctx.style
    }

    fn resolve_calc_value(&self, calc_ptr: *const (), parent_size: f32) -> f32 {
        resolve_calc_value(calc_ptr, parent_size)
    }

    fn set_unrounded_layout(&mut self, node_id: taffy::NodeId, layout: &taffy::Layout) {
        let id = taffy::NodeId::from(self.ctx.cells[usize::from(node_id)].node_id);
        self.pass.set_unrounded_layout(id, layout);
    }

    fn compute_child_layout(
        &mut self,
        node_id: taffy::NodeId,
        inputs: taffy::tree::LayoutInput,
    ) -> taffy::LayoutOutput {
        let id = taffy::NodeId::from(self.ctx.cells[usize::from(node_id)].node_id);
        self.pass.compute_child_layout(id, inputs)
    }
}

impl taffy::LayoutGridContainer for TableTreeWrapper<'_, '_> {
    type GridContainerStyle<'a>
        = &'a taffy::Style<Atom>
    where
        Self: 'a;

    type GridItemStyle<'a>
        = &'a taffy::Style<Atom>
    where
        Self: 'a;

    fn get_grid_container_style(&self, node_id: taffy::NodeId) -> Self::GridContainerStyle<'_> {
        self.get_core_container_style(node_id)
    }

    fn get_grid_child_style(&self, child_node_id: taffy::NodeId) -> Self::GridItemStyle<'_> {
        &self.ctx.cells[usize::from(child_node_id)].style
    }

    fn set_detailed_grid_info(&mut self, _node_id: taffy::NodeId, info: DetailedGridInfo<Atom>) {
        *self.ctx.computed_grid_info.borrow_mut() = Some(info);
    }
}
