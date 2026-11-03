//! Choosing where a justified paragraph's lines break. Knows nothing of fonts: it sees widths.
//! See `docs/superpowers/specs/2026-11-03-word-spacing-design.md`.

use std::collections::BTreeMap;

/// One glyph as the choice sees it.
#[derive(Clone, Copy, Debug)]
pub(super) struct Item {
    pub width: f64,
    /// Whether it can stretch or close up.
    pub space: bool,
    /// How far it hangs left of the measure when it starts a line.
    pub left_hang: f64,
    /// How far it hangs right of the measure when it ends a line.
    pub right_hang: f64,
}

/// Where a line can end: the next line starts at item `at`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct Break {
    pub at: usize,
    /// Whether the line ends in a drawn hyphen.
    pub hyphen: bool,
}

/// The glyphs between two line breaks in the text, on one long line.
pub(super) struct Run {
    pub items: Vec<Item>,
    pub candidates: Vec<Break>,
    pub hyphen_width: f64,
    pub hyphen_hang: f64,
}

/// How far a space can close up, as a share of normal.
const MIN_SPACE: f64 = 0.9;
/// How many lines past the one being set count towards choosing it.
const LOOK_AHEAD: usize = 1;
/// How much a space widens for one unit of looseness.
const STRETCH: f64 = 0.5;
/// What a hyphen costs, and a second in a row.
const HYPHEN_COST: f64 = 5e4;
const SECOND_HYPHEN_COST: f64 = 1.5e5;
const MOST_HYPHENS_IN_A_ROW: u8 = 2;
/// What it costs to put a tight line beside a very loose one.
const CLASH_COST: f64 = 1e4;
/// What it costs to end a paragraph on a line under this share of the measure.
const SHORT_LAST_COST: f64 = 3e4;
const SHORT_LAST_SHARE: f64 = 0.1;
/// What a word too wide for any line costs, so that it's a last resort.
const OVERFULL_COST: f64 = 1e13;
/// What a line with no spaces to stretch costs when it falls short: a word alone with a hole
/// after it, which is worse than almost any looseness.
const UNSTRETCHABLE_COST: f64 = 1e12;

/// Running totals, so that a line's widths are a subtraction.
struct Sums {
    width: Vec<f64>,
    space: Vec<f64>,
}

impl Sums {
    fn of(run: &Run) -> Sums {
        let (mut width, mut space) = (vec![0.0], vec![0.0]);
        let (mut all, mut spaces) = (0.0, 0.0);
        for item in &run.items {
            all += item.width;
            if item.space {
                spaces += item.width;
            }
            width.push(all);
            space.push(spaces);
        }
        Sums { width, space }
    }
}

/// A line's width at normal spacing, the width of its spaces, and how far its words must
/// reach, which is further than the measure when punctuation hangs.
fn measure_line(
    run: &Run,
    sums: &Sums,
    start: usize,
    end: usize,
    hyphen: bool,
    measure: f64,
) -> (f64, f64, f64) {
    let mut last = end;
    while last > start && run.items[last - 1].space {
        last -= 1;
    }
    if last == start {
        return (0.0, 0.0, measure);
    }
    let natural =
        sums.width[last] - sums.width[start] + if hyphen { run.hyphen_width } else { 0.0 };
    let spaces = sums.space[last] - sums.space[start];
    // Two marks can end a line together, as in a period and a closing quote.
    let right: f64 = if hyphen {
        run.hyphen_hang
    } else {
        run.items[start..last]
            .iter()
            .rev()
            .take(2)
            .take_while(|item| item.right_hang > 0.0)
            .map(|item| item.right_hang)
            .sum()
    };
    (
        natural,
        spaces,
        measure + run.items[start].left_hang + right,
    )
}

/// As many words as fit at normal spacing on each line, from `start` to `stop`.
fn first_fit_between(
    run: &Run,
    sums: &Sums,
    start: usize,
    stop: usize,
    measure: f64,
    most_lines: usize,
) -> Vec<Break> {
    let mut breaks = Vec::new();
    let mut at = start;
    while at < stop && breaks.len() < most_lines {
        let mut best = None;
        let candidates = run
            .candidates
            .iter()
            .filter(|c| !c.hyphen && c.at > at && c.at < stop)
            .map(|c| c.at)
            .chain(std::iter::once(stop));
        for end in candidates {
            let (natural, ..) = measure_line(run, sums, at, end, false, measure);
            if natural <= measure + 0.001 {
                best = Some(end);
            } else {
                best = best.or(Some(end));
                break;
            }
        }
        let end = best.unwrap_or(stop);
        breaks.push(Break {
            at: end,
            hyphen: false,
        });
        at = end;
    }
    breaks
}

