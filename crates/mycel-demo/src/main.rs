use mycel_core::FixedClock;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut clock = FixedClock::new(60)?;
    let steps = clock.advance_ns(1_000_000_000)?;
    println!(
        "Mycel headless demo: {steps} deterministic steps; tick={}",
        clock.tick()
    );
    Ok(())
}
