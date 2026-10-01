use std::process;

fn main() {
    let output = hycel_cli::execute(std::env::args_os().skip(1));
    print!("{}", output.stdout);
    eprint!("{}", output.stderr);
    process::exit(output.exit_code);
}
