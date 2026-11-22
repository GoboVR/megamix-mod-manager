//! Controller input (gilrs), turned into UI actions.
//!
//! On SteamOS/PB-OS, Steam exposes the built-in controls as a virtual
//! "X-Box 360 pad", which works in Desktop and Game Mode. In Desktop Mode,
//! Steam's desktop layout may *also* send arrow keys / Enter for the same
//! press, so [`InputClock`] drops whichever of the two arrives second.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use gilrs::{Axis, Button, EventType, Gilrs};

/// A controller action.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pad {
    Up,
    Down,
    Left,
    Right,
    A,
    B,
    X,
    Y,
    Lb,
    Rb,
    Start,
    Select,
}

impl Pad {
    pub fn name(self) -> &'static str {
        match self {
            Pad::Up => "up",
            Pad::Down => "down",
            Pad::Left => "left",
            Pad::Right => "right",
            Pad::A => "a",
            Pad::B => "b",
            Pad::X => "x",
            Pad::Y => "y",
            Pad::Lb => "lb",
            Pad::Rb => "rb",
            Pad::Start => "start",
            Pad::Select => "select",
        }
    }

    pub fn parse(s: &str) -> Option<Pad> {
        Some(match s.to_ascii_lowercase().as_str() {
            "up" => Pad::Up,
            "down" => Pad::Down,
            "left" => Pad::Left,
            "right" => Pad::Right,
            "a" => Pad::A,
            "b" => Pad::B,
            "x" => Pad::X,
            "y" => Pad::Y,
            "lb" => Pad::Lb,
            "rb" => Pad::Rb,
            "start" => Pad::Start,
            "select" => Pad::Select,
            _ => return None,
        })
    }

    fn is_direction(self) -> bool {
        matches!(self, Pad::Up | Pad::Down | Pad::Left | Pad::Right)
    }
}

/// Window within which a key press and a controller press are taken to be
/// the same physical press (Steam desktop layout sends both).
const SAME_PRESS: Duration = Duration::from_millis(150);

/// Remembers when the last controller action and the last key action were
/// handled, to drop duplicates.
#[derive(Default)]
pub struct InputClock {
    pad: Option<Instant>,
    key: Option<Instant>,
}

pub type SharedClock = Arc<Mutex<InputClock>>;

impl InputClock {
    /// A keyboard navigation key arrived; may it act?
    pub fn key_allowed(clock: &SharedClock) -> bool {
        let mut c = clock.lock().unwrap();
        let now = Instant::now();
        if c.pad.is_some_and(|t| now.duration_since(t) < SAME_PRESS) {
            return false;
        }
        c.key = Some(now);
        true
    }

    fn pad_allowed(clock: &SharedClock) -> bool {
        let mut c = clock.lock().unwrap();
        let now = Instant::now();
        if c.key.is_some_and(|t| now.duration_since(t) < SAME_PRESS) {
            return false;
        }
        c.pad = Some(now);
        true
    }
}

const REPEAT_DELAY: Duration = Duration::from_millis(380);
const REPEAT_EVERY: Duration = Duration::from_millis(85);
const STICK_ON: f32 = 0.6;
const STICK_OFF: f32 = 0.35;

