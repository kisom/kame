//! A touchpad on Linux, made to scroll as one does on macOS. Wayland hands over how far the
//! fingers moved and nothing more, where macOS speeds up a fast swipe and lets the page coast on
//! once the fingers lift. So before egui sees a frame's input, a fast swipe here goes further than
//! the fingers did, and lifting them mid-swipe goes on scrolling, slowing to a stop. Fingers back
//! on the pad, or a click, stop it.
//!
//! Only scrolls measured in points are touched: a mouse wheel counts in lines and is left alone.

use eframe::egui::{self, Event, MouseWheelSource, MouseWheelUnit, RawInput, TouchPhase, Vec2};

/// The fingers' speed, in points a second, up to which a swipe moves the page as far as they went.
const SLOW: f32 = 300.0;
/// The speed from which a swipe goes [`MOST`] times as far as the fingers did.
const FAST: f32 = 2500.0;
/// The most a fast swipe is sped up.
const MOST: f32 = 3.0;
/// How quickly a coast slows: its speed falls to about a third in this many seconds.
const SLOWING: f32 = 0.4;
/// The least speed at the lift, in points a second, that goes on into a coast.
const COAST_FROM: f32 = 150.0;
/// The speed at which a coast stops.
const STOP: f32 = 15.0;
/// Fingers still for this long before lifting have stopped the page, and it doesn't coast.
const STILL: f64 = 0.05;

/// What's known of the fingers between frames.
#[derive(Debug, Default)]
pub(crate) struct Touchpad {
    /// How fast the page was last going, in points a second, sped up as it was shown.
    velocity: Vec2,
    /// When the fingers last moved.
    moved: Option<f64>,
    /// When the coast last moved the page, while it's coasting.
    coast: Option<f64>,
    /// Whether the coast has told egui it started, so that it's told when it ends.
    told: bool,
}

/// How many times further than the fingers a swipe at `speed` goes.
fn gain(speed: f32) -> f32 {
    let fast = ((speed - SLOW) / (FAST - SLOW)).clamp(0.0, 1.0);
    1.0 + fast * (MOST - 1.0)
}

fn scroll(delta: Vec2, phase: TouchPhase, modifiers: egui::Modifiers) -> Event {
    Event::MouseWheel {
        unit: MouseWheelUnit::Point,
        delta,
        phase,
        source: MouseWheelSource::Momentum,
        modifiers,
    }
}

impl Touchpad {
    /// Whether the page is coasting, and so needs another frame.
    pub(crate) fn coasting(&self) -> bool {
        self.coast.is_some()
    }

    /// Rewrites a frame's input before egui sees it: a fast swipe sped up, and a coast going on.
    pub(crate) fn adjust(&mut self, input: &mut RawInput) {
        let Some(now) = input.time else {
            return;
        };
        let modifiers = egui::Modifiers::NONE;
        let mut swiped = Vec2::ZERO;
        let mut lifted = false;
        for event in &input.events {
            match event {
                Event::MouseWheel {
                    unit: MouseWheelUnit::Point,
                    delta,
                    phase: TouchPhase::Move,
                    ..
                } => swiped += *delta,
                Event::MouseWheel {
                    unit: MouseWheelUnit::Point,
                    phase: TouchPhase::Start,
                    ..
                } => self.stop(),
                Event::MouseWheel {
                    unit: MouseWheelUnit::Point,
                    phase: TouchPhase::End | TouchPhase::Cancel,
                    ..
                } => lifted = true,
                Event::PointerButton { pressed: true, .. } => {
                    if self.told {
                        input
                            .events
                            .push(scroll(Vec2::ZERO, TouchPhase::End, modifiers));
                    }
                    self.stop();
                    self.velocity = Vec2::ZERO;
                    self.moved = None;
                    return;
                }
                _ => {}
            }
        }
        if swiped != Vec2::ZERO {
            self.stop();
            // A pause longer than a frame or so starts the speed afresh.
            let since = self
                .moved
                .map_or(f64::INFINITY, |moved| now - moved)
                .clamp(1.0 / 240.0, f64::INFINITY);
            let fresh = since > 0.1;
            let seconds = if fresh { 1.0 / 60.0 } else { since as f32 };
            let times = gain(swiped.length() / seconds);
            for event in &mut input.events {
                if let Event::MouseWheel {
                    unit: MouseWheelUnit::Point,
                    delta,
                    phase: TouchPhase::Move,
                    ..
                } = event
                {
                    *delta *= times;
                }
            }
            let velocity = swiped * times / seconds;
            self.velocity = if fresh {
                velocity
            } else {
                (self.velocity + velocity) * 0.5
            };
            self.moved = Some(now);
        }
        if lifted {
            let still = self.moved.is_none_or(|moved| now - moved > STILL);
            if !still && self.velocity.length() >= COAST_FROM {
                self.coast = Some(now);
            }
            self.moved = None;
            return;
        }
        let Some(last) = self.coast else {
            return;
        };
        let seconds = (now - last).max(0.0) as f32;
        let velocity = self.velocity * (-seconds / SLOWING).exp();
        if velocity.length() < STOP {
            self.stop();
            input
                .events
                .push(scroll(Vec2::ZERO, TouchPhase::End, modifiers));
            return;
        }
        if !self.told {
            input
                .events
                .push(scroll(Vec2::ZERO, TouchPhase::Start, modifiers));
            self.told = true;
        }
        // How far the speed carries the page in the time since, as it slows.
        let distance = self.velocity * SLOWING * (1.0 - (-seconds / SLOWING).exp());
        input
            .events
            .push(scroll(distance, TouchPhase::Move, modifiers));
        self.velocity = velocity;
        self.coast = Some(now);
    }

