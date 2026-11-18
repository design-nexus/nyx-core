fn main() {
    std::process::exit(nyx_platform::cli::main(std::env::args().skip(1).collect()));
}
