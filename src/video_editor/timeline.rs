//! Output timeline math: kept segments plus title cards → a sequence of output items, and the
//! mappings between source time and output time. Cutting and trimming edit the segments.

use super::model::{Anchor, Segment, TitleCard, MIN_ITEM_MS};

/// One piece of the output.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ItemKind {
    /// Source video `[src_start, src_end)`.
    Clip { src_start: u64, src_end: u64 },
    /// Title card (index into the title list passed to [`build`]).
    Title(usize),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Item {
    pub out_start: u64,
    pub len: u64,
    pub kind: ItemKind,
}

impl Item {
    pub fn out_end(&self) -> u64 {
        self.out_start + self.len
    }
}

/// Where an output time falls.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pos {
    Clip {
        src_ms: u64,
        item: usize,
    },
    Title {
        index: usize,
        offset_ms: u64,
        item: usize,
    },
    /// At or past the end of the output.
    End,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Timeline {
    pub items: Vec<Item>,
    pub duration_ms: u64,
}

/// Sort, drop empty pieces and merge overlapping or touching segments.
pub fn normalize(segments: &mut Vec<Segment>) {
    segments.retain(|s| s.end_ms > s.start_ms);
    segments.sort_by_key(|s| s.start_ms);
    let mut out: Vec<Segment> = Vec::with_capacity(segments.len());
    for s in segments.drain(..) {
        match out.last_mut() {
            Some(last) if s.start_ms <= last.end_ms => last.end_ms = last.end_ms.max(s.end_ms),
            _ => out.push(s),
        }
    }
    *segments = out;
}

/// Build the output timeline. Title cards anchored at the same place keep their list order.
pub fn build(segments: &[Segment], titles: &[TitleCard]) -> Timeline {
    let mut segs = segments.to_vec();
    normalize(&mut segs);
    let mut items = Vec::new();
    let mut t = 0u64;
    let push = |items: &mut Vec<Item>, t: &mut u64, len: u64, kind: ItemKind| {
        if len > 0 {
            items.push(Item { out_start: *t, len, kind });
            *t += len;
        }
    };
    let title_len = |c: &TitleCard| c.duration_ms.max(MIN_ITEM_MS);
    for (i, c) in titles.iter().enumerate().filter(|(_, c)| c.anchor == Anchor::Start) {
        push(&mut items, &mut t, title_len(c), ItemKind::Title(i));
    }
    // Source-anchored cards in time order (stable: list order for equal times).
    let mut at: Vec<(u64, usize)> = titles
        .iter()
        .enumerate()
        .filter_map(|(i, c)| match c.anchor {
            Anchor::Source(ms) => Some((ms, i)),
            _ => None,
        })
        .collect();
    at.sort_by_key(|&(ms, _)| ms);
    let mut next = 0usize;
    for s in &segs {
        let mut cur = s.start_ms;
        while let Some(&(ms, i)) = at.get(next) {
            if ms >= s.end_ms {
                break;
            }
            if ms > cur {
                push(&mut items, &mut t, ms - cur, ItemKind::Clip { src_start: cur, src_end: ms });
                cur = ms;
            }
            push(&mut items, &mut t, title_len(&titles[i]), ItemKind::Title(i));
            next += 1;
        }
        push(&mut items, &mut t, s.end_ms - cur, ItemKind::Clip { src_start: cur, src_end: s.end_ms });
    }
    for &(_, i) in &at[next..] {
        push(&mut items, &mut t, title_len(&titles[i]), ItemKind::Title(i));
    }
    for (i, c) in titles.iter().enumerate().filter(|(_, c)| c.anchor == Anchor::End) {
        push(&mut items, &mut t, title_len(c), ItemKind::Title(i));
    }
    Timeline { items, duration_ms: t }
}

impl Timeline {
    /// Index of the item containing output time `out` (None at or past the end).
    pub fn item_at(&self, out: u64) -> Option<usize> {
        if out >= self.duration_ms {
            return None;
        }
        let i = self.items.partition_point(|it| it.out_end() <= out);
        (i < self.items.len()).then_some(i)
    }

