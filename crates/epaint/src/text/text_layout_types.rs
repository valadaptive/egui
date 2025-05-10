#![allow(clippy::derived_hash_with_manual_eq)] // We need to impl Hash for f32, but we don't implement Eq, which is fine
#![allow(clippy::wrong_self_convention)] // We use `from_` to indicate conversion direction. It's non-diomatic, but makes sense in this context.

use std::ops::Range;
use std::sync::Arc;

use super::cursor::{Affinity, AnchorBase, ByteCursor, Selection};
use super::glyph_atlas::UvRect;
use super::style::{FontId, TextFormat};
use crate::{mutex::Mutex, Color32, Mesh};
use emath::{vec2, Align, OrderedFloat, Pos2, Rect, Vec2};
use parley::OverflowWrap;

/// Describes the task of laying out text.
///
/// This supports mixing different fonts, color and formats (underline etc).
///
/// Pass this to [`crate::Fonts::layout_job`] or [`crate::text::layout`].
///
/// ## Example:
/// ```
/// use epaint::{Color32, text::{LayoutJob, TextStyle}, FontFamily, FontId};
///
/// let mut job = LayoutJob::default();
/// job.append(
///     "Hello ",
///     0.0,
///     TextStyle {
///         font_id: FontId::new(14.0, FontFamily::Proportional),
///         color: Color32::WHITE,
///         ..Default::default()
///     },
/// );
/// job.append(
///     "World!",
///     0.0,
///     TextStyle {
///         font_id: FontId::new(14.0, FontFamily::Monospace),
///         color: Color32::BLACK,
///         ..Default::default()
///     },
/// );
/// ```
///
/// As you can see, constructing a [`LayoutJob`] is currently a lot of work.
/// It would be nice to have a helper macro for it!
#[derive(Clone, Debug, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Deserialize, serde::Serialize))]
pub struct LayoutJob {
    /// The complete text of this job, referenced by [`LayoutSection`].
    pub text: String,

    /// The different section, which can have different fonts, colors, etc.
    pub sections: Vec<LayoutSection>,

    /// Controls the text wrapping and elision.
    pub wrap: TextWrapping,

    /// The first row must be at least this high.
    /// This is in case we lay out text that is the continuation
    /// of some earlier text (sharing the same row),
    /// in which case this will be the height of the earlier text.
    /// In other cases, set this to `0.0`.
    pub first_row_min_height: f32,

    /// If `true`, all `\n` characters will result in a new _paragraph_,
    /// starting on a new row.
    ///
    /// If `false`, all `\n` characters will be ignored
    /// and show up as the replacement character.
    ///
    /// Default: `true`.
    /// TODO(valadaptive): implement this
    pub break_on_newline: bool,

    /// How to horizontally align the text (`Align::LEFT`, `Align::Center`, `Align::RIGHT`).
    pub halign: Align,

    /// Justify text so that word-wrapped rows fill the whole [`TextWrapping::max_width`].
    pub justify: bool,

    /// Round output sizes using [`emath::GuiRounding`], to avoid rounding errors in layout code.
    pub round_output_to_gui: bool,
}

impl Default for LayoutJob {
    #[inline]
    fn default() -> Self {
        Self {
            text: Default::default(),
            sections: Default::default(),
            wrap: Default::default(),
            first_row_min_height: 0.0,
            break_on_newline: true,
            halign: Align::LEFT,
            justify: false,
            round_output_to_gui: true,
        }
    }
}

impl LayoutJob {
    /// Break on `\n` and at the given wrap width.
    #[inline]
    pub fn simple(text: String, font: FontId, color: Color32, wrap_width: f32) -> Self {
        Self {
            sections: vec![LayoutSection {
                leading_space: 0.0,
                byte_range: 0..text.len(),
                format: TextFormat::simple(font, color),
            }],
            text,
            wrap: TextWrapping {
                max_width: wrap_width,
                ..Default::default()
            },
            break_on_newline: true,
            ..Default::default()
        }
    }

    /// Break on `\n`
    #[inline]
    pub fn simple_format(text: String, format: TextFormat) -> Self {
        Self {
            sections: vec![LayoutSection {
                leading_space: 0.0,
                byte_range: 0..text.len(),
                format,
            }],
            text,
            break_on_newline: true,
            ..Default::default()
        }
    }

    /// Does not break on `\n`, but shows the replacement character instead.
    #[inline]
    pub fn simple_singleline(text: String, font: FontId, color: Color32) -> Self {
        Self {
            sections: vec![LayoutSection {
                leading_space: 0.0,
                byte_range: 0..text.len(),
                format: TextFormat::simple(font, color),
            }],
            text,
            wrap: Default::default(),
            break_on_newline: false,
            ..Default::default()
        }
    }

    #[inline]
    pub fn single_section(text: String, format: TextFormat) -> Self {
        Self {
            sections: vec![LayoutSection {
                leading_space: 0.0,
                byte_range: 0..text.len(),
                format,
            }],
            text,
            wrap: Default::default(),
            break_on_newline: true,
            ..Default::default()
        }
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.sections.is_empty()
    }

    /// Helper for adding a new section when building a [`LayoutJob`].
    pub fn append(&mut self, text: &str, leading_space: f32, format: TextFormat) {
        let start = self.text.len();
        self.text += text;
        let byte_range = start..self.text.len();
        self.sections.push(LayoutSection {
            leading_space,
            byte_range,
            format,
        });
    }

