//! Streaming paragraph script context, clipped after line breaking and bidi ordering.
use std::ops::Range;
use unicode_bidi::{BidiDataSource, HardcodedBidiData};
use unicode_script::{Script, ScriptExtension, UnicodeScript};
use unicode_segmentation::UnicodeSegmentation;

const MAX_BRACKETS: usize = 64;
struct Opening {
    bracket: char,
    run_start: usize,
    scripts: ScriptExtension,
}

pub(crate) const LINE_BYTES: usize = 4096;

struct Runs {
    cursor: usize,
    base: usize,
    end: usize,
    start: usize,
    scripts: ScriptExtension,
    openings: Vec<Opening>,
    pairing: bool,
}
impl Runs {
    fn new(text: &str, base: usize) -> Self {
        Self {
            cursor: 0,
            base,
            end: text.len(),
            start: 0,
            scripts: ScriptExtension::default(),
            openings: Vec::new(),
            pairing: true,
        }
    }
}
impl Runs {
    fn next(&mut self, text: &str) -> Option<(Range<usize>, Script)> {
        let resume = self.cursor;
        for (relative, grapheme) in text[resume..].grapheme_indices(true) {
            let offset = resume + relative;
            self.cursor = offset + grapheme.len();
            let mut next = ScriptExtension::for_str(grapheme);
            if next.is_empty() {
                // Keep incompatible marks and unknown characters in their grapheme.
                next = grapheme
                    .chars()
                    .map(|c| c.script())
                    .find(|s| !matches!(s, Script::Common | Script::Inherited | Script::Unknown))
                    .unwrap_or(Script::Common)
                    .into();
            }
            let bracket = grapheme
                .chars()
                .next()
                .and_then(|c| HardcodedBidiData.bidi_matched_opening_bracket(c));
            if self.pairing
                && let Some(bracket) = bracket.filter(|b| !b.is_open)
                && let Some(index) = self
                    .openings
                    .iter()
                    .rposition(|o| o.bracket == bracket.opening)
            {
                let inherited = self.openings[index].scripts.intersection(next);
                if !inherited.is_empty() {
                    next = inherited;
                }
                self.openings.truncate(index);
            }
            let compatible = self.scripts.intersection(next);
            let completed = if compatible.is_empty() {
                let run = (
                    self.base + self.start..self.base + offset,
                    choose(self.scripts),
                );
                self.start = offset;
                self.scripts = next;
                Some(run)
            } else {
                if self.scripts != compatible {
                    for opening in &mut self.openings {
                        if opening.run_start == self.start {
                            opening.scripts = compatible;
                        }
                    }
                }
                self.scripts = compatible;
                None
            };
            if self.pairing
                && let Some(bracket) = bracket.filter(|b| b.is_open)
            {
                if self.openings.len() == MAX_BRACKETS {
                    self.openings.clear();
                    self.pairing = false;
                } else {
                    self.openings.push(Opening {
                        bracket: bracket.opening,
                        run_start: self.start,
                        scripts: self.scripts,
                    });
                }
            }
            if completed.is_some() {
                return completed;
            }
        }
        if self.start < self.end {
            let run = (
                self.base + self.start..self.base + self.end,
                choose(self.scripts),
            );
            self.start = self.end;
            Some(run)
        } else {
            None
        }
    }
}

/// Forward-only paragraph scan retaining only runs intersecting a line window.
pub(crate) struct Window {
    runs: Runs,
    cached: Vec<(Range<usize>, Script)>,
}
impl Window {
    pub(crate) fn new(text: &str) -> Self {
        Self {
            runs: Runs::new(text, 0),
            cached: Vec::new(),
        }
    }
    pub(crate) fn prepare(&mut self, text: &str, start: usize) {
        let end = start.saturating_add(LINE_BYTES);
        self.cached.retain(|(range, _)| range.end > start);
        while self.cached.last().is_none_or(|(range, _)| range.end < end) {
            let Some(run) = self.runs.next(text) else {
                break;
            };
            if run.0.end <= start {
                continue;
            }
            if self.cached.len() == self.cached.capacity() {
                self.cached
                    .reserve_exact(64.min(LINE_BYTES + 1 - self.cached.len()));
            }
            self.cached.push(run);
            debug_assert!(self.cached.len() <= LINE_BYTES + 1);
        }
    }
    pub(crate) fn ranges(&self) -> &[(Range<usize>, Script)] {
        &self.cached
    }
}