    /// What plays at output time `out`.
    pub fn locate(&self, out: u64) -> Pos {
        let Some(i) = self.item_at(out) else { return Pos::End };
        let it = self.items[i];
        match it.kind {
            ItemKind::Clip { src_start, .. } => Pos::Clip { src_ms: src_start + (out - it.out_start), item: i },
            ItemKind::Title(index) => Pos::Title { index, offset_ms: out - it.out_start, item: i },
        }
    }

    /// Source time → output time, `None` if that moment was cut.
    pub fn source_to_output(&self, src: u64) -> Option<u64> {
        self.items.iter().find_map(|it| match it.kind {
            ItemKind::Clip { src_start, src_end } if src >= src_start && src < src_end => {
                Some(it.out_start + (src - src_start))
            }
            _ => None,
        })
    }

    /// Like [`Self::source_to_output`], but a cut moment maps to where the cut closed up (the
    /// start of the next kept clip, or the end of the video). Used to draw annotation bars.
    pub fn source_to_output_clamped(&self, src: u64) -> u64 {
        if let Some(o) = self.source_to_output(src) {
            return o;
        }
        // Clips are in source order: the first one starting at or after `src` is where it closed up.
        let mut last_clip_end = 0u64;
        for it in &self.items {
            if let ItemKind::Clip { src_start, .. } = it.kind {
                if src_start >= src {
                    return it.out_start;
                }
                last_clip_end = it.out_end();
            }
        }
        last_clip_end
    }

    /// The source ranges played inside output `[a, b)`, in output order.
    pub fn source_ranges(&self, a: u64, b: u64) -> Vec<Segment> {
        let mut out = Vec::new();
        for it in &self.items {
            let (lo, hi) = (a.max(it.out_start), b.min(it.out_end()));
            if lo >= hi {
                continue;
            }
            if let ItemKind::Clip { src_start, .. } = it.kind {
                out.push(Segment::new(src_start + (lo - it.out_start), src_start + (hi - it.out_start)));
            }
        }
        out
    }

    /// Output range of the video content (first clip start, last clip end), for trim handles.
    pub fn clip_bounds(&self) -> Option<(u64, u64)> {
        let is_clip = |i: &&Item| matches!(i.kind, ItemKind::Clip { .. });
        let first = self.items.iter().find(is_clip)?;
        let last = self.items.iter().rev().find(is_clip).unwrap_or(first);
        Some((first.out_start, last.out_end()))
    }

    /// Output start of title card `index`.
    pub fn title_start(&self, index: usize) -> Option<u64> {
        self.items.iter().find(|it| it.kind == ItemKind::Title(index)).map(|it| it.out_start)
    }
}

/// Remove source range `[a, b)` from the kept segments.
pub fn remove_source_range(segments: &mut Vec<Segment>, a: u64, b: u64) {
    if b <= a {
        return;
    }
    let mut out = Vec::with_capacity(segments.len() + 1);
    for s in segments.iter() {
        if s.end_ms <= a || s.start_ms >= b {
            out.push(*s);
            continue;
        }
        if s.start_ms < a {
            out.push(Segment::new(s.start_ms, a));
        }
        if s.end_ms > b {
            out.push(Segment::new(b, s.end_ms));
        }
    }
    out.retain(|s| !s.is_empty());
    *segments = out;
    normalize(segments);
}

/// Cut output range `[a, b)`: the video in it is removed from the segments, title cards inside
/// it are shortened (and dropped when shorter than [`MIN_ITEM_MS`]). Returns false when nothing
/// changed. The last kept video is never removed entirely.
pub fn cut(segments: &mut Vec<Segment>, titles: &mut Vec<TitleCard>, a: u64, b: u64) -> bool {
    let tl = build(segments, titles);
    let (a, b) = (a.min(tl.duration_ms), b.min(tl.duration_ms));
    if b <= a {
        return false;
    }
    let mut new_segs = segments.clone();
    for r in tl.source_ranges(a, b) {
        remove_source_range(&mut new_segs, r.start_ms, r.end_ms);
    }
    let mut changed = false;
    if new_segs.iter().map(Segment::len).sum::<u64>() >= MIN_ITEM_MS || segments.is_empty() {
        changed |= new_segs != *segments;
        *segments = new_segs;
    }
    // Titles: shorten by the overlap.
    let mut drop = Vec::new();
    for it in &tl.items {
        if let ItemKind::Title(i) = it.kind {
            let overlap = b.min(it.out_end()).saturating_sub(a.max(it.out_start));
            if overlap > 0 {
                if let Some(c) = titles.get_mut(i) {
                    let left = it.len.saturating_sub(overlap);
                    if left < MIN_ITEM_MS {
                        drop.push(i);
                    } else {
                        c.duration_ms = left;
                    }
                    changed = true;
                }
            }
        }
    }
    drop.sort_unstable();
    for i in drop.into_iter().rev() {
        titles.remove(i);
    }
    changed
}

