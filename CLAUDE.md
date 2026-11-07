# Building

- While working, build and test with debug builds only: `cargo build`, `cargo test`, and run
  `target/debug/spot`. An incremental debug build takes about a second.
- Run `macos/bundle.sh` (a release build, then installs Spot.app) only once, at the end, when the
  change is finished and the user wants it installed. Don't run it, or `cargo build --release`,
  to check each step.
- Never build `--profile dist` locally. It's the fully optimized build the release workflow
  ships, and a single rebuild takes several minutes.
- Don't hide cargo's output with `grep`. Pipe it through `tail` instead, so a
  "Blocking waiting for file lock" message still shows.
