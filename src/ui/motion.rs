//! Shared pulse clock for the repeating loaders.
//!
//! Ported from Zeron's motion kit (<https://github.com/zeronsh/comet>, MIT).
//! A repeating `with_animation` element requests a redraw every display frame
//! for as long as it is mounted — one working row pinned the whole window at
//! 120 Hz on a ProMotion panel. Loaders instead read their phase from one
//! shared clock: it ticks at ~30 fps, notifies only views that painted a
//! loader recently, and parks itself once the last lease lapses, so a window
//! with no loader mounted schedules nothing at all. Every loader shares one
//! epoch, keeping multi-instance loaders phase-locked.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use gpui::{
    AnyElement, App, EntityId, Global, IntoElement, RenderOnce, Svg, Transformation, Window,
    percentage,
};

/// Repeat-tick interval (~30 fps): visually equivalent for these chunky
/// pulses and spins at a quarter of a ProMotion display's redraws.
const PULSE_TICK: Duration = Duration::from_millis(33);

/// How long a view stays on the tick list after it last painted a loader. One
/// lease outlives a few missed frames; an unmounted loader stops renewing and
/// its view drops off, letting the clock park.
const PULSE_LEASE: Duration = Duration::from_millis(300);

/// The rotating `loader-circle` spinners' period.
const SPINNER_PERIOD: Duration = Duration::from_millis(900);

struct PulseClock {
    epoch: Instant,
    leases: HashMap<EntityId, Instant>,
    running: bool,
}

impl Global for PulseClock {}

impl Default for PulseClock {
    fn default() -> Self {
        Self {
            epoch: Instant::now(),
            leases: HashMap::new(),
            running: false,
        }
    }
}

/// Keep `view` re-rendering at [`PULSE_TICK`] until the lease lapses. A caller
/// that stops leasing stops being notified, and the clock parks once no
/// leases remain — quiescence needs no unsubscribe step.
pub fn pulse_lease(view: EntityId, cx: &mut App) {
    let clock = cx.default_global::<PulseClock>();
    clock.leases.insert(view, Instant::now() + PULSE_LEASE);
    if clock.running {
        return;
    }
    clock.running = true;
    cx.spawn(async move |cx| {
        loop {
            cx.background_executor().timer(PULSE_TICK).await;
            let parked = cx.update(|cx| {
                let clock = cx.default_global::<PulseClock>();
                let now = Instant::now();
                clock.leases.retain(|_, until| *until > now);
                if clock.leases.is_empty() {
                    clock.running = false;
                    return true;
                }
                let leased = clock.leases.keys().copied().collect::<Vec<_>>();
                for view in leased {
                    cx.notify(view);
                }
                false
            });
            if parked {
                break;
            }
        }
    })
    .detach();
}

/// Phase `[0,1)` of a repeating cycle of `period`, plus a lease keeping `view`
/// re-rendering while its loader stays mounted. Under reduce-motion this is a
/// constant 0 — the cycle's first frame, matching what a repeating
/// `with_animation` held — and nothing is scheduled.
fn pulse_phase(period: Duration, view: EntityId, cx: &mut App) -> f32 {
    if cx.reduce_motion() {
        return 0.0;
    }
    let clock = cx.default_global::<PulseClock>();
    let phase = (clock.epoch.elapsed().as_secs_f32() / period.as_secs_f32()).fract();
    pulse_lease(view, cx);
    phase
}

/// A loader element styled from the shared clock's phase. Resolving the phase
/// is deferred to render, where the owning view is known, so call sites need
/// neither a `Window` nor an `EntityId` in scope.
pub fn pulse(period: Duration, render: impl FnOnce(f32) -> AnyElement + 'static) -> Pulse {
    Pulse {
        period,
        render: Box::new(render),
    }
}

/// A rotating loader icon riding the shared clock.
pub fn spin(icon: Svg) -> AnyElement {
    pulse(SPINNER_PERIOD, move |phase| {
        icon.with_transformation(Transformation::rotate(percentage(phase)))
            .into_any_element()
    })
    .into_any_element()
}

#[derive(IntoElement)]
pub struct Pulse {
    period: Duration,
    render: Box<dyn FnOnce(f32) -> AnyElement>,
}

impl RenderOnce for Pulse {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let phase = pulse_phase(self.period, window.current_view(), cx);
        (self.render)(phase)
    }
}