/// A line's breaks in order, as first fit sets them. The last is always the end of the run.
#[cfg(test)]
pub(super) fn first_fit(run: &Run, measure: f64) -> Vec<Break> {
    let sums = Sums::of(run);
    first_fit_between(run, &sums, 0, run.items.len(), measure, usize::MAX)
}

/// Sets one line at a time, choosing each by how it and the next fit. A line depends only on the
/// text up to a little past it, so an edit moves little above it.
pub(super) fn set_lines(run: &Run, measure: f64) -> Vec<Break> {
    let sums = Sums::of(run);
    let stop = run.items.len();
    let mut breaks = Vec::new();
    let mut at = 0;
    let mut hyphens = 0;
    while at < stop {
        // The stopping point's own line costs nothing, so go one line past those that count.
        let near = first_fit_between(run, &sums, at, stop, measure, LOOK_AHEAD + 2);
        let until = if near.len() == LOOK_AHEAD + 2 {
            near[LOOK_AHEAD + 1].at
        } else {
            stop
        };
        let next = cheapest_next_line(run, &sums, at, until, measure, hyphens);
        hyphens = if next.hyphen { hyphens + 1 } else { 0 };
        breaks.push(next);
        at = next.at;
    }
    breaks
}

/// What it costs to set a line, and how loose that makes it: negative when it closes up.
/// `None` means that it doesn't fit however far its spaces close, so it's overfull.
fn line_cost(
    run: &Run,
    sums: &Sums,
    start: usize,
    end: usize,
    hyphen: bool,
    measure: f64,
    last: bool,
) -> Option<(f64, f64)> {
    let (natural, spaces, reach) = measure_line(run, sums, start, end, hyphen, measure);
    if natural > reach + 0.001 && (spaces == 0.0 || 1.0 + (reach - natural) / spaces < MIN_SPACE) {
        return None;
    }
    let share = if spaces > 0.0 {
        1.0 + (reach - natural) / spaces
    } else {
        f64::INFINITY
    };
    let (looseness, mut cost) = if last && share >= 1.0 {
        (0.0, 1.0)
    } else if share.is_infinite() {
        (3.0, UNSTRETCHABLE_COST)
    } else {
        let looseness = if share >= 1.0 {
            (share - 1.0) / STRETCH
        } else {
            -(1.0 - share) / (1.0 - MIN_SPACE)
        };
        let badness = 1.0 + 100.0 * looseness.abs().powi(3);
        (looseness, badness * badness)
    };
    if last && natural < SHORT_LAST_SHARE * measure {
        cost += SHORT_LAST_COST;
    }
    Some((looseness, cost))
}

fn looseness_class(looseness: f64) -> u8 {
    match looseness {
        l if l < -0.5 => 0,
        l if l <= 0.5 => 1,
        l if l <= 1.0 => 2,
        _ => 3,
    }
}

