//! Different types of text cursors, i.e. ways to point into a [`super::Galley`].

use std::ops::Range;

use ecolor::Color32;

use super::Galley;

/// Determines whether a cursor is attached to the preceding or following character.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Deserialize, serde::Serialize))]
pub enum Affinity {
    /// The cursor is attached to the following character in the text (e.g. if it's positioned in the middle of a line
    /// wrap, it'll be on the bottom line).
    #[default]
    Downstream,
    /// The cursor is attached to the preceding character in the text.
    Upstream,
}

impl PartialOrd for Affinity {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Affinity {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        match (self, other) {
            (Self::Downstream, Self::Downstream) | (Self::Upstream, Self::Upstream) => {
                std::cmp::Ordering::Equal
            }
            (Self::Downstream, Self::Upstream) => std::cmp::Ordering::Greater,
            (Self::Upstream, Self::Downstream) => std::cmp::Ordering::Less,
        }
    }
}

impl From<parley::Affinity> for Affinity {
    #[inline]
    fn from(value: parley::Affinity) -> Self {
        match value {
            parley::Affinity::Downstream => Self::Downstream,
            parley::Affinity::Upstream => Self::Upstream,
        }
    }
}

impl From<Affinity> for parley::Affinity {
    #[inline]
    fn from(value: Affinity) -> Self {
        match value {
            Affinity::Downstream => Self::Downstream,
            Affinity::Upstream => Self::Upstream,
        }
    }
}

/// Byte-index-based text cursor.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Deserialize, serde::Serialize))]
pub struct ByteCursor {
    pub index: usize,
    pub affinity: Affinity,
}

impl ByteCursor {
    pub const START: Self = Self {
        index: 0,
        affinity: Affinity::Downstream,
    };

    pub const END: Self = Self {
        index: usize::MAX,
        affinity: Affinity::Downstream,
    };

    #[inline]
    pub(crate) fn as_parley(
        &self,
        layout: &parley::Layout<Color32>,
        byte_offset: usize,
    ) -> parley::Cursor {
        parley::Cursor::from_byte_index(
            layout,
            self.index.saturating_sub(byte_offset),
            self.affinity.into(),
        )
    }

    pub(crate) fn from_parley(parley_cursor: &parley::Cursor, byte_offset: usize) -> Self {
        Self {
            index: parley_cursor.index() + byte_offset,
            affinity: parley_cursor.affinity().into(),
        }
    }

    pub(crate) fn prev_visual(&self, galley: &Galley) -> Self {
        let section = galley.section_at_cursor(*self);
        let layout = section.section.parley_layout.layout.lock();
        let parley_cursor = self.as_parley(&layout, section.byte_range.start);
        let prev_in_layout = parley_cursor.previous_visual(&layout);

        // We successfully advanced to the previous visual cluster.
        if parley_cursor.index() != prev_in_layout.index()
            || parley_cursor.affinity() != prev_in_layout.affinity()
        {
            return Self::from_parley(&prev_in_layout, section.byte_range.start);
        }

        // We need to check the previous section. Do nothing if we're at the start of the entire galley.
        let Some(prev_section) = galley.sections.get(section.index - 1) else {
            return Self::from_parley(&prev_in_layout, section.byte_range.start);
        };

        let prev_layout = prev_section.section.parley_layout.layout.lock();
        let parley_cursor = self.as_parley(&prev_layout, prev_section.byte_range.start);
        // TODO(valadaptive): parley::Cursor::previous_visual calls visual_clusters internally, which is expensive so
        // it'd be nice to deduplicate those calls. But it'd involve inlining the contents of
        // parley::Cursor::previous_visual.
        let prev_parley_cursor = parley_cursor.previous_visual(&prev_layout);
        let affinity = Self::affinity_for_dir(
            prev_parley_cursor.visual_clusters(&prev_layout)[0]
                .map_or_else(|| false, |c| c.is_rtl()),
            false,
        );
        Self::from_parley(
            &parley::Cursor::from_byte_index_unchecked(prev_parley_cursor.index(), affinity.into()),
            prev_section.byte_range.start,
        )
    }

    pub(crate) fn next_visual(&self, galley: &Galley) -> Self {
        let section = galley.section_at_cursor(*self);
        let layout = section.section.parley_layout.layout.lock();
        let parley_cursor = self.as_parley(&layout, section.byte_range.start);
        let next_in_layout = parley_cursor.next_visual(&layout);

        // We successfully advanced to the next visual cluster.
        if parley_cursor.index() != next_in_layout.index()
            || parley_cursor.affinity() != next_in_layout.affinity()
        {
            return Self::from_parley(&next_in_layout, section.byte_range.start);
        }

        // We need to check the next section. Do nothing if we're at the end of the entire galley.
        let Some(next_section) = galley.sections.get(section.index + 1) else {
            return Self::from_parley(&next_in_layout, section.byte_range.start);
        };

        dbg!("actually going next");

        let next_layout = next_section.section.parley_layout.layout.lock();
        let parley_cursor = self.as_parley(&next_layout, next_section.byte_range.start);
        // TODO(valadaptive): see comment above on prev_visual
        let next_parley_cursor = parley_cursor.next_visual(&next_layout);
        dbg!(
            parley_cursor.index(),
            next_parley_cursor.index(),
            parley_cursor.affinity(),
            next_parley_cursor.affinity()
        );
        let affinity = Self::affinity_for_dir(
            next_parley_cursor.visual_clusters(&next_layout)[1]
                .map_or_else(|| false, |c| c.is_rtl()),
            true,
        );
        Self::from_parley(
            &parley::Cursor::from_byte_index_unchecked(next_parley_cursor.index(), affinity.into()),
            next_section.byte_range.start,
        )
    }