/// Start reading controllers on a background thread. `send` is called (on
/// that thread) for every action, including auto-repeat of held directions.
/// Returns immediately; if no controller backend is available, does nothing.
pub fn spawn(clock: SharedClock, send: impl Fn(Pad) + Send + 'static) {
    std::thread::Builder::new()
        .name("gamepad".into())
        .spawn(move || {
            let mut gilrs = match Gilrs::new() {
                Ok(g) => g,
                Err(gilrs::Error::NotImplemented(g)) => g,
                Err(e) => {
                    eprintln!("controller support unavailable: {e}");
                    return;
                }
            };
            let emit = |p: Pad| {
                if InputClock::pad_allowed(&clock) {
                    send(p);
                }
            };
            // Held direction: (direction, pressed at, last repeat).
            let mut held: Option<(Pad, Instant, Instant)> = None;
            // Stick state per axis, so it acts like a D-pad.
            let mut stick_x: Option<Pad> = None;
            let mut stick_y: Option<Pad> = None;

            loop {
                while let Some(ev) = gilrs.next_event_blocking(Some(Duration::from_millis(20))) {
                    match ev.event {
                        EventType::ButtonPressed(b, _) => {
                            if let Some(p) = map_button(b) {
                                emit(p);
                                if p.is_direction() {
                                    let now = Instant::now();
                                    held = Some((p, now, now));
                                }
                            }
                        }
                        EventType::ButtonReleased(b, _) => {
                            if let Some(p) = map_button(b)
                                && held.is_some_and(|(h, _, _)| h == p)
                            {
                                held = None;
                            }
                        }
                        EventType::AxisChanged(axis, v, _) => {
                            let (slot, neg, pos) = match axis {
                                Axis::LeftStickX | Axis::DPadX => {
                                    (&mut stick_x, Pad::Left, Pad::Right)
                                }
                                // gilrs: up is positive.
                                Axis::LeftStickY | Axis::DPadY => {
                                    (&mut stick_y, Pad::Down, Pad::Up)
                                }
                                _ => continue,
                            };
                            let now_dir = if v >= STICK_ON {
                                Some(pos)
                            } else if v <= -STICK_ON {
                                Some(neg)
                            } else if v.abs() < STICK_OFF {
                                None
                            } else {
                                *slot // in the dead band between: keep state
                            };
                            if now_dir != *slot {
                                if let Some(old) = *slot
                                    && held.is_some_and(|(h, _, _)| h == old)
                                {
                                    held = None;
                                }
                                if let Some(d) = now_dir {
                                    emit(d);
                                    let now = Instant::now();
                                    held = Some((d, now, now));
                                }
                                *slot = now_dir;
                            }
                        }
                        EventType::Disconnected => held = None,
                        _ => {}
                    }
                }
                // Auto-repeat.
                if let Some((p, since, last)) = held {
                    let now = Instant::now();
                    if now.duration_since(since) >= REPEAT_DELAY
                        && now.duration_since(last) >= REPEAT_EVERY
                    {
                        emit(p);
                        held = Some((p, since, now));
                    }
                }
            }
        })
        .expect("spawn gamepad thread");
}

/// Names of the controllers gilrs can see right now (`--list-gamepads`).
pub fn list() -> Result<Vec<String>, String> {
    let gilrs = match Gilrs::new() {
        Ok(g) => g,
        Err(gilrs::Error::NotImplemented(g)) => g,
        Err(e) => return Err(e.to_string()),
    };
    Ok(gilrs
        .gamepads()
        .map(|(_, g)| {
            format!(
                "{} ({})",
                g.name(),
                if g.is_connected() {
                    "connected"
                } else {
                    "disconnected"
                }
            )
        })
        .collect())
}

fn map_button(b: Button) -> Option<Pad> {
    Some(match b {
        Button::DPadUp => Pad::Up,
        Button::DPadDown => Pad::Down,
        Button::DPadLeft => Pad::Left,
        Button::DPadRight => Pad::Right,
        Button::South => Pad::A,
        Button::East => Pad::B,
        Button::West => Pad::X,
        Button::North => Pad::Y,
        Button::LeftTrigger => Pad::Lb,
        Button::RightTrigger => Pad::Rb,
        Button::Start => Pad::Start,
        Button::Select => Pad::Select,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_roundtrip() {
        for p in [
            Pad::Up,
            Pad::Down,
            Pad::Left,
            Pad::Right,
            Pad::A,
            Pad::B,
            Pad::X,
            Pad::Y,
            Pad::Lb,
            Pad::Rb,
            Pad::Start,
            Pad::Select,
        ] {
            assert_eq!(Pad::parse(p.name()), Some(p));
        }
    }

    #[test]
    fn duplicate_presses_are_dropped() {
        let c: SharedClock = Default::default();
        assert!(InputClock::pad_allowed(&c));
        assert!(
            !InputClock::key_allowed(&c),
            "key right after pad = same press"
        );
        std::thread::sleep(SAME_PRESS + Duration::from_millis(20));
        assert!(InputClock::key_allowed(&c));
        assert!(
            !InputClock::pad_allowed(&c),
            "pad right after key = same press"
        );
    }
}