/// Move the start of the first kept segment to `src` (trim handle). Can also extend it back
/// towards 0 to undo a trim.
pub fn trim_start(segments: &mut [Segment], src: u64) {
    if let Some(first) = segments.first_mut() {
        first.start_ms = src.min(first.end_ms.saturating_sub(MIN_ITEM_MS));
    }
}

/// Move the end of the last kept segment to `src`, at most `duration`.
pub fn trim_end(segments: &mut [Segment], src: u64, duration: u64) {
    if let Some(last) = segments.last_mut() {
        last.end_ms = src.clamp(last.start_ms + MIN_ITEM_MS, duration.max(last.start_ms + MIN_ITEM_MS));
    }
}

/// `mm:ss.mmm` (or `h:mm:ss.mmm`).
pub fn format_ms(ms: u64) -> String {
    let (h, m, s, r) = (ms / 3_600_000, ms / 60_000 % 60, ms / 1000 % 60, ms % 1000);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}.{r:03}")
    } else {
        format!("{m:02}:{s:02}.{r:03}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn card(anchor: Anchor, duration_ms: u64) -> TitleCard {
        TitleCard { anchor, duration_ms, ..TitleCard::default() }
    }

    #[test]
    fn whole_video_maps_one_to_one() {
        let tl = build(&[Segment::new(0, 10_000)], &[]);
        assert_eq!(tl.duration_ms, 10_000);
        assert_eq!(tl.locate(0), Pos::Clip { src_ms: 0, item: 0 });
        assert_eq!(tl.locate(9_999), Pos::Clip { src_ms: 9_999, item: 0 });
        assert_eq!(tl.locate(10_000), Pos::End);
        assert_eq!(tl.source_to_output(4_321), Some(4_321));
    }

    #[test]
    fn cut_middle_maps_both_ways() {
        let mut segs = vec![Segment::new(0, 10_000)];
        let mut titles = vec![];
        assert!(cut(&mut segs, &mut titles, 3_000, 5_000));
        assert_eq!(segs, vec![Segment::new(0, 3_000), Segment::new(5_000, 10_000)]);
        let tl = build(&segs, &titles);
        assert_eq!(tl.duration_ms, 8_000);
        // Output 3000 is source 5000 (right after the cut).
        assert_eq!(tl.locate(3_000), Pos::Clip { src_ms: 5_000, item: 1 });
        assert_eq!(tl.locate(2_999), Pos::Clip { src_ms: 2_999, item: 0 });
        assert_eq!(tl.source_to_output(6_000), Some(4_000));
        assert_eq!(tl.source_to_output(4_000), None);
        assert_eq!(tl.source_to_output_clamped(4_000), 3_000);
        assert_eq!(tl.source_to_output_clamped(20_000), 8_000);
        // Round trip for every kept millisecond step.
        for out in (0..8_000).step_by(37) {
            let Pos::Clip { src_ms, .. } = tl.locate(out) else { panic!("{out}") };
            assert_eq!(tl.source_to_output(src_ms), Some(out));
        }
    }

    #[test]
    fn second_cut_uses_output_time() {
        let mut segs = vec![Segment::new(0, 10_000)];
        let mut titles = vec![];
        cut(&mut segs, &mut titles, 3_000, 5_000); // source 3..5 gone
                                                   // Output 2..4 = source 2..3 + 5..6
        cut(&mut segs, &mut titles, 2_000, 4_000);
        assert_eq!(segs, vec![Segment::new(0, 2_000), Segment::new(6_000, 10_000)]);
        assert_eq!(build(&segs, &titles).duration_ms, 6_000);
    }

    #[test]
    fn trims() {
        let mut segs = vec![Segment::new(0, 10_000)];
        trim_start(&mut segs, 1_500);
        trim_end(&mut segs, 9_000, 10_000);
        assert_eq!(segs, vec![Segment::new(1_500, 9_000)]);
        let tl = build(&segs, &[]);
        assert_eq!(tl.duration_ms, 7_500);
        assert_eq!(tl.locate(0), Pos::Clip { src_ms: 1_500, item: 0 });
        // Extending back restores, but never past the source.
        trim_start(&mut segs, 0);
        trim_end(&mut segs, 50_000, 10_000);
        assert_eq!(segs, vec![Segment::new(0, 10_000)]);
        // A trim can't swallow the segment.
        trim_start(&mut segs, 20_000);
        assert_eq!(segs[0].len(), MIN_ITEM_MS);
    }

    #[test]
    fn titles_are_inserted_in_order() {
        let segs = vec![Segment::new(0, 4_000), Segment::new(6_000, 10_000)];
        let titles = vec![
            card(Anchor::End, 1_000),
            card(Anchor::Start, 2_000),
            card(Anchor::Source(2_000), 500),
            card(Anchor::Source(5_000), 700), // in the cut gap: before source 6000
        ];
        let tl = build(&segs, &titles);
        let kinds: Vec<ItemKind> = tl.items.iter().map(|i| i.kind).collect();
        assert_eq!(
            kinds,
            vec![
                ItemKind::Title(1),
                ItemKind::Clip { src_start: 0, src_end: 2_000 },
                ItemKind::Title(2),
                ItemKind::Clip { src_start: 2_000, src_end: 4_000 },
                ItemKind::Title(3),
                ItemKind::Clip { src_start: 6_000, src_end: 10_000 },
                ItemKind::Title(0),
            ]
        );
        assert_eq!(tl.duration_ms, 8_000 + 1_000 + 2_000 + 500 + 700);
        assert_eq!(tl.locate(0), Pos::Title { index: 1, offset_ms: 0, item: 0 });
        assert_eq!(tl.locate(2_000), Pos::Clip { src_ms: 0, item: 1 });
        assert_eq!(tl.source_to_output(2_000), Some(4_500));
        assert_eq!(tl.title_start(0), Some(tl.duration_ms - 1_000));
        assert_eq!(tl.clip_bounds(), Some((2_000, tl.duration_ms - 1_000)));
    }

    #[test]
    fn cutting_over_a_title_shortens_or_drops_it() {
        let mut segs = vec![Segment::new(0, 10_000)];
        let mut titles = vec![card(Anchor::Start, 2_000)];
        // Output 0..1500 is the first 1.5 s of the title.
        assert!(cut(&mut segs, &mut titles, 0, 1_500));
        assert_eq!(titles[0].duration_ms, 500);
        assert_eq!(segs, vec![Segment::new(0, 10_000)]);
        // Output 0..1000 now covers the title (dropped) and source 0..500.
        assert!(cut(&mut segs, &mut titles, 0, 1_000));
        assert!(titles.is_empty());
        assert_eq!(segs, vec![Segment::new(500, 10_000)]);
    }

    #[test]
    fn cannot_cut_everything() {
        let mut segs = vec![Segment::new(0, 1_000)];
        let mut titles = vec![];
        assert!(!cut(&mut segs, &mut titles, 0, 1_000));
        assert_eq!(segs, vec![Segment::new(0, 1_000)]);
    }

    #[test]
    fn source_ranges_span_items() {
        let segs = vec![Segment::new(0, 1_000), Segment::new(2_000, 3_000)];
        let tl = build(&segs, &[]);
        assert_eq!(tl.source_ranges(500, 1_500), vec![Segment::new(500, 1_000), Segment::new(2_000, 2_500)]);
    }

    #[test]
    fn normalize_merges() {
        let mut s = vec![Segment::new(5, 10), Segment::new(0, 6), Segment::new(20, 20), Segment::new(10, 12)];
        normalize(&mut s);
        assert_eq!(s, vec![Segment::new(0, 12)]);
    }

    #[test]
    fn formats_time() {
        assert_eq!(format_ms(0), "00:00.000");
        assert_eq!(format_ms(61_234), "01:01.234");
        assert_eq!(format_ms(3_723_004), "1:02:03.004");
    }
}