    fn affinity_for_dir(is_rtl: bool, moving_right: bool) -> Affinity {
        // Adapted from Parley:
        // https://github.com/linebender/parley/blob/1390840a8c2973779f7eab09e3b0007de9dfc4e9/parley/src/layout/cursor.rs#L961-L966
        match (is_rtl, moving_right) {
            (true, true) | (false, false) => Affinity::Downstream,
            _ => Affinity::Upstream,
        }
    }
}

#[derive(Clone, Copy, Debug, Default)]
#[cfg_attr(feature = "serde", derive(serde::Deserialize, serde::Serialize))]
pub(super) enum AnchorBase {
    #[default]
    Cluster,
    Word(ByteCursor, ByteCursor),
    Line(ByteCursor, ByteCursor),
}

impl AnchorBase {
    fn from_parley(parley_anchor_base: &parley::AnchorBase, byte_offset: usize) -> Self {
        match parley_anchor_base {
            parley::AnchorBase::Cluster => Self::Cluster,
            parley::AnchorBase::Word(start, end) => Self::Word(
                ByteCursor::from_parley(start, byte_offset),
                ByteCursor::from_parley(end, byte_offset),
            ),
            parley::AnchorBase::Line(start, end) => Self::Line(
                ByteCursor::from_parley(start, byte_offset),
                ByteCursor::from_parley(end, byte_offset),
            ),
        }
    }

    fn as_parley(
        &self,
        layout: &parley::Layout<Color32>,
        byte_offset: usize,
    ) -> parley::AnchorBase {
        match self {
            Self::Cluster => parley::AnchorBase::Cluster,
            Self::Word(start, end) => parley::AnchorBase::Word(
                start.as_parley(layout, byte_offset),
                end.as_parley(layout, byte_offset),
            ),
            Self::Line(start, end) => parley::AnchorBase::Line(
                start.as_parley(layout, byte_offset),
                end.as_parley(layout, byte_offset),
            ),
        }
    }
}

/// Range between two cursors, with some extra text-edit state. Requires text layout to be done before it can be
/// constructed from two [`ByteCursor`]s.
#[derive(Clone, Copy, Debug, Default)]
#[cfg_attr(feature = "serde", derive(serde::Deserialize, serde::Serialize))]
pub struct Selection {
    pub(super) anchor: ByteCursor,
    pub(super) focus: ByteCursor,
    pub(super) h_pos: Option<f32>,
    pub(super) anchor_base: AnchorBase,
}

impl Selection {
    pub fn new(anchor: ByteCursor, focus: ByteCursor) -> Self {
        Self {
            anchor,
            focus,
            ..Default::default()
        }
    }

    /// When selecting with a mouse, this is where the mouse was first pressed.
    /// This part of the cursor does not move when shift is down.
    #[inline]
    pub fn anchor(&self) -> ByteCursor {
        self.anchor
    }

    /// When selecting with a mouse, this is where the mouse was released.
    /// When moving with e.g. shift+arrows, this is what moves.
    /// Note that the two ends can come in any order, and also be equal (no selection).
    #[inline]
    pub fn focus(&self) -> ByteCursor {
        self.focus
    }

    #[deprecated = "use `focus` instead"]
    pub fn primary(&self) -> ByteCursor {
        self.focus
    }

    #[deprecated = "use `anchor` instead"]
    pub fn secondary(&self) -> ByteCursor {
        self.anchor
    }

    /// Does this selection contain any characters, or is it empty (both ends are the same)?
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.anchor.index == self.focus.index
    }

    #[inline]
    pub fn sorted_cursors(&self) -> [ByteCursor; 2] {
        if self.anchor() < self.focus() {
            [self.anchor(), self.focus()]
        } else {
            [self.focus(), self.anchor()]
        }
    }

    #[inline]
    pub fn byte_range(&self) -> Range<usize> {
        let [min, max] = self.sorted_cursors();
        min.index..max.index
    }

    pub fn contains(&self, other: &Self) -> bool {
        let [my_min, my_max] = self.sorted_cursors();
        let [other_min, other_max] = other.sorted_cursors();
        other_min >= my_min && other_max <= my_max
    }

    #[inline]
    pub fn slice_str<'s>(&self, text: &'s str) -> &'s str {
        &text[self.byte_range()]
    }

    /// Collapses this selection into an empty one around its [`Self::focus()`].
    #[inline]
    pub fn collapse(&self) -> Self {
        Self::new(self.focus, self.focus)
    }

    pub(super) fn from_parley(parley_selection: &parley::Selection, byte_offset: usize) -> Self {
        Self {
            anchor: ByteCursor::from_parley(&parley_selection.anchor(), byte_offset),
            focus: ByteCursor::from_parley(&parley_selection.focus(), byte_offset),
            h_pos: parley_selection.h_pos(),
            anchor_base: AnchorBase::from_parley(&parley_selection.anchor_base(), byte_offset),
        }
    }

    pub(super) fn as_parley(
        &self,
        layout: &parley::Layout<Color32>,
        byte_offset: usize,
    ) -> parley::Selection {
        parley::Selection::from_parts(
            self.anchor.as_parley(layout, byte_offset),
            self.focus.as_parley(layout, byte_offset),
            self.anchor_base.as_parley(layout, byte_offset),
            self.h_pos,
        )
    }

    pub fn maybe_extend(&self, focus: ByteCursor, extend: bool) -> Self {
        if extend {
            Self::new(self.anchor, focus)
        } else {
            Self::new(focus, focus)
        }
    }
}

impl PartialEq for Selection {
    fn eq(&self, other: &Self) -> bool {
        self.anchor == other.anchor && self.focus == other.focus
    }
}

impl Eq for Selection {}