#[cfg(test)]
fn runs(text: &str, base: usize) -> Vec<(Range<usize>, Script)> {
    {
        let mut runs = Runs::new(text, base);
        std::iter::from_fn(|| runs.next(text)).collect()
    }
}

fn choose(scripts: ScriptExtension) -> Script {
    scripts
        .iter()
        .find(|s| !matches!(s, Script::Common | Script::Inherited))
        .unwrap_or(Script::Common)
}

/// Clips sorted script runs to a style span without restarting script context.
pub(crate) fn within(
    runs: &[(Range<usize>, Script)],
    span: Range<usize>,
) -> impl Iterator<Item = (Range<usize>, Script)> + '_ {
    let first = runs.partition_point(|(range, _)| range.end <= span.start);
    runs[first..]
        .iter()
        .take_while(move |(range, _)| range.start < span.end)
        .filter_map(move |(range, script)| {
            let start = range.start.max(span.start);
            let end = range.end.min(span.end);
            (start < end).then_some((start..end, *script))
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn window_keeps_bracket_context_across_wrapping_and_resume() {
        let text = format!("a ({} ) end", "α ".repeat(6000));
        let close = text.find(')').unwrap();
        let mut window = Window::new(&text);
        window.prepare(&text, 0);
        window.prepare(&text, 9000);
        window.prepare(&text, close);
        assert_eq!(
            within(window.ranges(), close..close + 1).collect::<Vec<_>>(),
            [(close..close + 1, Script::Latin)]
        );
        let mut resumed = Window::new(&text);
        resumed.prepare(&text, close);
        assert_eq!(resumed.ranges(), window.ranges());
        assert!(window.cached.len() <= 2);
    }

    #[test]
    fn dense_paragraph_window_discards_old_runs_and_skips_resume_prefix() {
        let text = "aα".repeat(20000);
        let all = runs(&text, 0);
        let mut window = Window::new(&text);
        for start in [0, 93, 4095, 8190, 51000, text.len()] {
            window.prepare(&text, start);
            let end = start.saturating_add(LINE_BYTES).min(text.len());
            assert_eq!(
                within(window.ranges(), start..end).collect::<Vec<_>>(),
                within(&all, start..end).collect::<Vec<_>>()
            );
            assert!(window.cached.len() <= LINE_BYTES + 1);
            assert!(window.cached.capacity() <= LINE_BYTES + 1);
            assert!(window.cached.iter().all(|(range, _)| range.end > start));
        }
        assert!(window.ranges().is_empty());
        let mut empty = Window::new("");
        empty.prepare("", 0);
        assert!(empty.ranges().is_empty());
    }

    #[test]
    fn style_clipping_preserves_bracket_and_neutral_script_context() {
        let text = "a (αб) end";
        let resolved = runs(text, 0);
        let close = text.find(')').unwrap();
        // The enclosed letters and closing bracket may have separate styles.
        assert_eq!(
            within(&resolved, 0..3).collect::<Vec<_>>(),
            [(0..3, Script::Latin)]
        );
        assert_eq!(
            within(&resolved, 3..close).collect::<Vec<_>>(),
            [(3..5, Script::Greek), (5..7, Script::Cyrillic)]
        );
        assert_eq!(
            within(&resolved, close..close + 1).collect::<Vec<_>>(),
            [(close..close + 1, Script::Latin)]
        );
        assert_eq!(
            within(&resolved, close + 1..text.len()).collect::<Vec<_>>(),
            [(close + 1..text.len(), Script::Latin)]
        );
        let leading = runs("(α)", 20);
        assert_eq!(
            within(&leading, 20..21).collect::<Vec<_>>(),
            [(20..21, Script::Greek)]
        );
        assert_eq!(
            within(&leading, 23..24).collect::<Vec<_>>(),
            [(23..24, Script::Greek)]
        );
        assert_eq!(within(&resolved, 3..3).count(), 0);
        assert_eq!(within(&resolved, text.len()..text.len() + 1).count(), 0);
        assert_eq!(within(&[], 0..1).count(), 0);
    }

    #[test]
    fn same_direction_scripts_split_and_neutral_prefix_follows_first_script() {
        let text = "123: Latin Ελληνικά Кириллица";
        let parts = runs(text, 7);
        assert_eq!(
            parts.iter().map(|(_, script)| *script).collect::<Vec<_>>(),
            [Script::Latin, Script::Greek, Script::Cyrillic]
        );
        assert_eq!(
            parts
                .iter()
                .map(|(range, _)| &text[range.start - 7..range.end - 7])
                .collect::<Vec<_>>(),
            ["123: Latin ", "Ελληνικά ", "Кириллица"]
        );
    }
    #[test]
    fn script_extensions_resolve_shared_japanese_marks_from_context() {
        assert_eq!(runs("ーあ", 0), vec![(0..6, Script::Hiragana)]);
        assert_eq!(runs("アー", 0), vec![(0..6, Script::Katakana)]);
        assert_eq!(
            runs("アーあ", 0),
            vec![(0..6, Script::Katakana), (6..9, Script::Hiragana)]
        );
    }
    fn parts(text: &str) -> Vec<(&str, Script)> {
        runs(text, 11)
            .into_iter()
            .map(|(range, script)| (&text[range.start - 11..range.end - 11], script))
            .collect()
    }
    #[test]
    fn paired_brackets_return_to_the_enclosing_script() {
        assert_eq!(
            parts("Latin (γ) end"),
            vec![
                ("Latin (", Script::Latin),
                ("γ", Script::Greek),
                (") end", Script::Latin)
            ]
        );
        assert_eq!(
            parts("α [a (б) a] α"),
            vec![
                ("α [", Script::Greek),
                ("a (", Script::Latin),
                ("б", Script::Cyrillic),
                (") a", Script::Latin),
                ("] α", Script::Greek)
            ]
        );
    }
    #[test]
    fn leading_brackets_adopt_the_first_script_and_unicode_pairs_match() {
        assert_eq!(
            parts("((α б)б)"),
            vec![
                ("((α ", Script::Greek),
                ("б", Script::Cyrillic),
                (")", Script::Greek),
                ("б", Script::Cyrillic),
                (")", Script::Greek)
            ]
        );
        assert_eq!(
            parts("a ⟦б⟧ a"),
            vec![
                ("a ⟦", Script::Latin),
                ("б", Script::Cyrillic),
                ("⟧ a", Script::Latin)
            ]
        );
        assert_eq!(
            parts("「あア」"),
            vec![
                ("「あ", Script::Hiragana),
                ("ア", Script::Katakana),
                ("」", Script::Hiragana)
            ]
        );
    }
    #[test]
    fn malformed_pairs_do_not_reuse_closed_or_discarded_openings() {
        assert_eq!(
            parts("a (α [б) в]"),
            vec![
                ("a (", Script::Latin),
                ("α [", Script::Greek),
                ("б", Script::Cyrillic),
                (") ", Script::Latin),
                ("в]", Script::Cyrillic)
            ]
        );
        assert_eq!(
            parts("a (α)β)"),
            vec![
                ("a (", Script::Latin),
                ("α", Script::Greek),
                (")", Script::Latin),
                ("β)", Script::Greek)
            ]
        );
    }
    #[test]
    fn excessive_nesting_disables_pairing_without_losing_source() {
        let at_limit = format!("a{}α{}", "(".repeat(MAX_BRACKETS), ")".repeat(MAX_BRACKETS));
        assert_eq!(
            parts(&at_limit)
                .iter()
                .map(|(_, script)| *script)
                .collect::<Vec<_>>(),
            [Script::Latin, Script::Greek, Script::Latin]
        );
        let text = format!(
            "a{}α{}",
            "(".repeat(MAX_BRACKETS + 1),
            ")".repeat(MAX_BRACKETS + 1)
        );
        let result = parts(&text);
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].1, Script::Latin);
        assert_eq!(result[1].1, Script::Greek);
        assert_eq!(
            result.iter().map(|(text, _)| *text).collect::<String>(),
            text
        );
    }
    #[test]
    fn graphemes_unknowns_and_emoji_keep_complete_source_coverage() {
        assert_eq!(
            parts("a (α)\u{345} a"),
            vec![
                ("a (", Script::Latin),
                ("α)\u{345} ", Script::Greek),
                ("a", Script::Latin),
            ]
        );
        for text in [
            "a\u{301}α\u{301}",
            "a\u{345}б",
            "👩\u{200d}💻 Latin",
            "\u{e000}б",
            "a (α)\u{301} a",
            "a (α)\u{345} a",
            "...",
            "",
        ] {
            let parts = runs(text, 0);
            let boundaries: Vec<_> = text
                .grapheme_indices(true)
                .map(|(i, _)| i)
                .chain(std::iter::once(text.len()))
                .collect();
            let mut end = 0;
            for (part, _) in parts {
                assert_eq!(part.start, end);
                assert!(part.end > part.start);
                assert!(boundaries.contains(&part.start));
                assert!(boundaries.contains(&part.end));
                end = part.end;
            }
            assert_eq!(end, text.len());
        }
    }
}