    /// The height of the tallest font used in the job.
    ///
    /// Returns a value rounded to [`emath::GUI_ROUNDING`].
    pub fn font_height(&self, fonts: &mut crate::Fonts<'_>) -> f32 {
        let mut max_height = 0.0_f32;
        for section in &self.sections {
            max_height = max_height.max(fonts.row_height(&section.format.font_id));
        }
        max_height
    }

    /// The wrap with, with a small margin in some cases.
    pub fn effective_wrap_width(&self) -> f32 {
        if self.round_output_to_gui {
            // On a previous pass we may have rounded down by at most 0.5 and reported that as a width.
            // egui may then set that width as the max width for subsequent frames, and it is important
            // that we then don't wrap earlier.
            self.wrap.max_width + 0.5
        } else {
            self.wrap.max_width
        }
    }
}

impl std::hash::Hash for LayoutJob {
    #[inline]
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        let Self {
            text,
            sections,
            wrap,
            first_row_min_height,
            break_on_newline,
            halign,
            justify,
            round_output_to_gui,
        } = self;

        text.hash(state);
        sections.hash(state);
        wrap.hash(state);
        emath::OrderedFloat(*first_row_min_height).hash(state);
        break_on_newline.hash(state);
        halign.hash(state);
        justify.hash(state);
        round_output_to_gui.hash(state);
    }
}

// ----------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Deserialize, serde::Serialize))]
pub struct LayoutSection {
    /// Can be used for first row indentation.
    pub leading_space: f32,

    /// Range into the galley text
    pub byte_range: Range<usize>,

    pub format: TextFormat,
}

impl std::hash::Hash for LayoutSection {
    #[inline]
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        let Self {
            leading_space,
            byte_range,
            format: style,
        } = self;
        OrderedFloat(*leading_space).hash(state);
        byte_range.hash(state);
        style.hash(state);
    }
}

// ----------------------------------------------------------------------------

/// How to wrap and elide text.
///
/// This enum is used in high-level APIs where providing a [`TextWrapping`] is too verbose.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Deserialize, serde::Serialize))]
pub enum TextWrapMode {
    /// The text should expand the `Ui` size when reaching its boundary.
    Extend,

    /// The text should wrap to the next line when reaching the `Ui` boundary.
    Wrap,

    /// The text should be elided using "…" when reaching the `Ui` boundary.
    ///
    /// Note that using [`TextWrapping`] and [`LayoutJob`] offers more control over the elision.
    Truncate,
}

/// Controls the text wrapping and elision of a [`LayoutJob`].
#[derive(Clone, Debug, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Deserialize, serde::Serialize))]
pub struct TextWrapping {
    /// Wrap text so that no row is wider than this.
    ///
    /// If you would rather truncate text that doesn't fit, set [`Self::max_rows`] to `1`.
    ///
    /// Set `max_width` to [`f32::INFINITY`] to turn off wrapping and elision.
    ///
    /// Note that `\n` always produces a new row
    /// if [`LayoutJob::break_on_newline`] is `true`.
    pub max_width: f32,

    /// Maximum amount of rows the text galley should have.
    ///
    /// If this limit is reached, text will be truncated
    /// and [`Self::overflow_character`] appended to the final row.
    /// You can detect this by checking [`Galley::elided`].
    ///
    /// If set to `0`, no text will be outputted.
    ///
    /// If set to `1`, a single row will be outputted,
    /// eliding the text after [`Self::max_width`] is reached.
    /// When you set `max_rows = 1`, it is recommended you also set [`Self::break_anywhere`] to `true`.
    ///
    /// Default value: `usize::MAX`.
    pub max_rows: usize,

    /// If `true`: Allow breaking between any characters.
    /// If `false` (default): prefer breaking between words, etc.
    ///
    /// NOTE: Due to limitations in the current implementation,
    /// when truncating text using [`Self::max_rows`] the text may be truncated
    /// in the middle of a word even if [`Self::break_anywhere`] is `false`.
    /// Therefore it is recommended to set [`Self::break_anywhere`] to `true`
    /// whenever [`Self::max_rows`] is set to `1`.
    pub break_anywhere: bool,

    /// Character to use to represent elided text.
    ///
    /// The default is `…`.
    ///
    /// If not set, no character will be used (but the text will still be elided).
    pub overflow_character: Option<char>,
}

impl std::hash::Hash for TextWrapping {
    #[inline]
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        let Self {
            max_width,
            max_rows,
            break_anywhere,
            overflow_character,
        } = self;
        emath::OrderedFloat(*max_width).hash(state);
        max_rows.hash(state);
        break_anywhere.hash(state);
        overflow_character.hash(state);
    }
}

impl Default for TextWrapping {
    fn default() -> Self {
        Self {
            max_width: f32::INFINITY,
            max_rows: usize::MAX,
            break_anywhere: false,
            overflow_character: Some('…'),
        }
    }
}

impl TextWrapping {
    /// Create a [`TextWrapping`] from a [`TextWrapMode`] and an available width.
    pub fn from_wrap_mode_and_width(mode: TextWrapMode, max_width: f32) -> Self {
        match mode {
            TextWrapMode::Extend => Self::no_max_width(),
            TextWrapMode::Wrap => Self::wrap_at_width(max_width),
            TextWrapMode::Truncate => Self::truncate_at_width(max_width),
        }
    }

    /// A row can be as long as it need to be.
    pub fn no_max_width() -> Self {
        Self {
            max_width: f32::INFINITY,
            ..Default::default()
        }
    }

