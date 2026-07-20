//! The pidgin CLI. A thin shell that mirrors pi's `bin/pi` entrypoint: parse
//! argv and drive the coding-agent startup flow. All logic lives in [`cli`].

mod cli;

fn main() -> ! {
    cli::main()
}