    fn stop(&mut self) {
        self.coast = None;
        self.told = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FRAME: f64 = 1.0 / 60.0;

    fn wheel(unit: MouseWheelUnit, dy: f32, phase: TouchPhase) -> Event {
        Event::MouseWheel {
            unit,
            delta: Vec2::new(0.0, dy),
            phase,
            source: MouseWheelSource::Unknown,
            modifiers: egui::Modifiers::NONE,
        }
    }

    fn fingers(dy: f32, phase: TouchPhase) -> Event {
        wheel(MouseWheelUnit::Point, dy, phase)
    }

    /// A touchpad given frames of input in turn, a frame apart.
    struct Pad {
        touchpad: Touchpad,
        time: f64,
    }

    impl Pad {
        fn new() -> Pad {
            Pad {
                touchpad: Touchpad::default(),
                time: 0.0,
            }
        }

        /// The next frame, with `events`, and what egui is given for it.
        fn frame(&mut self, events: Vec<Event>) -> Vec<Event> {
            self.time += FRAME;
            let mut input = RawInput {
                time: Some(self.time),
                events,
                ..Default::default()
            };
            self.touchpad.adjust(&mut input);
            input.events
        }

        /// A swipe of `frames` frames moving `dy` each, fingers kept down, and how far egui
        /// is told it went.
        fn swipe(&mut self, dy: f32, frames: usize) -> f32 {
            let mut went = moved(&self.frame(vec![fingers(0.0, TouchPhase::Start)]));
            for _ in 0..frames {
                went += moved(&self.frame(vec![fingers(dy, TouchPhase::Move)]));
            }
            went
        }

        fn lift(&mut self) -> Vec<Event> {
            self.frame(vec![fingers(0.0, TouchPhase::End)])
        }
    }

    /// How far a frame's scrolls move the page.
    fn moved(events: &[Event]) -> f32 {
        events
            .iter()
            .map(|event| match event {
                Event::MouseWheel {
                    unit: MouseWheelUnit::Point,
                    delta,
                    phase: TouchPhase::Move,
                    ..
                } => delta.y,
                _ => 0.0,
            })
            .sum()
    }

    /// How far a frame's coast moves the page.
    fn coasted(events: &[Event]) -> f32 {
        events
            .iter()
            .map(|event| match event {
                Event::MouseWheel {
                    source: MouseWheelSource::Momentum,
                    phase: TouchPhase::Move,
                    delta,
                    ..
                } => delta.y,
                _ => 0.0,
            })
            .sum()
    }

    #[test]
    fn a_slow_swipe_moves_as_far_as_the_fingers() {
        let mut pad = Pad::new();
        let went = pad.swipe(-2.0, 30);
        assert!((went - -60.0).abs() < 0.01, "it went {went}");
    }

    #[test]
    fn a_fast_swipe_goes_further_than_the_fingers() {
        let mut pad = Pad::new();
        let went = pad.swipe(-50.0, 10);
        assert!(went < -500.0 * 2.5, "it went {went}");
        assert!(went >= -500.0 * MOST - 0.01, "it went {went}");
    }

    #[test]
    fn lifting_the_fingers_mid_swipe_coasts_to_a_stop() {
        let mut pad = Pad::new();
        pad.swipe(-30.0, 10);
        assert_eq!(coasted(&pad.lift()), 0.0);
        let mut steps = Vec::new();
        for _ in 0..600 {
            steps.push(coasted(&pad.frame(Vec::new())));
        }
        assert!(steps[0] < 0.0, "it goes on the way it was going");
        assert!(
            steps.windows(2).all(|pair| pair[1] >= pair[0]),
            "it only slows"
        );
        assert!(!pad.touchpad.coasting(), "it stops");
        assert_eq!(steps.last(), Some(&0.0));
        let total: f32 = steps.iter().sum();
        assert!(total < -100.0, "it coasted {total}");
    }

    #[test]
    fn a_coast_tells_egui_it_started_and_ended() {
        let mut pad = Pad::new();
        pad.swipe(-30.0, 10);
        pad.lift();
        let first = pad.frame(Vec::new());
        assert!(matches!(
            first.first(),
            Some(Event::MouseWheel {
                phase: TouchPhase::Start,
                ..
            })
        ));
        let mut last = Vec::new();
        while pad.touchpad.coasting() {
            last = pad.frame(Vec::new());
        }
        assert!(matches!(
            last.last(),
            Some(Event::MouseWheel {
                phase: TouchPhase::End,
                ..
            })
        ));
    }

    #[test]
    fn fingers_that_stop_before_lifting_dont_coast() {
        let mut pad = Pad::new();
        pad.swipe(-30.0, 10);
        for _ in 0..10 {
            pad.frame(Vec::new());
        }
        pad.lift();
        assert_eq!(coasted(&pad.frame(Vec::new())), 0.0);
        assert!(!pad.touchpad.coasting());
    }

    #[test]
    fn a_slow_lift_doesnt_coast() {
        let mut pad = Pad::new();
        pad.swipe(-1.0, 10);
        pad.lift();
        assert_eq!(coasted(&pad.frame(Vec::new())), 0.0);
        assert!(!pad.touchpad.coasting());
    }

    #[test]
    fn fingers_back_on_the_pad_stop_the_coast() {
        let mut pad = Pad::new();
        pad.swipe(-30.0, 10);
        pad.lift();
        assert!(coasted(&pad.frame(Vec::new())) < 0.0);
        let touched = pad.frame(vec![fingers(0.0, TouchPhase::Start)]);
        assert_eq!(coasted(&touched), 0.0);
        assert!(!pad.touchpad.coasting());
        assert_eq!(coasted(&pad.frame(Vec::new())), 0.0);
    }

    #[test]
    fn a_click_stops_the_coast() {
        let mut pad = Pad::new();
        pad.swipe(-30.0, 10);
        pad.lift();
        pad.frame(Vec::new());
        let clicked = pad.frame(vec![Event::PointerButton {
            pos: egui::Pos2::ZERO,
            button: egui::PointerButton::Primary,
            pressed: true,
            modifiers: egui::Modifiers::NONE,
        }]);
        assert_eq!(coasted(&clicked), 0.0);
        assert!(matches!(
            clicked.last(),
            Some(Event::MouseWheel {
                phase: TouchPhase::End,
                ..
            })
        ));
        assert!(!pad.touchpad.coasting());
    }

    #[test]
    fn a_mouse_wheel_is_left_alone() {
        let mut pad = Pad::new();
        for _ in 0..10 {
            let events = pad.frame(vec![wheel(MouseWheelUnit::Line, -3.0, TouchPhase::Move)]);
            assert_eq!(
                events,
                vec![wheel(MouseWheelUnit::Line, -3.0, TouchPhase::Move)]
            );
        }
        pad.frame(vec![wheel(MouseWheelUnit::Line, 0.0, TouchPhase::End)]);
        assert_eq!(pad.frame(Vec::new()), Vec::new());
    }
}