    /// A row can be at most `max_width` wide but can wrap in any number of lines.
    pub fn wrap_at_width(max_width: f32) -> Self {
        Self {
            max_width,
            ..Default::default()
        }
    }

    /// Elide text that doesn't fit within the given width, replaced with `…`.
    pub fn truncate_at_width(max_width: f32) -> Self {
        Self {
            max_width,
            max_rows: 1,
            break_anywhere: true,
            ..Default::default()
        }
    }

    pub(crate) fn apply_to_parley_style(&self, style: &mut parley::TextStyle<'_, Color32>) {
        style.overflow_wrap = OverflowWrap::Anywhere;
        style.word_break = if self.break_anywhere {
            parley::WordBreakStrength::BreakAll
        } else {
            parley::WordBreakStrength::Normal
        };
    }
}

#[derive(Clone, Default)]
pub(super) struct LayoutAndOffset {
    // This needs to be wrapped in a Mutex because Shape stores it and must be Send+Sync
    pub(super) layout: Mutex<parley::Layout<Color32>>,
    /// Position offset added to the Parley layout. Must be subtracted again to
    /// translate coords into Parley-space.
    pub(super) offset: Vec2,
}

impl LayoutAndOffset {
    pub(super) fn new(layout: parley::Layout<Color32>, offset: Vec2) -> Self {
        Self {
            layout: Mutex::new(layout),
            offset,
        }
    }
}

// ----------------------------------------------------------------------------

/// Text that has been laid out, ready for painting.
///
/// You can create a [`Galley`] using [`crate::Fonts::layout_job`];
///
/// Needs to be recreated if the underlying font atlas texture changes, which
/// happens under the following conditions:
/// - `pixels_per_point` or `max_texture_size` change. These parameters are set
///   in [`crate::text::Fonts::begin_pass`]. When using `egui` they are set
///   from `egui::InputState` and can change at any time.
/// - The atlas has become full. This can happen any time a new glyph is added
///   to the atlas, which in turn can happen any time new text is laid out.
///
/// The name comes from typography, where a "galley" is a metal tray
/// containing a column of set type, usually the size of a page of text.
#[derive(Clone)]
#[cfg_attr(feature = "serde", derive(serde::Deserialize, serde::Serialize))]
pub struct Galley {
    /// The job that this galley is the result of.
    /// Contains the original string and style sections.
    pub job: Arc<LayoutJob>,

    /// Rows of text, from top to bottom.
    ///
    /// The number of characters in all rows sum up to `job.text.chars().count()`
    /// unless [`Self::elided`] is `true`.
    ///
    /// Note that a paragraph (a piece of text separated with `\n`)
    /// can be split up into multiple rows.
    pub sections: Vec<PlacedSection>,

    /// Geometry for any selection to be painted, by row.
    pub selection_rects: Option<Vec<Vec<Rect>>>,

    /// Color of the selection, if it exists. Otherwise, [`Color32::TRANSPARENT`].
    pub selection_color: Color32,

    /// Set to true the text was truncated due to [`TextWrapping::max_rows`].
    pub elided: bool,

    /// Bounding rect.
    ///
    /// `rect.top()` is always 0.0.
    ///
    /// With [`LayoutJob::halign`]:
    /// * [`Align::LEFT`]: `rect.left() == 0.0`
    /// * [`Align::Center`]: `rect.center() == 0.0`
    /// * [`Align::RIGHT`]: `rect.right() == 0.0`
    pub rect: Rect,

    /// Tight bounding box around all the meshes in all the rows.
    /// Can be used for culling.
    pub mesh_bounds: Rect,

    /// Total number of vertices in all the row meshes.
    pub num_vertices: usize,

    /// Total number of indices in all the row meshes.
    pub num_indices: usize,

    /// The number of physical pixels for each logical point.
    /// Since this affects the layout, we keep track of it
    /// so that we can warn if this has changed once we get to
    /// tessellation.
    pub pixels_per_point: f32,
}

// parley::Layout does not implement Debug
impl std::fmt::Debug for Galley {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Galley")
            .field("job", &self.job)
            .field("elided", &self.elided)
            .field("rect", &self.rect)
            .field("mesh_bounds", &self.mesh_bounds)
            .field("num_vertices", &self.num_vertices)
            .field("num_indices", &self.num_indices)
            .field("pixels_per_point", &self.pixels_per_point)
            .finish()
    }
}

#[cfg(feature = "accesskit")]
#[derive(Clone)]
pub struct GalleyAccessibility {
    layout_access: parley::LayoutAccessibility,
    pub nodes: Vec<(accesskit::NodeId, accesskit::Node)>,
}

#[cfg(feature = "accesskit")]
#[derive(Default, Clone)]
pub(super) struct LazyAccessibility(std::sync::OnceLock<GalleyAccessibility>);

#[cfg(feature = "accesskit")]
impl LazyAccessibility {
    fn get_or_init(
        &self,
        text: &str,
        layout: &parley::Layout<Color32>,
        layout_offset: Vec2,
    ) -> &GalleyAccessibility {
        self.0.get_or_init(|| {
            // TODO(valadaptive): this is quite janky since parley expects to be
            // able to directly write to a TreeUpdate. Ask if there's a better
            // way to do this.
            let nodes = Vec::new();
            let mut tree_update = accesskit::TreeUpdate {
                nodes,
                tree: None,
                focus: accesskit::NodeId(0), // TODO(valadaptive): does this need to be a "real" value?
            };
            let mut parent_node = accesskit::Node::new(accesskit::Role::Unknown);
            let mut id_counter = 0;
            let mut next_node_id = || {
                id_counter += 1;
                accesskit::NodeId(id_counter)
            };

            let mut layout_access = parley::LayoutAccessibility::default();
            layout_access.build_nodes(
                text,
                layout,
                &mut tree_update,
                &mut parent_node,
                &mut next_node_id,
                layout_offset.x as f64,
                layout_offset.y as f64,
            );

            GalleyAccessibility {
                layout_access,
                nodes: tree_update.nodes,
            }
        })
    }
}