/// The first break on the cheapest way from `start` to `until`.
fn cheapest_next_line(
    run: &Run,
    sums: &Sums,
    start: usize,
    until: usize,
    measure: f64,
    hyphens_so_far: u8,
) -> Break {
    /// Where a line ended, how loose it was, and how many hyphens have ended lines in a row.
    type State = (usize, u8, u8);
    struct Route {
        cost: f64,
        from: Option<State>,
        hyphen: bool,
    }

    let mut points: Vec<Break> = run
        .candidates
        .iter()
        .copied()
        .filter(|c| c.at > start && c.at < until)
        .collect();
    points.push(Break {
        at: until,
        hyphen: false,
    });

    // Ordered, so that ties always resolve the same way and layout is a pure function.
    let mut routes: BTreeMap<State, Route> = BTreeMap::new();
    routes.insert(
        (start, 1, hyphens_so_far),
        Route {
            cost: 0.0,
            from: None,
            hyphen: false,
        },
    );

    let mut positions = vec![start];
    positions.extend(points.iter().map(|p| p.at));
    for at in positions {
        let here: Vec<(State, f64)> = routes
            .range((at, 0, 0)..=(at, u8::MAX, u8::MAX))
            .map(|(state, route)| (*state, route.cost))
            .collect();
        if here.is_empty() {
            continue;
        }
        let mut lines = Vec::new();
        let mut fits = false;
        for point in points.iter().filter(|p| p.at > at) {
            let last = point.at == until;
            match line_cost(run, sums, at, point.at, point.hyphen, measure, last) {
                Some((looseness, cost)) => {
                    fits = true;
                    lines.push((*point, looseness, cost));
                }
                None => {
                    // Words only get wider from here. A line nothing fits on still has to go.
                    if !fits {
                        lines.push((*point, 0.0, OVERFULL_COST));
                    }
                    break;
                }
            }
        }
        for (point, looseness, cost) in lines {
            let class = looseness_class(looseness);
            for (state, so_far) in &here {
                let hyphens = if point.hyphen { state.2 + 1 } else { 0 };
                if hyphens > MOST_HYPHENS_IN_A_ROW {
                    continue;
                }
                let mut total = so_far + cost;
                if (i16::from(class) - i16::from(state.1)).abs() > 1 {
                    total += CLASH_COST;
                }
                if point.hyphen {
                    total += if hyphens >= 2 {
                        SECOND_HYPHEN_COST
                    } else {
                        HYPHEN_COST
                    };
                }
                let next = (point.at, class, hyphens);
                if routes.get(&next).is_none_or(|route| total < route.cost) {
                    routes.insert(
                        next,
                        Route {
                            cost: total,
                            from: Some(*state),
                            hyphen: point.hyphen,
                        },
                    );
                }
            }
        }
    }

    // A route always exists, unless hyphens have run out, and a whole word can follow those.
    let end = routes
        .range((until, 0, 0)..=(until, u8::MAX, u8::MAX))
        .min_by(|a, b| a.1.cost.total_cmp(&b.1.cost))
        .map(|(state, _)| *state);
    let mut first = Break {
        at: until,
        hyphen: false,
    };
    let mut at = end;
    while let Some(state) = at {
        let route = &routes[&state];
        if route.from.is_some() {
            first = Break {
                at: state.0,
                hyphen: route.hyphen,
            };
        }
        at = route.from;
    }
    first
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Words as widths at ten a letter, with spaces of four.
    fn run_of(text: &str) -> Run {
        let mut items = Vec::new();
        let mut candidates = Vec::new();
        for (index, c) in text.chars().enumerate() {
            let space = c == ' ';
            items.push(Item {
                width: if space { 4.0 } else { 10.0 },
                space,
                left_hang: 0.0,
                right_hang: 0.0,
            });
            if space {
                candidates.push(Break {
                    at: index + 1,
                    hyphen: false,
                });
            }
        }
        Run {
            items,
            candidates,
            hyphen_width: 5.0,
            hyphen_hang: 3.5,
        }
    }

    fn ends(breaks: &[Break]) -> Vec<usize> {
        breaks.iter().map(|b| b.at).collect()
    }

    #[test]
    fn first_fit_takes_as_many_words_as_fit() {
        // "aaa bbb" is 64 wide, and the space after it doesn't count.
        let run = run_of("aaa bbb ccc ddd");
        assert_eq!(ends(&first_fit(&run, 64.0)), vec![8, 15]);
        assert_eq!(ends(&first_fit(&run, 63.0)), vec![4, 8, 12, 15]);
    }

    #[test]
    fn a_paragraph_that_fits_is_one_line() {
        let run = run_of("aaa bbb");
        assert_eq!(ends(&set_lines(&run, 500.0)), vec![7]);
    }

    #[test]
    fn looking_ahead_avoids_a_loose_line() {
        let run = run_of(
            "the quick brown fox jumps over the lazy dog and then it runs away again across \
             the wide green field until the evening comes and everyone goes home at last with \
             extraordinarily disproportionate considerations about misunderstandings",
        );
        let loose = |breaks: &[Break], measure: f64| {
            let mut start = 0;
            let mut count = 0;
            for (index, b) in breaks.iter().enumerate() {
                if index + 1 < breaks.len() {
                    let share = spread(&run, start, b.at, measure);
                    count += usize::from(share.is_some_and(|share| share > 1.5));
                }
                start = b.at;
            }
            count
        };
        let (mut first, mut chosen) = (0, 0);
        for measure in (200..500).step_by(7) {
            let measure = f64::from(measure);
            first += loose(&first_fit(&run, measure), measure);
            chosen += loose(&set_lines(&run, measure), measure);
        }
        assert!(chosen < first, "{chosen} loose lines against {first}");
    }

    /// The final width of a line's spaces, as a share of normal.
    fn spread(run: &Run, start: usize, end: usize, measure: f64) -> Option<f64> {
        let mut last = end;
        while last > start && run.items[last - 1].space {
            last -= 1;
        }
        let natural: f64 = run.items[start..last].iter().map(|i| i.width).sum();
        let spaces: f64 = run.items[start..last]
            .iter()
            .filter(|i| i.space)
            .map(|i| i.width)
            .sum();
        (spaces > 0.0).then(|| 1.0 + (measure - natural) / spaces)
    }

    #[test]
    fn a_word_is_not_left_alone_while_another_would_fit() {
        // Alone, "bbbbbbbb" leaves a hole. With its neighbour the single space is very wide,
        // and that's still better.
        let run = run_of("aaaaaaaa bbbbbbbb cccccccccccccccccccc dddddddd");
        for b in set_lines(&run, 200.0) {
            assert_ne!(b.at, 9, "aaaaaaaa was left alone");
        }
    }

    #[test]
    fn spaces_never_close_past_the_limit() {
        let text = "the quick brown fox jumps over the lazy dog and then it runs away again \
                    across the wide green field until the evening comes and everyone goes home";
        let run = run_of(text);
        for measure in [150.0, 200.0, 263.0, 300.0, 417.0] {
            let breaks = set_lines(&run, measure);
            let mut start = 0;
            for b in &breaks {
                if let Some(f) = spread(&run, start, b.at, measure) {
                    assert!(f >= MIN_SPACE - 1e-9, "{f} at measure {measure}");
                }
                start = b.at;
            }
        }
    }

    #[test]
    fn a_word_too_wide_for_any_line_gets_its_own() {
        let run = run_of("aa bbbbbbbbbbbbbbbbbbbbbbbbbbbbbb cc");
        let breaks = set_lines(&run, 100.0);
        assert_eq!(ends(&breaks), vec![3, 34, 36]);
    }

    #[test]
    fn every_item_is_on_a_line_in_order() {
        let run = run_of("one two three four five six seven eight nine ten eleven twelve");
        let breaks = set_lines(&run, 170.0);
        let at = ends(&breaks);
        assert!(at.windows(2).all(|pair| pair[0] < pair[1]), "{at:?}");
        assert_eq!(at.last(), Some(&run.items.len()));
    }

    #[test]
    fn the_same_run_always_sets_the_same_lines() {
        let run = run_of("one two three four five six seven eight nine ten eleven twelve");
        let first = set_lines(&run, 170.0);
        for _ in 0..50 {
            assert_eq!(set_lines(&run, 170.0), first);
        }
    }

    #[test]
    fn a_hyphen_is_used_only_where_it_helps() {
        let plain = run_of("aaaa bbbb cccc dddd eeee");
        let mut hyphened = run_of("aaaa bbbb cccc dddd eeee");
        hyphened.candidates.push(Break {
            at: 7,
            hyphen: true,
        });
        hyphened.candidates.sort_by_key(|b| b.at);
        // At a measure that fits whole words nicely, the hyphen is too dear.
        assert_eq!(set_lines(&hyphened, 94.0), set_lines(&plain, 94.0));

        // A line only a long word can fill takes one. "aaa " then a word that leaves it
        // three times too loose without a break.
        let mut long = run_of("aaa bbb cccccccccccccccccccc dd");
        long.candidates.push(Break {
            at: 18,
            hyphen: true,
        });
        long.candidates.sort_by_key(|b| b.at);
        let breaks = set_lines(&long, 170.0);
        assert!(breaks.iter().any(|b| b.hyphen), "{breaks:?}");
    }

    #[test]
    fn three_hyphens_never_end_lines_in_a_row() {
        let text = "abcdefghijklmnopqrstuvwxyzabcdefghijklmnopqrstuvwxyz";
        let mut run = run_of(text);
        run.candidates = (6..text.len())
            .step_by(3)
            .map(|at| Break { at, hyphen: true })
            .collect();
        let breaks = set_lines(&run, 100.0);
        let mut in_a_row = 0;
        for b in &breaks {
            in_a_row = if b.hyphen { in_a_row + 1 } else { 0 };
            assert!(in_a_row <= 2, "{breaks:?}");
        }
    }
}
