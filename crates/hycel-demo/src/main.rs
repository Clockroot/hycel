use hycel_core::{
    CanonicalState, CanonicalWriter, DEFAULT_TICKS_PER_SECOND, FixedClock,
    InputFrame as TickInputFrame, Replay, Schedule, ScheduleError, SystemId,
};

// Small, headless proof that Hycel's chosen 1.0 target—a deterministic 2D
// platformer—is feasible with the current Rust kernel. This is demo-only
// prototype behavior, not a stable physics/gameplay API.
const RUN_PER_TICK_MILLI_UNITS: i64 = 80;
const JUMP_SPEED_MILLI_UNITS_PER_TICK: i64 = -60;
const GRAVITY_MILLI_UNITS_PER_TICK: i64 = 2;
const HORIZONTAL_ACTION: u16 = 0;
const JUMP_ACTION: u16 = 1;
const DEMO_SEED: u64 = 0x004d_5943_454c;
const DEMO_STREAM: u64 = 0;

#[derive(Debug, Clone, Copy, Default)]
struct InputFrame {
    horizontal: i16,
    jump_pressed: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PlatformerState {
    /// Horizontal position in thousandths of a world unit.
    x_milli_units: i64,
    /// Vertical position of the player's feet; ground is zero, positive is down.
    y_milli_units: i64,
    /// Vertical velocity in thousandths of a world unit per simulation tick.
    vertical_velocity: i64,
    grounded: bool,
}

impl CanonicalState for PlatformerState {
    fn write_canonical(&self, writer: &mut CanonicalWriter) {
        writer.write_i64(self.x_milli_units);
        writer.write_i64(self.y_milli_units);
        writer.write_i64(self.vertical_velocity);
        writer.write_bool(self.grounded);
    }
}

impl Default for PlatformerState {
    fn default() -> Self {
        Self {
            x_milli_units: 0,
            y_milli_units: 0,
            vertical_velocity: 0,
            grounded: true,
        }
    }
}

#[derive(Debug, Clone, Copy, Default)]
struct PlatformerPrototype {
    player: PlatformerState,
}

impl CanonicalState for PlatformerPrototype {
    fn write_canonical(&self, writer: &mut CanonicalWriter) {
        self.player.write_canonical(writer);
    }
}

impl PlatformerPrototype {
    fn step(&mut self, input: InputFrame) {
        let horizontal = i64::from(input.horizontal.clamp(-1, 1));
        self.player.x_milli_units = self
            .player
            .x_milli_units
            .saturating_add(horizontal * RUN_PER_TICK_MILLI_UNITS);

        if input.jump_pressed && self.player.grounded {
            self.player.vertical_velocity = JUMP_SPEED_MILLI_UNITS_PER_TICK;
            self.player.grounded = false;
        }

        if !self.player.grounded {
            self.player.vertical_velocity = self
                .player
                .vertical_velocity
                .saturating_add(GRAVITY_MILLI_UNITS_PER_TICK);
            self.player.y_milli_units = self
                .player
                .y_milli_units
                .saturating_add(self.player.vertical_velocity);

            // Prototype has one flat ground plane; the full engine will have
            // general collision shapes and a separately chosen physics layer.
            if self.player.y_milli_units >= 0 {
                self.player.y_milli_units = 0;
                self.player.vertical_velocity = 0;
                self.player.grounded = true;
            }
        }
    }
}

fn platformer_schedule() -> Result<Schedule<PlatformerPrototype, ()>, ScheduleError> {
    let mut schedule = Schedule::<PlatformerPrototype, ()>::new(DEMO_SEED, DEMO_STREAM);
    schedule.add_system(0, SystemId::new(1), |game, context| {
        game.step(InputFrame {
            horizontal: context.input().axis(HORIZONTAL_ACTION),
            jump_pressed: context.input().button(JUMP_ACTION),
        });
        Ok(())
    })?;
    Ok(schedule)
}

fn record_demo() -> Result<(PlatformerPrototype, i64, Replay), Box<dyn std::error::Error>> {
    let mut clock = FixedClock::new(DEFAULT_TICKS_PER_SECOND)?;
    let mut game = PlatformerPrototype::default();
    let mut schedule = platformer_schedule()?;
    let mut recorded_inputs = Vec::new();
    let mut highest_jump_milli_units = 0_i64;

    // Run a short, repeatable headless platformer session: move right and jump
    // once. This provides a real Rust executable alongside the scope documents.
    for frame in 0_u32..90 {
        let due_steps = clock.advance_ns(16_666_667)?;
        if due_steps != 1 {
            return Err(format!("expected one tick for demo frame, got {due_steps}").into());
        }

        let mut input = TickInputFrame::new(u64::from(frame));
        input.set_axis(HORIZONTAL_ACTION, 1);
        input.set_button(JUMP_ACTION, frame == 30);
        schedule.run_tick(&mut game, &input)?;
        recorded_inputs.push(input);
        highest_jump_milli_units = highest_jump_milli_units.min(game.player.y_milli_units);
    }

    if schedule.next_tick() != clock.tick() {
        return Err("clock and simulation schedule ticks diverged".into());
    }
    let replay = Replay::new(
        DEFAULT_TICKS_PER_SECOND,
        DEMO_SEED,
        DEMO_STREAM,
        recorded_inputs,
    )?;
    Ok((game, highest_jump_milli_units, replay))
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let (game, highest_jump_milli_units, replay) = record_demo()?;
    let replay = Replay::from_json(&replay.to_json()?)?;
    let mut playback_game = PlatformerPrototype::default();
    replay.playback(
        &mut platformer_schedule()?,
        &mut playback_game,
        DEFAULT_TICKS_PER_SECOND,
    )?;
    let state_hash = game.state_hash();
    if state_hash != playback_game.state_hash() {
        return Err("headless replay state hash did not match recorded simulation".into());
    }

    println!(
        "Hycel platformer prototype: ticks={}, x={} milli-units, highest_jump={} milli-units, grounded={}, state_hash={}",
        replay.frames().len(),
        game.player.x_milli_units,
        highest_jump_milli_units,
        game.player.grounded,
        state_hash
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        DEFAULT_TICKS_PER_SECOND, InputFrame, PlatformerPrototype, platformer_schedule, record_demo,
    };
    use hycel_core::{CanonicalState, Replay};

    #[test]
    fn player_moves_horizontally_by_fixed_tick_amount() {
        let mut game = PlatformerPrototype::default();
        game.step(InputFrame {
            horizontal: 1,
            jump_pressed: false,
        });
        assert_eq!(game.player.x_milli_units, 80);
        assert!(game.player.grounded);
    }

    #[test]
    fn jump_rises_and_lands_on_ground() {
        let mut game = PlatformerPrototype::default();
        game.step(InputFrame {
            horizontal: 0,
            jump_pressed: true,
        });
        assert!(game.player.y_milli_units < 0);
        assert!(!game.player.grounded);

        let mut reached_apex = false;
        for _ in 0..120 {
            game.step(InputFrame::default());
            reached_apex |= game.player.vertical_velocity >= 0;
            if game.player.grounded {
                break;
            }
        }

        assert!(reached_apex);
        assert!(game.player.grounded);
        assert_eq!(game.player.y_milli_units, 0);
        assert_eq!(game.player.vertical_velocity, 0);
    }

    #[test]
    fn airborne_jump_press_does_not_double_jump() {
        let mut game = PlatformerPrototype::default();
        game.step(InputFrame {
            horizontal: 0,
            jump_pressed: true,
        });
        let velocity_after_jump = game.player.vertical_velocity;
        game.step(InputFrame {
            horizontal: 0,
            jump_pressed: true,
        });
        assert!(game.player.vertical_velocity > velocity_after_jump);
        assert!(!game.player.grounded);
    }

    #[test]
    fn recorded_json_replay_reproduces_the_canonical_platformer_hash() {
        let (recorded, _, replay) = record_demo().unwrap();
        let json = replay.to_json().unwrap();
        let replay = Replay::from_json(&json).unwrap();
        let mut playback = PlatformerPrototype::default();
        replay
            .playback(
                &mut platformer_schedule().unwrap(),
                &mut playback,
                DEFAULT_TICKS_PER_SECOND,
            )
            .unwrap();
        assert_eq!(recorded.state_hash(), playback.state_hash());
    }

    #[test]
    fn identical_inputs_produce_identical_simulation_state() {
        let mut a = PlatformerPrototype::default();
        let mut b = PlatformerPrototype::default();
        for frame in 0..90 {
            let input = InputFrame {
                horizontal: i16::from(frame < 60),
                jump_pressed: frame == 20,
            };
            a.step(input);
            b.step(input);
        }
        assert_eq!(a.player, b.player);
    }
}