#[derive(Clone)]
#[cfg_attr(feature = "serde", derive(serde::Deserialize, serde::Serialize))]
pub struct GalleySection {
    pub rows: Vec<Row>,

    pub height: f32,

    /// Parley text layout, primarily used for text editing.
    /// TODO(valadaptive): serde support?
    #[cfg_attr(feature = "serde", serde(skip))]
    pub(super) parley_layout: LayoutAndOffset,

    /// Optional extra layout for the character used to truncate this text.
    /// Boxed because it's pretty large and most text is not truncated.
    ///
    /// TODO(valadaptive): use this to test whether the truncation character is
    /// selected.
    #[cfg_attr(feature = "serde", serde(skip))]
    #[allow(dead_code)]
    pub(super) overflow_char_layout: Option<Box<LayoutAndOffset>>,

    /// Lazy-initialized AccessKit adapter for this galley's layout.
    #[cfg(feature = "accesskit")]
    #[cfg_attr(feature = "serde", serde(skip))]
    pub(super) accessibility: LazyAccessibility,

    /// Set to true the text was truncated due to [`TextWrapping::max_rows`].
    pub elided: bool,

    /// Tight bounding box around all the meshes in all the rows.
    /// Can be used for culling.
    pub mesh_bounds: Rect,

    /// Total number of vertices in all the row meshes.
    pub num_vertices: usize,

    /// Total number of indices in all the row meshes.
    pub num_indices: usize,
}

impl Default for GalleySection {
    fn default() -> Self {
        Self {
            rows: Default::default(),
            height: 0.0,
            parley_layout: Default::default(),
            overflow_char_layout: Default::default(),
            #[cfg(feature = "accesskit")]
            accessibility: Default::default(),
            elided: Default::default(),
            mesh_bounds: Rect::ZERO,
            num_vertices: Default::default(),
            num_indices: Default::default(),
        }
    }
}

#[derive(Clone)]
#[cfg_attr(feature = "serde", derive(serde::Deserialize, serde::Serialize))]
pub struct PlacedSection {
    pub index: usize,
    pub section: Arc<GalleySection>,
    /// Y position of the start of the section
    pub y_start: f32,
    pub row_range: Range<usize>,
    pub byte_range: Range<usize>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Deserialize, serde::Serialize))]
pub struct Row {
    /// Logical bounding rectangle based on font heights etc.
    /// Use this when drawing a selection or similar!
    /// Includes leading and trailing whitespace.
    pub rect: Rect,
    /// The mesh, ready to be rendered.
    pub visuals: RowVisuals,
}

/// The tessellated output of a row.
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Deserialize, serde::Serialize))]
pub struct RowVisuals {
    /// The tessellated text, using non-normalized (texel) UV coordinates.
    /// That is, you need to divide the uv coordinates by the texture size.
    pub mesh: Mesh,

    /// Bounds of the mesh, and can be used for culling.
    /// Does NOT include leading or trailing whitespace glyphs!!
    pub mesh_bounds: Rect,

    /// The number of triangle indices added before the first glyph triangle.
    ///
    /// This can be used to insert more triangles after the background but before the glyphs,
    /// i.e. for text selection visualization.
    pub glyph_index_start: usize,

    /// The range of vertices in the mesh that contain glyphs (as opposed to background, underlines, strikethorugh, etc).
    ///
    /// The glyph vertices comes after backgrounds (if any), but before any underlines and strikethrough.
    pub glyph_vertex_range: Range<usize>,
}

impl Default for RowVisuals {
    fn default() -> Self {
        Self {
            mesh: Default::default(),
            mesh_bounds: Rect::NOTHING,
            glyph_index_start: 0,
            glyph_vertex_range: 0..0,
        }
    }
}

#[derive(Copy, Clone, Debug, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Deserialize, serde::Serialize))]
pub struct Glyph {
    /// The character this glyph represents.
    pub chr: char,

    /// Baseline position, relative to the galley.
    /// Logical position: pos.y is the same for all chars of the same [`TextStyle`].
    pub pos: Pos2,

    /// Logical width of the glyph.
    pub advance_width: f32,

    /// Height of this row of text.
    ///
    /// Usually same as [`Self::font_height`],
    /// unless explicitly overridden by [`TextStyle::line_height`].
    pub line_height: f32,

    /// Position and size of the glyph in the font texture, in texels.
    pub uv_rect: UvRect,
}

impl Glyph {
    #[inline]
    pub fn size(&self) -> Vec2 {
        Vec2::new(self.advance_width, self.line_height)
    }

    /// Same y range for all characters with the same [`TextStyle`].
    #[inline]
    pub fn logical_rect(&self) -> Rect {
        Rect::from_min_size(self.pos, self.size())
    }
}

/// Helper for creating and transforming text [`Selection`]s given a layout
/// computed in a [`Galley`].
pub struct SelectionDriver<'a> {
    galley: &'a Galley,
}

impl SelectionDriver<'_> {
    fn text(&self) -> &str {
        &self.galley.job.text
    }

    /// Returns a [`Selection`] of the entire contents of the associated [`Galley`].
    pub fn select_all(&self) -> Selection {
        Selection::new(
            ByteCursor {
                index: 0,
                affinity: Default::default(),
            },
            ByteCursor {
                index: self.text().len() - 1,
                affinity: Default::default(),
            },
        )
    }

    /// Returns an empty [`Selection`] at the given byte location.
    pub fn select_at_cursor(&self, cursor: &ByteCursor) -> Selection {
        Selection::new(*cursor, *cursor)
    }

    /// Returns a [`Selection`] at the given [`ByteCursor`] range. See [`Selection::anchor`] and [`Selection::focus`]
    /// for more info.
    pub fn select_cursor_range(&self, anchor: &ByteCursor, focus: &ByteCursor) -> Selection {
        Selection::new(*anchor, *focus)
    }

    /// Returns an empty [`Selection`] at the given galley-space location.
    pub fn select_single_point_at(&self, pos: Vec2) -> Selection {
        let (Vec2 { x, y }, section) = self.galley.pos_to_parley(pos);

        let parley_selection =
            parley::Selection::from_point(&section.section.parley_layout.layout.lock(), x, y);
        Selection::from_parley(&parley_selection, section.byte_range.start)
    }

    /// Returns a [`Selection`] of the word at the given galley-space location.
    pub fn select_word_at(&self, pos: Vec2) -> Selection {
        let (Vec2 { x, y }, section) = self.galley.pos_to_parley(pos);

        let parley_selection =
            parley::Selection::word_from_point(&section.section.parley_layout.layout.lock(), x, y);
        Selection::from_parley(&parley_selection, section.byte_range.start)
    }

    /// Returns a [`Selection`] of the layout line (wrapping text creates distinct lines) at the given galley-space
    /// location.
    pub fn select_line_at(&self, pos: Vec2) -> Selection {
        let (Vec2 { x, y }, section) = self.galley.pos_to_parley(pos);

        let parley_selection =
            parley::Selection::line_from_point(&section.section.parley_layout.layout.lock(), x, y);
        Selection::from_parley(&parley_selection, section.byte_range.start)
    }

    /// Given four cursors, return the left-most and right-most cursors from the set.
    ///
    /// Copied from Parley.
    fn cursor_min_max(
        layout: &parley::Layout<Color32>,
        cursors: [parley::Cursor; 4],
    ) -> [parley::Cursor; 2] {
        let cursor_pos = cursors
            .map(|cursor| (cursor, cursor.geometry(layout, 0.0)))
            .map(|(cursor, rect)| (cursor, (rect.y0, rect.x0)));
        let mut min = cursor_pos[0];
        let mut max = cursor_pos[0];
        for pos in cursor_pos {
            if pos.1 < min.1 {
                min = pos;
            }
            if pos.1 > max.1 {
                max = pos;
            }
        }
        [min.0, max.0]
    }

    /// Returns a [`Selection`] with the [`Selection::focus`] moved to the given galley-space location. If this is a
    /// word-based or line-based selection, the [`Selection::anchor`] may also be extended to a word or line boundary
    /// respectively.
    pub fn extend_selection_to_point(&self, selection: &Selection, pos: Vec2) -> Selection {
        let (Vec2 { x, y }, dest_section) = self.galley.pos_to_parley(pos);
        let layout = dest_section.section.parley_layout.layout.lock();

        let (target, start, end) = match selection.anchor_base {
            AnchorBase::Cluster => {
                let new_focus = parley::Cursor::from_point(&layout, x, y);
                return Selection::new(
                    selection.anchor,
                    ByteCursor::from_parley(&new_focus, dest_section.byte_range.start),
                );
            }
            AnchorBase::Word(start, end) => (
                parley::Selection::word_from_point(&layout, x, y),
                start,
                end,
            ),
            AnchorBase::Line(start, end) => (
                parley::Selection::line_from_point(&layout, x, y),
                start,
                end,
            ),
        };

        let [anchor, focus] = Self::cursor_min_max(
            &layout,
            [
                target.anchor(),
                target.focus(),
                start.as_parley(&layout, dest_section.byte_range.start),
                end.as_parley(&layout, dest_section.byte_range.start),
            ],
        );

        Selection {
            anchor: ByteCursor::from_parley(&anchor, dest_section.byte_range.start),
            focus: ByteCursor::from_parley(&focus, dest_section.byte_range.start),
            h_pos: None,
            anchor_base: selection.anchor_base,
        }
    }

    /// Returns a [`Selection`] with the [`Selection::focus`] moved to the given [`ByteCursor`]. If this is a word-based
    /// or line-based selection, the [`Selection::anchor`] may also be extended to a word or line boundary respectively.
    pub fn extend_selection_to_cursor(
        &self,
        selection: &Selection,
        focus: &ByteCursor,
    ) -> Selection {
        Selection::new(selection.anchor, *focus)
    }

    /// Returns a [`Selection`] at the previous visual character (this will differ from the previous "logical" character
    /// in right-to-left text). If the `extend` parameter is true, the original selection's [`Selection::anchor`] will
    /// stay where it is; if it is false, the selection at the new location will be empty.
    pub fn select_prev_character(&self, selection: &Selection, extend: bool) -> Selection {
        if !selection.is_empty() && !extend {
            let [anchor_geom, focus_geom] = [selection.anchor, selection.focus].map(|cursor| {
                let section = self.galley.section_at_cursor(cursor);
                let layout = section.section.parley_layout.layout.lock();
                cursor
                    .as_parley(&layout, section.byte_range.start)
                    .geometry(&layout, 0.0)
            });
            let new_focus = if (anchor_geom.y0, anchor_geom.x0) < (focus_geom.y0, focus_geom.x0) {
                selection.anchor
            } else {
                selection.focus
            };
            Selection::new(new_focus, new_focus)
        } else {
            selection.maybe_extend(selection.focus.prev_visual(self.galley), extend)
        }
    }

    /// Returns a [`Selection`] at the next visual character (this will differ from the previous "logical" character
    /// in right-to-left text). If the `extend` parameter is true, the original selection's [`Selection::anchor`] will
    /// stay where it is; if it is false, the selection at the new location will be empty.
    pub fn select_next_character(&self, selection: &Selection, extend: bool) -> Selection {
        if !selection.is_empty() && !extend {
            let [anchor_geom, focus_geom] = [selection.anchor, selection.focus].map(|cursor| {
                let section = self.galley.section_at_cursor(cursor);
                let layout = section.section.parley_layout.layout.lock();
                cursor
                    .as_parley(&layout, section.byte_range.start)
                    .geometry(&layout, 0.0)
            });
            let new_focus = if (anchor_geom.y0, anchor_geom.x0) > (focus_geom.y0, focus_geom.x0) {
                selection.anchor
            } else {
                selection.focus
            };
            Selection::new(new_focus, new_focus)
        } else {
            selection.maybe_extend(selection.focus.next_visual(self.galley), extend)
        }
    }

    /// Returns the byte range of the beginning of the paragraph to the left of the cursor (returning 0 if we're at the
    /// first paragraph). Used for deleting text, so we don't need to return a [`Selection`].
    pub fn paragraph_before_cursor(&self, selection: &Selection) -> Option<Range<usize>> {
        let range = selection.byte_range();
        let newline_index = self.text()[0..range.start].rfind('\n').unwrap_or(0);
        if newline_index == range.end {
            self.prev_cluster(selection)
        } else {
            Some(newline_index..range.end)
        }
    }

    /// Returns the byte range of the beginning of the paragraph to the right of the cursor (returning 0 if we're at the
    /// first paragraph). Used for deleting text, so we don't need to return a [`Selection`].
    pub fn paragraph_after_cursor(&self, selection: &Selection) -> Option<Range<usize>> {
        let range = selection.byte_range();
        let newline_index = self.text()[range.start..]
            .find('\n')
            .map_or(self.text().len(), |idx| idx + range.start);
        if newline_index == range.end {
            self.next_cluster(selection)
        } else {
            Some(range.start..newline_index)
        }
    }

    /// Returns the byte range of the previous logical character. Used for deleting text, so we don't need to return a
    /// [`Selection`].
    pub fn prev_cluster(&self, selection: &Selection) -> Option<Range<usize>> {
        // Adapted from Parley:
        // https://github.com/linebender/parley/blob/4307d3f/parley/src/layout/editor.rs#L236-L275

        /*let cluster = selection.0.focus().logical_clusters(self.layout)[0]?;
        let range = cluster.text_range();
        let end = range.end;
        let start = if cluster.is_hard_line_break() || cluster.is_emoji() {
            // For newline sequences and emoji, delete the previous cluster
            range.start
        } else {
            // Otherwise, delete the previous character
            let (start, _) = self
                .text
                .get(..end)
                .and_then(|s| s.char_indices().next_back())?;
            start
        };
        Some(start..end)*/
        todo!()
    }

    /// Returns the byte range of the previous logical character. Used for deleting text, so we don't need to return a
    /// [`Selection`].
    pub fn next_cluster(&self, selection: &Selection) -> Option<Range<usize>> {
        // Adapted from Parley:
        // https://github.com/linebender/parley/blob/4307d3f/parley/src/layout/editor.rs#L215-L233
        /*let cluster = selection.0.focus().logical_clusters(self.layout)[1]?;
        let range = cluster.text_range();
        if range.is_empty() {
            return None;
        }
        Some(range)*/
        todo!()
    }

    /// Returns a [`Selection`] at the previous visual word (this will differ from the previous "logical" word in
    /// right-to-left text). If the `extend` parameter is true, the original selection's [`Selection::anchor`] will stay
    /// where it is; if it is false, the selection at the new location will be empty.
    pub fn select_prev_word(&self, selection: &Selection, extend: bool) -> Selection {
        //selection.0.previous_visual_word(self.layout, extend).into()
        todo!()
    }

    /// Returns a [`Selection`] at the next visual word (this will differ from the next "logical" word in
    /// right-to-left text). If the `extend` parameter is true, the original selection's [`Selection::anchor`] will stay
    /// where it is; if it is false, the selection at the new location will be empty.
    pub fn select_next_word(&self, selection: &Selection, extend: bool) -> Selection {
        //selection.0.next_visual_word(self.layout, extend).into()
        todo!()
    }

    /// Returns a [`Selection`] at the same approximate x-position in the previous line. Successive calls to
    /// [`Self::select_prev_row`] and [`Self::select_next_row`] maintain the [`Selection`]'s internal state, and will
    /// remember the selection cursor's horizontal position, even if the currently-selected line is not that long.
    pub fn select_prev_row(&self, selection: &Selection, extend: bool) -> Selection {
        //selection.0.previous_line(self.layout, extend).into()
        todo!()
    }

    /// Returns a [`Selection`] at the same approximate x-position in the next line. Successive calls to
    /// [`Self::select_prev_row`] and [`Self::select_next_row`] maintain the [`Selection`]'s internal state, and will
    /// remember the selection cursor's horizontal position, even if the currently-selected line is not that long.
    pub fn select_next_row(&self, selection: &Selection, extend: bool) -> Selection {
        //selection.0.next_line(self.layout, extend).into()
        todo!()
    }

    /// Returns a [`Selection`] at the start of the current line. If the `extend` parameter is true, the original
    /// selection's [`Selection::anchor`] will stay where it is; if it is false, the selection at the new location will
    /// be empty.
    pub fn select_row_start(&self, selection: &Selection, extend: bool) -> Selection {
        let section = self.galley.section_at_cursor(selection.focus);
        let layout = section.section.parley_layout.layout.lock();
        Selection::from_parley(
            &selection
                .as_parley(&layout, section.byte_range.start)
                .line_start(&layout, extend),
            section.byte_range.start,
        )
    }

    /// Returns a [`Selection`] at the end of the current line. If the `extend` parameter is true, the original
    /// selection's [`Selection::anchor`] will stay where it is; if it is false, the selection at the new location will
    /// be empty.
    pub fn select_row_end(&self, selection: &Selection, extend: bool) -> Selection {
        // TODO: doesn't work on wrapped lines for some reason
        let section = self.galley.section_at_cursor(selection.focus);
        let layout = section.section.parley_layout.layout.lock();
        Selection::from_parley(
            &selection
                .as_parley(&layout, section.byte_range.start)
                .line_end(&layout, extend),
            section.byte_range.start,
        )
    }

    #[cfg(feature = "accesskit")]
    /// Create a new selection from an [`accesskit::TextSelection`].
    pub fn from_accesskit_selection(
        &self,
        selection: &accesskit::TextSelection,
    ) -> Option<Selection> {
        /*let accessibility =
            self.accessibility
                .get_or_init(self.text, self.layout, self.layout_offset);
        parley::Selection::from_access_selection(
            selection,
            self.layout,
            &accessibility.layout_access,
        )
        .map(Into::into)*/
        todo!()
    }

    #[cfg(feature = "accesskit")]
    /// Convert the given selection to an [`accesskit::TextSelection`].
    pub fn to_accesskit_selection(
        &self,
        selection: &Selection,
    ) -> Option<accesskit::TextSelection> {
        /*let accessibility =
            self.accessibility
                .get_or_init(self.text, self.layout, self.layout_offset);
        selection
            .0
            .to_access_selection(self.layout, &accessibility.layout_access)*/
        todo!()
    }
}

// ----------------------------------------------------------------------------

impl Galley {
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.job.is_empty()
    }

    /// The full, non-elided text of the input job.
    #[inline]
    pub fn text(&self) -> &str {
        &self.job.text
    }

    #[inline]
    pub fn size(&self) -> Vec2 {
        self.rect.size()
    }

    #[cfg(feature = "accesskit")]
    pub fn accessibility(&self) -> &GalleyAccessibility {
        /*self.accessibility.get_or_init(
            &self.job.text,
            &self.parley_layout.layout.lock(),
            self.parley_layout.offset,
        )*/
        todo!()
    }

    pub fn paint_selection(&mut self, color: Color32, selection: Selection) -> bool {
        self.selection_color = color;

        let mut did_draw_any = false;

        for (section, parley_selection) in Self::selection_to_parley(&mut self.sections, selection)
        {
            let inner_section = Arc::make_mut(&mut section.section);
            parley_selection.geometry_with(
                &inner_section.parley_layout.layout.lock(),
                |parley_rect, line_idx| {
                    let rect = Rect {
                        min: Pos2::new(parley_rect.x0 as f32, parley_rect.y0 as f32),
                        max: Pos2::new(parley_rect.x1 as f32, parley_rect.y1 as f32),
                    }
                    .translate(inner_section.parley_layout.offset + vec2(0.0, section.y_start));

                    let Some(row) = inner_section.rows.get_mut(line_idx) else {
                        return;
                    };

                    let selection_rects = self
                        .selection_rects
                        .get_or_insert_with(|| vec![Vec::new(); 5]);
                    selection_rects[line_idx].push(rect);
                    // Count selection rectangle bounds when culling
                    row.visuals.mesh_bounds = row.visuals.mesh_bounds.union(rect);
                    did_draw_any = true;
                },
            );
        }

        did_draw_any
    }

    pub fn estimated_first_row_height(&self) -> f32 {
        self.sections
            .first()
            .and_then(|section| section.section.rows.first())
            .map_or_else(|| self.size().y, |row| row.rect.height())
    }

    pub(super) fn section_at_cursor(&self, cursor: ByteCursor) -> &PlacedSection {
        let mut section_index = self
            .sections
            .binary_search_by_key(&cursor.index, |section| section.byte_range.start)
            .unwrap_or_else(|i| i.saturating_sub(1))
            .min(self.sections.len() - 1);
        if cursor.affinity == Affinity::Upstream
            && cursor.index == self.sections[section_index].byte_range.start
        {
            section_index = section_index.saturating_sub(1);
        }
        &self.sections[section_index]
    }

    fn section_by_row_index(
        sections: &[PlacedSection],
        row_index: usize,
    ) -> Option<&PlacedSection> {
        let index = sections
            .binary_search_by_key(&row_index, |section| section.row_range.start)
            .unwrap_or_else(|i| i.saturating_sub(1));
        sections.get(index)
    }

    fn section_by_row_index_mut(
        sections: &mut [PlacedSection],
        row_index: usize,
    ) -> Option<&mut PlacedSection> {
        let index = sections
            .binary_search_by_key(&row_index, |section| section.row_range.start)
            .unwrap_or_else(|i| i.saturating_sub(1));
        sections.get_mut(index)
    }

    pub fn rows(&self) -> impl Iterator<Item = &Row> {
        self.sections
            .iter()
            .map(|section| section.section.rows.iter())
            .flatten()
    }

    pub fn rows_mut(&mut self) -> impl Iterator<Item = &mut Row> {
        self.sections
            .iter_mut()
            .map(|section| Arc::make_mut(&mut section.section).rows.iter_mut())
            .flatten()
    }

    fn selection_to_parley(
        sections: &mut [PlacedSection],
        selection: Selection,
    ) -> impl Iterator<Item = (&mut PlacedSection, parley::Selection)> {
        sections
            .iter_mut()
            // TODO: I think <= is correct here
            .skip_while(move |section| section.byte_range.end <= selection.byte_range().start)
            .take_while(move |section| section.byte_range.start <= selection.byte_range().end)
            .map(move |section| {
                let relative_start = selection
                    .byte_range()
                    .start
                    .saturating_sub(section.byte_range.start);
                let relative_end = selection
                    .byte_range()
                    .end
                    .saturating_sub(section.byte_range.start);

                let parley_selection = {
                    let layout = section.section.parley_layout.layout.lock();

                    parley::Selection::new(
                        parley::Cursor::from_byte_index(
                            &layout,
                            relative_start,
                            Default::default(),
                        ),
                        parley::Cursor::from_byte_index(&layout, relative_end, Default::default()),
                    )
                };

                (section, parley_selection)
            })
    }
}

impl AsRef<str> for Galley {
    #[inline]
    fn as_ref(&self) -> &str {
        self.text()
    }
}

impl std::borrow::Borrow<str> for Galley {
    #[inline]
    fn borrow(&self) -> &str {
        self.text()
    }
}

impl std::ops::Deref for Galley {
    type Target = str;
    #[inline]
    fn deref(&self) -> &str {
        self.text()
    }
}

// ----------------------------------------------------------------------------

/// ## Physical positions
impl Galley {
    fn pos_to_parley(&self, pos: Vec2) -> (Vec2, &PlacedSection) {
        let section_index = self
            .sections
            .binary_search_by_key(&OrderedFloat(pos.y), |section| {
                OrderedFloat(section.y_start)
            })
            .unwrap_or_else(|i| i.saturating_sub(1));

        let section = &self.sections[section_index];
        let pos = pos - section.section.parley_layout.offset - vec2(0.0, section.y_start);

        (pos, section)
    }

    /// Returns a 0-width Rect.
    pub fn pos_from_cursor(&self, cursor: ByteCursor) -> Rect {
        let section = self.section_at_cursor(cursor);
        let layout = section.section.parley_layout.layout.lock();
        let cursor = cursor.as_parley(&layout, section.byte_range.start);
        dbg!(section.index, cursor.index(), cursor.affinity());
        let parley_rect = cursor.geometry(&layout, 0.0);
        Rect {
            min: Pos2::new(
                parley_rect.x0 as f32,
                parley_rect.y0 as f32 + section.y_start,
            ),
            max: Pos2::new(
                parley_rect.x1 as f32,
                parley_rect.y1 as f32 + section.y_start,
            ),
        }
        .translate(section.section.parley_layout.offset)
    }

    /// Cursor at the given position within the galley.
    ///
    /// A cursor above the galley is considered same as a cursor at the start, and a cursor below the galley is
    /// considered same as a cursor at the end. This allows implementing text-selection by dragging above/below the
    /// galley.
    pub fn cursor_from_pos(&self, pos: Vec2) -> ByteCursor {
        let (Vec2 { x, y }, section) = self.pos_to_parley(pos);
        let layout = section.section.parley_layout.layout.lock();
        ByteCursor::from_parley(
            &parley::Cursor::from_point(&layout, x, y),
            section.byte_range.start,
        )
    }
}

/// ## Cursor positions
impl Galley {
    /// Cursor to the first character.
    ///
    /// This is the same as [`ByteCursor::default`].
    #[inline]
    #[allow(clippy::unused_self)]
    pub fn begin(&self) -> ByteCursor {
        ByteCursor::default()
    }

    /// Cursor to one-past last character.
    pub fn end(&self) -> ByteCursor {
        let Some(section) = self.sections.last() else {
            return Default::default();
        };
        ByteCursor::from_parley(
            &parley::Cursor::from_byte_index(
                &section.section.parley_layout.layout.lock(),
                usize::MAX,
                Default::default(),
            ),
            section.byte_range.start,
        )
    }
}

/// ## Selections
impl Galley {
    /// Scoped access to a [`SelectionDriver`] for creating and transforming
    /// text [`Selection`]s.
    pub fn selection<T>(&self, f: impl FnOnce(&mut SelectionDriver<'_>) -> T) -> T {
        let mut driver = SelectionDriver {
            galley: &self,
            //#[cfg(feature = "accesskit")]
            //accessibility: &self.accessibility(),
        };

        f(&mut driver)
    }
}
